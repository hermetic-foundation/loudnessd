// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    error::Error,
    rc::Rc,
    time::Duration,
};

use pipewire::{
    context::ContextRc,
    loop_::{Signal, Timeout},
    main_loop::MainLoopRc,
};

use crate::{
    ControllerBank, SignalDomain,
    pipewire_backend::{
        DiscoveredStream, GraphState, PortDirection as GraphPortDirection, track_graph,
    },
    pipewire_filter::{ConnectedFilter, PortDirection, UnconnectedFilter},
    pipewire_links::OwnedLinks,
    pipewire_route_backend::PipewireRouteBackend,
    route_transaction::{ActiveRoute, bypass, install},
    routing::{RoutePlanError, plan_route},
    stream_control::StreamControl,
};

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
}

impl Daemon {
    fn tick(&mut self) {
        self.remove_disappeared_streams();
        self.discover_streams();
        self.connect_pending_filters();
        self.update_controls();
    }

    fn remove_disappeared_streams(&mut self) {
        let present: HashSet<_> = self
            .graph
            .borrow()
            .streams()
            .map(|stream| stream.node_id)
            .collect();
        self.managed.retain(|node_id, _| present.contains(node_id));
        self.unsupported.retain(|node_id| present.contains(node_id));
    }

    fn discover_streams(&mut self) {
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

    fn shutdown(&mut self) {
        let managed = std::mem::take(&mut self.managed);
        for (_, stream) in managed {
            let ManagedStream::Active {
                stream,
                mut filter,
                route,
                ..
            } = stream
            else {
                continue;
            };
            let mut backend = PipewireRouteBackend::new(
                &self.main_loop,
                &self.core,
                &self.registry,
                Rc::clone(&self.graph),
                &mut filter,
                &mut self.retained_direct_links,
            );
            match bypass(&mut backend, route) {
                Ok(bypassed) => self.retained_direct_links.push(bypassed.direct_links),
                Err(error) => eprintln!(
                    "loudnessd: failed to bypass stream {} at {:?}: {}",
                    stream.node_id, error.transition.operation, error.transition.error
                ),
            }
        }
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

pub fn run(controllers: ControllerBank) -> Result<(), Box<dyn Error>> {
    let main_loop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    let (graph, _registry_listener) = track_graph(&registry);
    let running = Rc::new(Cell::new(true));
    let signal_running = Rc::clone(&running);
    let _sig_int = main_loop
        .loop_()
        .add_signal_local(Signal::INT, move || signal_running.set(false));
    let signal_running = Rc::clone(&running);
    let _sig_term = main_loop
        .loop_()
        .add_signal_local(Signal::TERM, move || signal_running.set(false));
    let mut daemon = Daemon {
        main_loop: main_loop.clone(),
        core,
        registry,
        graph,
        controllers,
        managed: HashMap::new(),
        unsupported: HashSet::new(),
        retained_direct_links: Vec::new(),
    };

    eprintln!("loudnessd: monitoring and normalizing PipeWire application streams");
    while running.get() {
        main_loop.loop_().iterate(Timeout::Finite(CONTROL_INTERVAL));
        daemon.tick();
    }
    daemon.shutdown();
    Ok(())
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
}
