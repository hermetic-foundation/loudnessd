// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use pipewire::{context::ContextRc, loop_::Timeout, main_loop::MainLoopRc};
use signal_hook::consts::signal::{SIGINT, SIGTERM};

use crate::{
    ControllerBank, ControllerConfig, SignalDomain, UserConfig,
    ipc::{ControlServer, write_response},
    pipewire_backend::{
        DiscoveredStream, GraphState, PortDirection as GraphPortDirection, track_graph,
    },
    pipewire_filter::{ConnectedFilter, PortDirection, UnconnectedFilter},
    pipewire_links::OwnedLinks,
    pipewire_route_backend::PipewireRouteBackend,
    process_metrics,
    recovery::{RecoveryJournal, path_for_socket},
    route_transaction::{ActiveRoute, bypass, install},
    routing::{RouteHealth, RoutePlanError, plan_route, route_health},
    runtime_config::RuntimeConfig,
    status::{ControlStatus, DaemonStatus, RouteStatus, StreamLifecycle, StreamStatus},
    stream_control::StreamControl,
};

mod command;

use command::Command;

const CONTROL_INTERVAL: Duration = Duration::from_millis(100);
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
    core: pipewire::core::CoreRc,
    registry: pipewire::registry::RegistryRc,
    graph: Rc<std::cell::RefCell<GraphState>>,
    controllers: ControllerBank,
    managed: HashMap<u32, ManagedStream>,
    unsupported: HashSet<u32>,
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
                self.unsupported.clear();
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
        let graph = self.graph.borrow();
        let mut streams: Vec<_> = self
            .managed
            .iter()
            .map(|(node_id, managed)| {
                let (lifecycle, route, control) = match managed {
                    ManagedStream::Connecting { control, .. } => (
                        StreamLifecycle::Connecting,
                        RouteStatus::Connecting,
                        control,
                    ),
                    ManagedStream::Active { control, route, .. } => (
                        StreamLifecycle::Active,
                        route_status(route_health(route.plan(), &graph)),
                        control,
                    ),
                };
                let update = control.last_update();
                let meter = update.map(|update| update.meter);
                let controller_config = self.controllers.config(control.domain());
                let gain_db = update
                    .map(|update| update.decision.target_gain_db())
                    .unwrap_or(0.0);
                let gain_clamped = gain_is_limited(
                    controller_config,
                    gain_db,
                    meter.map(|meter| meter.source_loudness_lufs),
                );
                StreamStatus {
                    node_id: *node_id,
                    domain: domain_name(control.domain()).to_owned(),
                    application: control.application_id().to_owned(),
                    meter_sequence: meter.map(|meter| meter.sequence),
                    lifecycle,
                    route,
                    control: update
                        .map(|update| control_status(update.decision))
                        .unwrap_or(ControlStatus::Waiting),
                    target_lufs: controller_config.target_lufs,
                    source_lufs: meter.map(|meter| meter.source_loudness_lufs),
                    source_peak_dbtp: meter.and_then(|meter| meter.source_true_peak_dbtp),
                    output_lufs: meter.and_then(|meter| meter.output_loudness_lufs),
                    output_peak_dbtp: meter.and_then(|meter| meter.output_true_peak_dbtp),
                    gain_db,
                    gain_clamped,
                    limiter_db: meter.map(|meter| meter.limiter_reduction_db).unwrap_or(0.0),
                    limiter_max_db: meter
                        .map(|meter| meter.maximum_limiter_reduction_db)
                        .unwrap_or(0.0),
                }
            })
            .collect();
        streams.sort_by_key(|stream| stream.node_id);
        DaemonStatus {
            enabled: self.enabled,
            managed: streams.len(),
            active: streams
                .iter()
                .filter(|stream| stream.lifecycle == StreamLifecycle::Active)
                .count(),
            process: process_metrics::read(),
            streams,
        }
    }

    fn reconfigure(&mut self, next: RuntimeConfig) -> Result<(), String> {
        if !self.bypass_all() {
            return Err("could not bypass every stream; configuration was not applied".to_owned());
        }
        self.controllers.apply_user_config(next.effective());
        self.runtime_config = next;
        self.unsupported.clear();
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
        self.unsupported.retain(|node_id| present.contains(node_id));
        if self.managed.len() != previous_count {
            self.sync_recovery_journal(None);
        }
    }

    fn prune_retained_direct_links(&mut self) {
        let graph = self.graph.borrow();
        self.retained_direct_links
            .retain(|links| links.ids().any(|id| graph.contains_link_id(id)));
    }

    fn direct_specs(
        &self,
        extra: Option<&crate::routing::RoutePlan>,
    ) -> Vec<crate::routing::LinkSpec> {
        let mut specs: Vec<_> = self
            .managed
            .values()
            .filter_map(|managed| match managed {
                ManagedStream::Active { route, .. } => Some(route.plan()),
                ManagedStream::Connecting { .. } => None,
            })
            .chain(extra)
            .flat_map(|plan| plan.channels.iter().map(|channel| channel.original))
            .collect();
        specs.sort_by_key(|spec| (spec.output.port_id, spec.input.port_id));
        specs.dedup();
        specs
    }

    fn sync_recovery_journal(&self, extra: Option<&crate::routing::RoutePlan>) -> bool {
        match self.recovery_journal.replace(&self.direct_specs(extra)) {
            Ok(()) => true,
            Err(error) => {
                eprintln!("loudnessd: cannot update route recovery journal: {error}");
                false
            }
        }
    }

    fn clear_recovery_journal(&self) -> bool {
        match self.recovery_journal.replace(&[]) {
            Ok(()) => true,
            Err(error) => {
                eprintln!("loudnessd: cannot clear route recovery journal: {error}");
                false
            }
        }
    }

    fn recover_prior_routes(&mut self) {
        let specs = match self.recovery_journal.load() {
            Ok(specs) => specs,
            Err(error) => {
                eprintln!("loudnessd: cannot read route recovery journal: {error}");
                return;
            }
        };
        if specs.is_empty() {
            return;
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !recovery_endpoints_present(&self.graph.borrow(), &specs)
            && std::time::Instant::now() < deadline
        {
            self.main_loop
                .loop_()
                .iterate(Timeout::Finite(Duration::from_millis(20)));
        }
        if !recovery_endpoints_present(&self.graph.borrow(), &specs) {
            eprintln!("loudnessd: prior route endpoints disappeared; discarding recovery journal");
            self.clear_recovery_journal();
            return;
        }

        let missing: Vec<_> = specs
            .iter()
            .copied()
            .filter(|spec| {
                !self
                    .graph
                    .borrow()
                    .contains_link(spec.output.port_id, spec.input.port_id)
            })
            .collect();
        if missing.is_empty() {
            self.clear_recovery_journal();
            return;
        }
        match OwnedLinks::create_lingering(&self.core, &self.registry, &missing) {
            Ok(links) => {
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                while !missing.iter().all(|spec| {
                    self.graph
                        .borrow()
                        .contains_link(spec.output.port_id, spec.input.port_id)
                }) && std::time::Instant::now() < deadline
                {
                    self.main_loop
                        .loop_()
                        .iterate(Timeout::Finite(Duration::from_millis(20)));
                }
                if missing.iter().all(|spec| {
                    self.graph
                        .borrow()
                        .contains_link(spec.output.port_id, spec.input.port_id)
                }) {
                    eprintln!(
                        "loudnessd: restored {} direct links after an unclean exit",
                        missing.len()
                    );
                    drop(links);
                    self.clear_recovery_journal();
                } else {
                    eprintln!("loudnessd: timed out restoring direct links after an unclean exit");
                    links.destroy_globals(&self.registry);
                }
            }
            Err(error) => {
                eprintln!("loudnessd: cannot restore routes after an unclean exit: {error}")
            }
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
            self.unsupported.remove(&node_id);
            if health == RouteHealth::Superseded {
                eprintln!("loudnessd: stream {node_id} route changed; reconnecting");
                self.sync_recovery_journal(None);
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
                || self.unsupported.contains(&stream.node_id)
            {
                continue;
            }
            let application_id = application_id(&stream);
            if !self
                .controllers
                .policy_for(&application_id)
                .enables(stream.domain)
            {
                self.unsupported.insert(stream.node_id);
                continue;
            }
            let Some(channels) = self.ready_channels(&stream) else {
                continue;
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
                    self.unsupported.insert(stream.node_id);
                }
            }
        }
    }

    fn ready_channels(&self, stream: &DiscoveredStream) -> Option<Vec<String>> {
        let graph = self.graph.borrow();
        let direction = match stream.domain {
            SignalDomain::Playback => GraphPortDirection::Output,
            SignalDomain::Capture => GraphPortDirection::Input,
        };
        let ports: Vec<_> = graph
            .ports()
            .filter(|port| port.node_id == stream.node_id && port.direction == direction)
            .collect();
        if ports.is_empty() {
            return None;
        }
        let links: Vec<_> = graph.links().collect();
        let mut channels = HashSet::new();
        for port in ports {
            let channel = port.channel.as_ref()?.clone();
            if !channels.insert(channel) {
                return None;
            }
            let route_count = links
                .iter()
                .filter(|link| match stream.domain {
                    SignalDomain::Playback => link.output_port_id == port.port_id,
                    SignalDomain::Capture => link.input_port_id == port.port_id,
                })
                .count();
            if route_count != 1 {
                return None;
            }
        }
        let mut channels: Vec<_> = channels.into_iter().collect();
        channels.sort();
        Some(channels)
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
                Err(RoutePlanError::MissingFilterPort { .. }) => {
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
                    eprintln!("loudnessd: stream {node_id} cannot be routed: {error:?}");
                    self.unsupported.insert(node_id);
                    continue;
                }
            };
            if !self.sync_recovery_journal(Some(&plan)) {
                self.unsupported.insert(node_id);
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
                    self.unsupported.insert(node_id);
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

fn gain_is_limited(config: ControllerConfig, gain_db: f32, source_lufs: Option<f32>) -> bool {
    const TOLERANCE_DB: f32 = 0.001;
    (gain_db - config.maximum_boost_db).abs() < TOLERANCE_DB
        || (gain_db + config.maximum_cut_db).abs() < TOLERANCE_DB
        || source_lufs.is_some_and(|source_lufs| {
            let required_gain_db = config.target_lufs - source_lufs;
            required_gain_db > config.maximum_boost_db + TOLERANCE_DB
                || required_gain_db < -config.maximum_cut_db - TOLERANCE_DB
        })
}

fn recovery_endpoints_present(graph: &GraphState, specs: &[crate::routing::LinkSpec]) -> bool {
    specs.iter().all(|spec| {
        graph.contains_port(
            spec.output.node_id,
            spec.output.port_id,
            GraphPortDirection::Output,
        ) && graph.contains_port(
            spec.input.node_id,
            spec.input.port_id,
            GraphPortDirection::Input,
        )
    })
}

fn control_status(decision: crate::Decision) -> ControlStatus {
    match decision {
        crate::Decision::Bypass => ControlStatus::Bypass,
        crate::Decision::Silence { .. } => ControlStatus::Silence,
        crate::Decision::Hold { .. } => ControlStatus::Settled,
        crate::Decision::Adjust { .. } => ControlStatus::Converging,
    }
}

fn route_status(health: RouteHealth) -> RouteStatus {
    match health {
        RouteHealth::Healthy => RouteStatus::Healthy,
        RouteHealth::Superseded => RouteStatus::Superseded,
        RouteHealth::Broken => RouteStatus::Broken,
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
    let registry = core.get_registry_rc()?;
    let (graph, _registry_listener) = track_graph(&registry);
    let recovery_journal = RecoveryJournal::new(path_for_socket(&socket_path));
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
        unsupported: HashSet::new(),
        retained_direct_links: Vec::new(),
        recovery_journal,
        config_path,
        runtime_config: RuntimeConfig::new(baseline),
        enabled: true,
    };

    daemon.recover_prior_routes();

    eprintln!("loudnessd: monitoring and normalizing PipeWire application streams");
    while !terminated.load(Ordering::Relaxed) {
        main_loop.loop_().iterate(Timeout::Finite(CONTROL_INTERVAL));
        daemon.tick();
        daemon.process_requests(&control_server);
    }
    if daemon.bypass_all() {
        Ok(())
    } else {
        Err("could not restore every direct route during shutdown".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn reports_gain_limited_before_slew_reaches_the_limit() {
        let config = ControllerConfig::default();

        assert!(gain_is_limited(config, 3.0, Some(-40.0)));
        assert!(gain_is_limited(
            config,
            config.maximum_boost_db,
            Some(-20.0)
        ));
        assert!(!gain_is_limited(config, 3.0, Some(-16.0)));
        assert!(!gain_is_limited(config, 0.0, None));
    }
}
