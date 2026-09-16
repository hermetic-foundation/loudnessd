// SPDX-License-Identifier: AGPL-3.0-or-later

use pipewire::{
    core::CoreRc, link::Link, properties::PropertiesBox, proxy::ProxyT, registry::RegistryRc,
};

use crate::routing::LinkSpec;

pub struct OwnedLinks {
    links: Vec<Link>,
}

impl OwnedLinks {
    pub fn create(core: &CoreRc, specs: &[LinkSpec]) -> Result<Self, pipewire::Error> {
        Self::create_with_linger(core, None, specs, false)
    }

    pub fn create_lingering(
        core: &CoreRc,
        registry: &RegistryRc,
        specs: &[LinkSpec],
    ) -> Result<Self, pipewire::Error> {
        Self::create_with_linger(core, Some(registry), specs, true)
    }

    fn create_with_linger(
        core: &CoreRc,
        registry: Option<&RegistryRc>,
        specs: &[LinkSpec],
        linger: bool,
    ) -> Result<Self, pipewire::Error> {
        let mut owned = Self {
            links: Vec::with_capacity(specs.len()),
        };
        for spec in specs {
            let properties = link_properties(*spec, linger);
            match core.create_object::<Link>("link-factory", &properties) {
                Ok(link) => owned.links.push(link),
                Err(error) => {
                    if let Some(registry) = registry {
                        owned.destroy_globals_in_place(registry);
                    } else {
                        owned.destroy_all();
                    }
                    return Err(error);
                }
            }
        }
        Ok(owned)
    }

    pub fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.links.iter().map(|link| link.upcast_ref().id())
    }

    pub fn len(&self) -> usize {
        self.links.len()
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    pub fn destroy(mut self) {
        self.destroy_all();
    }

    pub fn destroy_globals(mut self, registry: &RegistryRc) {
        self.destroy_globals_in_place(registry);
    }

    fn destroy_globals_in_place(&mut self, registry: &RegistryRc) {
        for id in self.ids().collect::<Vec<_>>() {
            let _ = registry.destroy_global(id).into_result();
        }
        self.destroy_all();
    }

    fn destroy_all(&mut self) {
        // Dropping non-lingering proxies removes their server objects. Keeping
        // the proxies here also ties every link to the client connection, so a
        // daemon crash cannot leave persistent routes behind.
        self.links.clear();
    }
}

impl Drop for OwnedLinks {
    fn drop(&mut self) {
        self.destroy_all();
    }
}

fn link_properties(spec: LinkSpec, linger: bool) -> PropertiesBox {
    let output_node = spec.output.node_id.to_string();
    let output_port = spec.output.port_id.to_string();
    let input_node = spec.input.node_id.to_string();
    let input_port = spec.input.port_id.to_string();
    pipewire::properties::properties! {
        "link.output.node" => output_node,
        "link.output.port" => output_port,
        "link.input.node" => input_node,
        "link.input.port" => input_port,
        "object.linger" => if linger { "true" } else { "false" },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        rc::Rc,
        time::{Duration, Instant},
    };

    use pipewire::loop_::Timeout;

    use super::*;
    use crate::{
        pipewire_backend::{GraphObject, PortDirection as GraphPortDirection, snapshot_graph},
        pipewire_filter::{PortDirection, UnconnectedFilter},
        routing::LinkEndpoint,
    };

    fn spec() -> LinkSpec {
        LinkSpec {
            output: LinkEndpoint {
                node_id: 10,
                port_id: 11,
            },
            input: LinkEndpoint {
                node_id: 20,
                port_id: 21,
            },
        }
    }

    fn roundtrip(main_loop: &pipewire::main_loop::MainLoopRc, core: &CoreRc) {
        let done = Rc::new(Cell::new(false));
        let callback_done = Rc::clone(&done);
        let callback_loop = main_loop.clone();
        let pending = core.sync(0).unwrap();
        let _listener = core
            .add_listener_local()
            .done(move |id, sequence| {
                if id == pipewire::core::PW_ID_CORE && sequence == pending {
                    callback_done.set(true);
                    callback_loop.quit();
                }
            })
            .register();
        while !done.get() {
            main_loop.run();
        }
    }

    #[test]
    fn link_properties_are_explicit_and_non_persistent() {
        let properties = link_properties(spec(), false);

        assert_eq!(properties.get("link.output.node"), Some("10"));
        assert_eq!(properties.get("link.output.port"), Some("11"));
        assert_eq!(properties.get("link.input.node"), Some("20"));
        assert_eq!(properties.get("link.input.port"), Some("21"));
        assert_eq!(properties.get("object.linger"), Some("false"));

        let restored = link_properties(spec(), true);
        assert_eq!(restored.get("object.linger"), Some("true"));
    }

    #[test]
    #[ignore = "requires a live PipeWire user session"]
    fn live_link_lifetimes_match_the_linger_policy() {
        let main_loop = pipewire::main_loop::MainLoopRc::new(None).unwrap();
        let context = pipewire::context::ContextRc::new(&main_loop, None).unwrap();
        let core = context.connect_rc(None).unwrap();

        let mut producer =
            UnconnectedFilter::new_on_core(&core, "loudnessd-link-test-source").unwrap();
        producer
            .add_mono_port(PortDirection::Output, "output_FL", "FL")
            .unwrap();
        let mut producer = producer.connect_inactive().unwrap();

        let mut consumer =
            UnconnectedFilter::new_on_core(&core, "loudnessd-link-test-sink").unwrap();
        consumer
            .add_mono_port(PortDirection::Input, "input_FL", "FL")
            .unwrap();
        let mut consumer = consumer.connect_inactive().unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while (producer.node_id().is_none() || consumer.node_id().is_none())
            && Instant::now() < deadline
        {
            main_loop
                .loop_()
                .iterate(Timeout::Finite(Duration::from_millis(20)));
        }
        let producer_id = producer.node_id().unwrap();
        let consumer_id = consumer.node_id().unwrap();
        producer.set_active(true).unwrap();
        consumer.set_active(true).unwrap();
        let graph = snapshot_graph().unwrap();
        let output = graph.iter().find_map(|object| match object {
            GraphObject::Port(port)
                if port.node_id == producer_id && port.direction == GraphPortDirection::Output =>
            {
                Some(port.port_id)
            }
            _ => None,
        });
        let input = graph.iter().find_map(|object| match object {
            GraphObject::Port(port)
                if port.node_id == consumer_id && port.direction == GraphPortDirection::Input =>
            {
                Some(port.port_id)
            }
            _ => None,
        });
        let spec = LinkSpec {
            output: LinkEndpoint {
                node_id: producer_id,
                port_id: output.unwrap(),
            },
            input: LinkEndpoint {
                node_id: consumer_id,
                port_id: input.unwrap(),
            },
        };

        let links = OwnedLinks::create(&core, &[spec]).unwrap();
        assert_eq!(links.len(), 1);
        roundtrip(&main_loop, &core);
        let link_exists = || {
            snapshot_graph().unwrap().iter().any(|object| {
                matches!(object, GraphObject::Link(link)
                    if link.output_port_id == spec.output.port_id
                        && link.input_port_id == spec.input.port_id)
            })
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while !link_exists() && Instant::now() < deadline {
            main_loop
                .loop_()
                .iterate(Timeout::Finite(Duration::from_millis(20)));
        }
        assert!(link_exists());

        links.destroy();
        roundtrip(&main_loop, &core);
        let deadline = Instant::now() + Duration::from_secs(2);
        while link_exists() && Instant::now() < deadline {
            main_loop
                .loop_()
                .iterate(Timeout::Finite(Duration::from_millis(20)));
        }
        assert!(!link_exists());

        let registry = core.get_registry_rc().unwrap();
        let lingering = OwnedLinks::create_lingering(&core, &registry, &[spec]).unwrap();
        roundtrip(&main_loop, &core);
        assert!(link_exists());

        drop(lingering);
        roundtrip(&main_loop, &core);
        assert!(link_exists());

        drop(producer);
        drop(consumer);
        roundtrip(&main_loop, &core);
        let deadline = Instant::now() + Duration::from_secs(2);
        while link_exists() && Instant::now() < deadline {
            main_loop
                .loop_()
                .iterate(Timeout::Finite(Duration::from_millis(20)));
        }
        assert!(!link_exists());
    }
}
