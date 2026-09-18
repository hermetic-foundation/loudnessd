// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    error::Error,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use ::pipewire::{context::ContextRc, loop_::Timeout, main_loop::MainLoopRc};
use signal_hook::consts::signal::{SIGINT, SIGTERM};

use crate::{
    ipc::{ControlServer, write_response},
    normalization::{ControllerBank, SignalDomain, UserConfig, stream::StreamControl},
    pipewire::{
        filter::{ConnectedFilter, PortDirection, UnconnectedFilter},
        graph::{DiscoveredStream, GraphState, track_graph},
        routing::{
            RouteHealth, RoutePlanError,
            backend::PipewireRouteBackend,
            journal::{RecoveryJournal, path_for_socket},
            links::OwnedLinks,
            plan_route, route_health,
            transaction::{ActiveRoute, bypass, install, release},
        },
    },
    runtime_config::RuntimeConfig,
    status::{DaemonStatus, SkippedStreamStatus},
};

mod command;
mod discovery;
mod recovery;
mod reporting;

use command::Command;
use discovery::{ChannelReadiness, ChannelSettler, channel_readiness};

const CONTROL_INTERVAL: Duration = Duration::from_millis(100);
const TOPOLOGY_SETTLE_INTERVAL: Duration = Duration::from_millis(500);
const TOPOLOGY_SKIP_INTERVAL: Duration = Duration::from_secs(5);
const PIPEWIRE_SAMPLE_RATE: u32 = 48_000;

enum ManagedStream {
    Connecting {
        stream: DiscoveredStream,
        filter: ConnectedFilter,
        control: StreamControl,
    },
    Active {
        stream: DiscoveredStream,
        filter: ConnectedFilter,
        control: StreamControl,
        route: ActiveRoute<OwnedLinks>,
    },
}

struct Daemon {
    main_loop: MainLoopRc,
    core: ::pipewire::core::CoreRc,
    registry: ::pipewire::registry::RegistryRc,
    graph: Rc<std::cell::RefCell<GraphState>>,
    controllers: ControllerBank,
    managed: HashMap<u32, ManagedStream>,
    skipped: HashMap<u32, SkippedStreamStatus>,
    channel_settler: ChannelSettler,
    retained_direct_links: Vec<OwnedLinks>,
    recovery_journal: RecoveryJournal,
    config_path: PathBuf,
    runtime_config: RuntimeConfig,
    enabled: bool,
}

impl Daemon {
    fn tick(&mut self) {
        self.remove_disappeared_streams();
        self.prune_retained_direct_links();
        self.reconcile_routes();
        self.discover_streams();
        self.connect_pending_filters();
        self.update_controls();
    }

    fn process_requests(&mut self, server: &ControlServer) {
        loop {
            let (stream, request) = match server.accept_request() {
                Ok(Some(request)) => request,
                Ok(None) => break,
                Err(error) => {
                    eprintln!("loudnessd: control socket error: {error}");
                    break;
                }
            };
            let response = match self.handle_request(request.trim()) {
                Ok(response) => response,
                Err(error) => format!("error: {error}\n"),
            };
            if let Err(error) = write_response(stream, &response) {
                eprintln!("loudnessd: control response failed: {error}");
            }
        }
    }

    fn handle_request(&mut self, request: &str) -> Result<String, String> {
        match command::parse(request)? {
            Command::Status => Ok(self.status().to_text()),
            Command::StatusJson => {
                serde_json::to_string(&self.status()).map_err(|error| error.to_string())
            }
            Command::Reload => {
                let source = std::fs::read_to_string(&self.config_path)
                    .map_err(|error| error.to_string())?;
                let baseline = UserConfig::from_toml(&source).map_err(|error| error.to_string())?;
                let mut next = self.runtime_config.clone();
                next.replace_baseline(baseline);
                self.reconfigure(next)?;
                Ok("ok\n".to_owned())
            }
            Command::Enable => {
                self.enabled = true;
                self.skipped.clear();
                self.channel_settler.clear();
                Ok("ok\n".to_owned())
            }
            Command::Disable => {
                self.enabled = false;
                if self.bypass_all() {
                    Ok("ok\n".to_owned())
                } else {
                    self.enabled = true;
                    Err("could not bypass every stream; normalization remains enabled".to_owned())
                }
            }
            Command::Set {
                application_id,
                domain,
                enabled,
            } => {
                let mut next = self.runtime_config.clone();
                next.set(application_id, domain, enabled);
                self.reconfigure(next)?;
                Ok("ok\n".to_owned())
            }
            Command::Reset { application_id } => {
                let mut next = self.runtime_config.clone();
                next.reset(&application_id);
                self.reconfigure(next)?;
                Ok("ok\n".to_owned())
            }
            Command::Export => self
                .runtime_config
                .export_toml()
                .map_err(|error| error.to_string()),
        }
    }

    fn status(&self) -> DaemonStatus {
        reporting::snapshot(
            self.enabled,
            &self.controllers,
            &self.managed,
            &self.skipped,
            &self.graph.borrow(),
        )
    }

    fn reconfigure(&mut self, next: RuntimeConfig) -> Result<(), String> {
        let effective = next.effective();
        effective
            .controller_configs()
            .map_err(|error| format!("invalid controller configuration: {error}"))?;
        if !self.bypass_all() {
            return Err("could not bypass every stream; configuration was not applied".to_owned());
        }
        self.controllers
            .apply_user_config(effective)
            .expect("controller configuration was validated before bypass");
        self.runtime_config = next;
        self.skipped.clear();
        self.channel_settler.clear();
        Ok(())
    }

    fn remove_disappeared_streams(&mut self) {
        let previous_count = self.managed.len();
        let present: HashSet<_> = self
            .graph
            .borrow()
            .streams()
            .map(|stream| stream.node_id)
            .collect();
        self.managed.retain(|node_id, _| present.contains(node_id));
        self.skipped.retain(|node_id, _| present.contains(node_id));
        self.channel_settler
            .retain(|node_id| present.contains(&node_id));
        if self.managed.len() != previous_count {
            self.sync_recovery_journal(None);
        }
    }

    fn reconcile_routes(&mut self) {
        let unhealthy: Vec<_> = {
            let graph = self.graph.borrow();
            self.managed
                .iter()
                .filter_map(|(node_id, managed)| {
                    let ManagedStream::Active { route, .. } = managed else {
                        return None;
                    };
                    let health = route_health(route.plan(), &graph);
                    (health != RouteHealth::Healthy).then_some((*node_id, health))
                })
                .collect()
        };

        for (node_id, health) in unhealthy {
            let Some(ManagedStream::Active {
                stream,
                mut filter,
                control,
                route,
            }) = self.managed.remove(&node_id)
            else {
                continue;
            };
            self.skipped.remove(&node_id);
            if matches!(health, RouteHealth::Superseded | RouteHealth::EndpointsGone) {
                let result = {
                    let mut backend = PipewireRouteBackend::new(
                        &self.main_loop,
                        &self.core,
                        &self.registry,
                        Rc::clone(&self.graph),
                        &mut filter,
                        &mut self.retained_direct_links,
                    );
                    release(&mut backend, route)
                };
                let reason = match health {
                    RouteHealth::Superseded => "route changed",
                    RouteHealth::EndpointsGone => "route endpoint disappeared",
                    RouteHealth::Healthy | RouteHealth::Broken => unreachable!(),
                };
                if let Err(error) = result {
                    eprintln!(
                        "loudnessd: stream {node_id} {reason}; filter deactivation failed: {}",
                        error.error
                    );
                } else {
                    eprintln!("loudnessd: stream {node_id} {reason}; reconnecting");
                }
                self.sync_recovery_journal(None);
                self.managed.insert(
                    node_id,
                    ManagedStream::Connecting {
                        stream,
                        filter,
                        control,
                    },
                );
                continue;
            }

            let result = {
                let mut backend = PipewireRouteBackend::new(
                    &self.main_loop,
                    &self.core,
                    &self.registry,
                    Rc::clone(&self.graph),
                    &mut filter,
                    &mut self.retained_direct_links,
                );
                bypass(&mut backend, route)
            };
            match result {
                Ok(bypassed) => {
                    self.retained_direct_links.push(bypassed.direct_links);
                    eprintln!("loudnessd: recovered broken route for stream {node_id}");
                }
                Err(error) => {
                    eprintln!(
                        "loudnessd: broken route recovery failed for stream {} at {:?}: {}",
                        stream.node_id, error.transition.operation, error.transition.error
                    );
                    self.managed.insert(
                        node_id,
                        ManagedStream::Active {
                            stream,
                            filter,
                            control,
                            route: error.route,
                        },
                    );
                }
            }
            self.sync_recovery_journal(None);
        }
    }

    fn discover_streams(&mut self) {
        if !self.enabled {
            return;
        }
        let streams: Vec<_> = self.graph.borrow().streams().cloned().collect();
        for stream in streams {
            if self.managed.contains_key(&stream.node_id)
                || self.skipped.contains_key(&stream.node_id)
            {
                continue;
            }
            let application_id = application_id(&stream);
            if !self
                .controllers
                .policy_for(&application_id)
                .enables(stream.domain)
            {
                if !self.graph.borrow().has_node_info(stream.node_id) {
                    continue;
                }
                self.skip_stream(&stream, "disabled by policy");
                continue;
            }
            let readiness = channel_readiness(&stream, &self.graph.borrow());
            let settle_for = if matches!(readiness, ChannelReadiness::Skipped(_)) {
                TOPOLOGY_SKIP_INTERVAL
            } else {
                TOPOLOGY_SETTLE_INTERVAL
            };
            let readiness =
                self.channel_settler
                    .observe(stream.node_id, readiness, Instant::now(), settle_for);
            let channels = match readiness {
                ChannelReadiness::Pending => continue,
                ChannelReadiness::Ready(channels) => channels,
                ChannelReadiness::Skipped(reason) => {
                    self.skip_stream(&stream, reason);
                    continue;
                }
            };
            match self.create_filter(&stream, &channels) {
                Ok(filter) => {
                    let control = StreamControl::new(
                        stream.domain,
                        application_id,
                        stream.node_id.to_string(),
                    );
                    self.managed.insert(
                        stream.node_id,
                        ManagedStream::Connecting {
                            stream,
                            filter,
                            control,
                        },
                    );
                }
                Err(error) => {
                    eprintln!(
                        "loudnessd: stream {} filter creation failed: {error}",
                        stream.node_id
                    );
                    self.skip_stream(&stream, format!("filter creation failed: {error}"));
                }
            }
        }
    }

    fn create_filter(
        &self,
        stream: &DiscoveredStream,
        channels: &[String],
    ) -> Result<ConnectedFilter, Box<dyn Error>> {
        let domain = match stream.domain {
            SignalDomain::Playback => "playback",
            SignalDomain::Capture => "capture",
        };
        let mut filter = UnconnectedFilter::new_on_core(
            &self.core,
            &format!("loudnessd-{domain}-{}", stream.node_id),
        )?;
        for channel in channels {
            filter.add_mono_port(PortDirection::Input, format!("input_{channel}"), channel)?;
            filter.add_mono_port(PortDirection::Output, format!("output_{channel}"), channel)?;
        }
        filter.enable_meter(PIPEWIRE_SAMPLE_RATE)?;
        Ok(filter.connect_inactive()?)
    }

    fn connect_pending_filters(&mut self) {
        let pending: Vec<_> = self
            .managed
            .iter()
            .filter_map(|(node_id, managed)| {
                matches!(managed, ManagedStream::Connecting { .. }).then_some(*node_id)
            })
            .collect();
        for node_id in pending {
            let Some(ManagedStream::Connecting {
                stream,
                mut filter,
                control,
            }) = self.managed.remove(&node_id)
            else {
                continue;
            };
            let Some(filter_node_id) = filter.node_id() else {
                self.managed.insert(
                    node_id,
                    ManagedStream::Connecting {
                        stream,
                        filter,
                        control,
                    },
                );
                continue;
            };
            let (ports, links) = {
                let graph = self.graph.borrow();
                (
                    graph.ports().cloned().collect::<Vec<_>>(),
                    graph.links().cloned().collect::<Vec<_>>(),
                )
            };
            let plan = match plan_route(stream.domain, node_id, filter_node_id, &ports, &links) {
                Ok(plan) => plan,
                Err(
                    RoutePlanError::MissingFilterPort { .. }
                    | RoutePlanError::MissingRoute { .. }
                    | RoutePlanError::AmbiguousRoute { .. },
                ) => {
                    self.managed.insert(
                        node_id,
                        ManagedStream::Connecting {
                            stream,
                            filter,
                            control,
                        },
                    );
                    continue;
                }
                Err(error) => {
                    eprintln!("loudnessd: stream {node_id} cannot be routed: {error}");
                    self.skip_stream(&stream, format!("route planning failed: {error}"));
                    continue;
                }
            };
            if !self.sync_recovery_journal(Some(&plan)) {
                self.skip_stream(&stream, "recovery journal update failed");
                continue;
            }
            let mut backend = PipewireRouteBackend::new(
                &self.main_loop,
                &self.core,
                &self.registry,
                Rc::clone(&self.graph),
                &mut filter,
                &mut self.retained_direct_links,
            );
            match install(&mut backend, plan) {
                Ok(route) => {
                    eprintln!(
                        "loudnessd: normalizing {} stream {node_id} ({})",
                        domain_name(stream.domain),
                        application_id(&stream)
                    );
                    self.managed.insert(
                        node_id,
                        ManagedStream::Active {
                            stream,
                            filter,
                            control,
                            route,
                        },
                    );
                }
                Err(error) => {
                    eprintln!(
                        "loudnessd: stream {node_id} route installation failed at {:?}: {}",
                        error.operation, error.error
                    );
                    self.skip_stream(
                        &stream,
                        format!(
                            "route installation failed at {:?}: {}",
                            error.operation, error.error
                        ),
                    );
                    self.sync_recovery_journal(None);
                }
            }
        }
    }

    fn update_controls(&mut self) {
        for managed in self.managed.values_mut() {
            let ManagedStream::Active {
                filter, control, ..
            } = managed
            else {
                continue;
            };
            if let Err(error) = control.update(
                filter,
                &mut self.controllers,
                CONTROL_INTERVAL.as_secs_f32(),
            ) {
                eprintln!("loudnessd: controller update failed: {error}");
            }
        }
    }

    fn skip_stream(&mut self, stream: &DiscoveredStream, reason: impl Into<String>) {
        self.skipped.insert(
            stream.node_id,
            SkippedStreamStatus {
                node_id: stream.node_id,
                domain: domain_name(stream.domain).to_owned(),
                application: application_id(stream),
                reason: reason.into(),
            },
        );
    }

    fn bypass_all(&mut self) -> bool {
        let managed = std::mem::take(&mut self.managed);
        let mut succeeded = true;
        for (node_id, stream) in managed {
            let ManagedStream::Active {
                stream,
                mut filter,
                control,
                route,
            } = stream
            else {
                continue;
            };
            let result = {
                let mut backend = PipewireRouteBackend::new(
                    &self.main_loop,
                    &self.core,
                    &self.registry,
                    Rc::clone(&self.graph),
                    &mut filter,
                    &mut self.retained_direct_links,
                );
                bypass(&mut backend, route)
            };
            match result {
                Ok(bypassed) => self.retained_direct_links.push(bypassed.direct_links),
                Err(error) => {
                    succeeded = false;
                    eprintln!(
                        "loudnessd: failed to bypass stream {} at {:?}: {}",
                        stream.node_id, error.transition.operation, error.transition.error
                    );
                    self.managed.insert(
                        node_id,
                        ManagedStream::Active {
                            stream,
                            filter,
                            control,
                            route: error.route,
                        },
                    );
                }
            }
        }
        self.sync_recovery_journal(None) && succeeded
    }
}

fn application_id(stream: &DiscoveredStream) -> String {
    stream
        .application_id
        .as_ref()
        .or(stream.process_binary.as_ref())
        .or(stream.application_name.as_ref())
        .cloned()
        .unwrap_or_else(|| format!("node-{}", stream.node_id))
}

fn domain_name(domain: SignalDomain) -> &'static str {
    match domain {
        SignalDomain::Playback => "playback",
        SignalDomain::Capture => "capture",
    }
}

pub fn run(
    controllers: ControllerBank,
    config_path: PathBuf,
    baseline: UserConfig,
    socket_path: PathBuf,
) -> Result<(), Box<dyn Error>> {
    let main_loop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let server_cookie = Rc::new(Cell::new(None));
    let connection_error = Rc::new(RefCell::new(None));
    let cookie_callback = Rc::clone(&server_cookie);
    let error_callback = Rc::clone(&connection_error);
    let _core_listener = core
        .add_listener_local()
        .info(move |info| cookie_callback.set(Some(info.cookie())))
        .error(move |id, sequence, result, message| {
            if is_fatal_core_error(id, result) {
                *error_callback.borrow_mut() = Some(format!(
                    "{message} (object {id}, sequence {sequence}, result {result})"
                ));
            }
        })
        .register();
    let cookie_deadline = Instant::now() + Duration::from_secs(2);
    while server_cookie.get().is_none()
        && connection_error.borrow().is_none()
        && Instant::now() < cookie_deadline
    {
        main_loop
            .loop_()
            .iterate(Timeout::Finite(Duration::from_millis(20)));
    }
    if let Some(error) = connection_error.borrow_mut().take() {
        return Err(format!("PipeWire connection failed: {error}").into());
    }
    let server_cookie = server_cookie
        .get()
        .ok_or("PipeWire did not report its server identity")?;
    let registry = core.get_registry_rc()?;
    let (graph, _registry_listener) = track_graph(&registry);
    let recovery_journal = RecoveryJournal::new(path_for_socket(&socket_path), server_cookie);
    let control_server = ControlServer::bind(socket_path)?;
    let terminated = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGINT, Arc::clone(&terminated))?;
    signal_hook::flag::register(SIGTERM, Arc::clone(&terminated))?;
    let mut daemon = Daemon {
        main_loop: main_loop.clone(),
        core,
        registry,
        graph,
        controllers,
        managed: HashMap::new(),
        skipped: HashMap::new(),
        channel_settler: ChannelSettler::default(),
        retained_direct_links: Vec::new(),
        recovery_journal,
        config_path,
        runtime_config: RuntimeConfig::new(baseline),
        enabled: true,
    };

    daemon.recover_prior_routes();

    eprintln!("loudnessd: monitoring and normalizing PipeWire application streams");
    while !terminated.load(Ordering::Relaxed) && connection_error.borrow().is_none() {
        main_loop.loop_().iterate(Timeout::Finite(CONTROL_INTERVAL));
        daemon.tick();
        daemon.process_requests(&control_server);
    }
    if let Some(error) = connection_error.borrow_mut().take() {
        // A broken PipeWire core can leave client-side proxies and filters in
        // a partially destroyed state. Calling their normal destructors may
        // re-enter already-freed native objects. The process is about to exit,
        // so retain the disconnected object graph and let the OS reclaim it.
        std::mem::forget(daemon);
        std::mem::forget(_registry_listener);
        std::mem::forget(_core_listener);
        std::mem::forget(context);
        std::mem::forget(main_loop);
        return Err(format!("PipeWire connection failed: {error}").into());
    }
    if daemon.bypass_all() {
        Ok(())
    } else {
        Err("could not restore every direct route during shutdown".into())
    }
}

fn is_fatal_core_error(id: u32, result: i32) -> bool {
    id == ::pipewire::core::PW_ID_CORE && result == -libc::EPIPE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_broken_core_connections_are_fatal() {
        assert!(is_fatal_core_error(
            ::pipewire::core::PW_ID_CORE,
            -libc::EPIPE
        ));
        assert!(!is_fatal_core_error(6, -libc::EEXIST));
        assert!(!is_fatal_core_error(
            ::pipewire::core::PW_ID_CORE,
            -libc::EEXIST
        ));
    }

    #[test]
    fn application_identity_prefers_stable_metadata() {
        let stream = DiscoveredStream {
            node_id: 42,
            domain: SignalDomain::Playback,
            application_id: Some("org.example.Player".to_owned()),
            application_name: Some("Player".to_owned()),
            process_binary: Some("player-bin".to_owned()),
            media_name: None,
        };

        assert_eq!(application_id(&stream), "org.example.Player");
    }

    #[test]
    fn application_identity_has_a_node_scoped_fallback() {
        let stream = DiscoveredStream {
            node_id: 42,
            domain: SignalDomain::Capture,
            application_id: None,
            application_name: None,
            process_binary: None,
            media_name: None,
        };

        assert_eq!(application_id(&stream), "node-42");
    }
}
