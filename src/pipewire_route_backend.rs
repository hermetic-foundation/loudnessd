// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::RefCell,
    fmt,
    rc::Rc,
    time::{Duration, Instant},
};

use pipewire::{core::CoreRc, loop_::Timeout, main_loop::MainLoopRc, registry::RegistryRc};

use crate::{
    pipewire_backend::GraphState,
    pipewire_filter::ConnectedFilter,
    pipewire_links::OwnedLinks,
    route_transaction::RouteBackend,
    routing::{LinkSpec, OriginalLink},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PipewireRouteError {
    Create(String),
    Remove(String),
    ConfirmCreate,
    ConfirmRemove,
    WrongFilter {
        expected: Option<u32>,
        requested: u32,
    },
    Activate(String),
    Restore(String),
}

impl fmt::Display for PipewireRouteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create(error) => write!(formatter, "could not create PipeWire links: {error}"),
            Self::Remove(error) => write!(formatter, "could not remove a PipeWire link: {error}"),
            Self::ConfirmCreate => formatter.write_str("timed out confirming PipeWire links"),
            Self::ConfirmRemove => formatter.write_str("timed out confirming link removal"),
            Self::WrongFilter {
                expected,
                requested,
            } => write!(
                formatter,
                "route requested filter {requested}, but backend owns {expected:?}"
            ),
            Self::Activate(error) => write!(formatter, "could not change filter state: {error}"),
            Self::Restore(error) => write!(formatter, "could not restore direct links: {error}"),
        }
    }
}

impl std::error::Error for PipewireRouteError {}

pub struct PipewireRouteBackend<'a> {
    main_loop: &'a MainLoopRc,
    core: &'a CoreRc,
    registry: &'a RegistryRc,
    graph: Rc<RefCell<GraphState>>,
    filter: &'a mut ConnectedFilter,
    retained_direct_links: &'a mut Vec<OwnedLinks>,
    timeout: Duration,
}

impl<'a> PipewireRouteBackend<'a> {
    pub fn new(
        main_loop: &'a MainLoopRc,
        core: &'a CoreRc,
        registry: &'a RegistryRc,
        graph: Rc<RefCell<GraphState>>,
        filter: &'a mut ConnectedFilter,
        retained_direct_links: &'a mut Vec<OwnedLinks>,
    ) -> Self {
        Self {
            main_loop,
            core,
            registry,
            graph,
            filter,
            retained_direct_links,
            timeout: Duration::from_secs(2),
        }
    }

    fn wait_for(&self, predicate: impl Fn(&GraphState) -> bool) -> bool {
        let deadline = Instant::now() + self.timeout;
        loop {
            if predicate(&self.graph.borrow()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            self.main_loop
                .loop_()
                .iterate(Timeout::Finite(Duration::from_millis(20)));
        }
    }

    fn links_present(&self, specs: &[LinkSpec]) -> bool {
        self.wait_for(|graph| {
            specs
                .iter()
                .all(|spec| graph.contains_link(spec.output.port_id, spec.input.port_id))
        })
    }

    fn links_absent(&self, links: &[OriginalLink]) -> bool {
        self.wait_for(|graph| {
            links
                .iter()
                .all(|link| !graph.contains_link_id(link.link_id))
        })
    }

    fn create_confirmed(&self, specs: &[LinkSpec]) -> Result<OwnedLinks, PipewireRouteError> {
        let links = OwnedLinks::create(self.core, specs)
            .map_err(|error| PipewireRouteError::Create(error.to_string()))?;
        if self.links_present(specs) {
            Ok(links)
        } else {
            links.destroy();
            Err(PipewireRouteError::ConfirmCreate)
        }
    }

    fn restore_missing(&mut self, links: &[OriginalLink]) -> Result<(), PipewireRouteError> {
        let missing: Vec<_> = {
            let graph = self.graph.borrow();
            links
                .iter()
                .filter(|link| !graph.contains_link_id(link.link_id))
                .map(|link| link.spec)
                .collect()
        };
        if missing.is_empty() {
            return Ok(());
        }
        let restored = self
            .create_confirmed(&missing)
            .map_err(|error| PipewireRouteError::Restore(error.to_string()))?;
        self.retained_direct_links.push(restored);
        Ok(())
    }
}

impl RouteBackend for PipewireRouteBackend<'_> {
    type LinkSet = OwnedLinks;
    type Error = PipewireRouteError;

    fn create_links(&mut self, specs: &[LinkSpec]) -> Result<Self::LinkSet, Self::Error> {
        self.create_confirmed(specs)
    }

    fn remove_originals(&mut self, links: &[OriginalLink]) -> Result<(), Self::Error> {
        for link in links {
            if let Err(error) = self.registry.destroy_global(link.link_id).into_result() {
                let removal = PipewireRouteError::Remove(error.to_string());
                self.restore_missing(links)?;
                return Err(removal);
            }
        }
        if self.links_absent(links) {
            Ok(())
        } else {
            self.restore_missing(links)?;
            Err(PipewireRouteError::ConfirmRemove)
        }
    }

    fn restore_originals(&mut self, specs: &[LinkSpec]) -> Result<(), Self::Error> {
        let restored = self
            .create_confirmed(specs)
            .map_err(|error| PipewireRouteError::Restore(error.to_string()))?;
        self.retained_direct_links.push(restored);
        Ok(())
    }

    fn set_filter_active(&mut self, filter_node_id: u32, active: bool) -> Result<(), Self::Error> {
        if self.filter.node_id() != Some(filter_node_id) {
            return Err(PipewireRouteError::WrongFilter {
                expected: self.filter.node_id(),
                requested: filter_node_id,
            });
        }
        self.filter
            .set_active(active)
            .map_err(|error| PipewireRouteError::Activate(error.to_string()))
    }

    fn destroy_links(&mut self, links: Self::LinkSet) {
        links.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SignalDomain,
        pipewire_backend::{PortDirection as GraphPortDirection, track_graph},
        pipewire_filter::{PortDirection, UnconnectedFilter},
        route_transaction::{bypass, install},
        routing::{LinkEndpoint, plan_route},
    };

    fn pump_until(main_loop: &MainLoopRc, predicate: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !predicate() && Instant::now() < deadline {
            main_loop
                .loop_()
                .iterate(Timeout::Finite(Duration::from_millis(20)));
        }
        assert!(predicate());
    }

    #[test]
    #[ignore = "requires a live PipeWire user session"]
    fn live_route_install_and_bypass_touch_only_disposable_nodes() {
        let main_loop = MainLoopRc::new(None).unwrap();
        let context = pipewire::context::ContextRc::new(&main_loop, None).unwrap();
        let core = context.connect_rc(None).unwrap();
        let registry = core.get_registry_rc().unwrap();
        let (graph, _listener) = track_graph(&registry);

        let mut source =
            UnconnectedFilter::new_on_core(&core, "loudnessd-route-test-source").unwrap();
        source
            .add_mono_port(PortDirection::Output, "output_FL", "FL")
            .unwrap();
        let source = source.connect_inactive().unwrap();
        let mut destination =
            UnconnectedFilter::new_on_core(&core, "loudnessd-route-test-destination").unwrap();
        destination
            .add_mono_port(PortDirection::Input, "input_FL", "FL")
            .unwrap();
        let destination = destination.connect_inactive().unwrap();
        let mut normalizer =
            UnconnectedFilter::new_on_core(&core, "loudnessd-route-test-normalizer").unwrap();
        normalizer
            .add_mono_port(PortDirection::Input, "input_FL", "FL")
            .unwrap();
        normalizer
            .add_mono_port(PortDirection::Output, "output_FL", "FL")
            .unwrap();
        let mut normalizer = normalizer.connect_inactive().unwrap();

        pump_until(&main_loop, || {
            source.node_id().is_some()
                && destination.node_id().is_some()
                && normalizer.node_id().is_some()
        });
        let source_id = source.node_id().unwrap();
        let destination_id = destination.node_id().unwrap();
        let normalizer_id = normalizer.node_id().unwrap();
        pump_until(&main_loop, || {
            graph
                .borrow()
                .ports()
                .filter(|port| [source_id, destination_id, normalizer_id].contains(&port.node_id))
                .count()
                == 4
        });

        let port_id = |node_id, direction| {
            graph
                .borrow()
                .ports()
                .find(|port| port.node_id == node_id && port.direction == direction)
                .unwrap()
                .port_id
        };
        let original_spec = LinkSpec {
            output: LinkEndpoint {
                node_id: source_id,
                port_id: port_id(source_id, GraphPortDirection::Output),
            },
            input: LinkEndpoint {
                node_id: destination_id,
                port_id: port_id(destination_id, GraphPortDirection::Input),
            },
        };
        let mut retained = Vec::new();
        let mut backend = PipewireRouteBackend::new(
            &main_loop,
            &core,
            &registry,
            Rc::clone(&graph),
            &mut normalizer,
            &mut retained,
        );
        let original_owner = backend.create_links(&[original_spec]).unwrap();
        let (ports, links) = {
            let graph = graph.borrow();
            (
                graph.ports().cloned().collect::<Vec<_>>(),
                graph.links().cloned().collect::<Vec<_>>(),
            )
        };
        let plan = plan_route(
            SignalDomain::Playback,
            source_id,
            normalizer_id,
            &ports,
            &links,
        )
        .unwrap();
        let replacement_specs = plan.insertion().stage;

        let active = install(&mut backend, plan).unwrap();
        assert!(
            !graph
                .borrow()
                .contains_link(original_spec.output.port_id, original_spec.input.port_id)
        );
        assert!(replacement_specs.iter().all(|spec| {
            graph
                .borrow()
                .contains_link(spec.output.port_id, spec.input.port_id)
        }));

        let bypassed = bypass(&mut backend, active).ok().unwrap();
        pump_until(&main_loop, || {
            graph
                .borrow()
                .contains_link(original_spec.output.port_id, original_spec.input.port_id)
                && replacement_specs.iter().all(|spec| {
                    !graph
                        .borrow()
                        .contains_link(spec.output.port_id, spec.input.port_id)
                })
        });

        drop(bypassed);
        drop(original_owner);
    }
}
