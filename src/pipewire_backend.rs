// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use pipewire::{
    context::ContextRc,
    loop_::Signal,
    main_loop::MainLoopRc,
    registry::{Listener as RegistryListener, RegistryRc},
    types::ObjectType,
};

use crate::SignalDomain;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredStream {
    pub node_id: u32,
    pub domain: SignalDomain,
    pub application_id: Option<String>,
    pub application_name: Option<String>,
    pub process_binary: Option<String>,
    pub media_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistryEvent {
    Added(DiscoveredStream),
    Removed(DiscoveredStream),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PortDirection {
    Input,
    Output,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredPort {
    pub port_id: u32,
    pub node_id: u32,
    pub direction: PortDirection,
    pub name: Option<String>,
    pub channel: Option<String>,
    pub format_dsp: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredLink {
    pub link_id: u32,
    pub output_node_id: u32,
    pub output_port_id: u32,
    pub input_node_id: u32,
    pub input_port_id: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphObject {
    Stream(DiscoveredStream),
    Port(DiscoveredPort),
    Link(DiscoveredLink),
}

impl GraphObject {
    pub fn id(&self) -> u32 {
        match self {
            Self::Stream(stream) => stream.node_id,
            Self::Port(port) => port.port_id,
            Self::Link(link) => link.link_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphEvent {
    Added(GraphObject),
    Removed(GraphObject),
}

#[derive(Clone, Debug, Default)]
pub struct GraphState {
    streams: HashMap<u32, DiscoveredStream>,
    ports: HashMap<u32, DiscoveredPort>,
    links: HashMap<u32, DiscoveredLink>,
}

impl GraphState {
    pub fn apply(&mut self, event: GraphEvent) {
        match event {
            GraphEvent::Added(object) => self.insert(object),
            GraphEvent::Removed(object) => self.remove(&object),
        }
    }

    pub fn insert(&mut self, object: GraphObject) {
        match object {
            GraphObject::Stream(stream) => {
                self.streams.insert(stream.node_id, stream);
            }
            GraphObject::Port(port) => {
                self.ports.insert(port.port_id, port);
            }
            GraphObject::Link(link) => {
                self.links.insert(link.link_id, link);
            }
        }
    }

    pub fn remove(&mut self, object: &GraphObject) {
        match object {
            GraphObject::Stream(stream) => {
                self.streams.remove(&stream.node_id);
                self.ports.retain(|_, port| port.node_id != stream.node_id);
                self.links.retain(|_, link| {
                    link.output_node_id != stream.node_id && link.input_node_id != stream.node_id
                });
            }
            GraphObject::Port(port) => {
                self.ports.remove(&port.port_id);
                self.links.retain(|_, link| {
                    link.output_port_id != port.port_id && link.input_port_id != port.port_id
                });
            }
            GraphObject::Link(link) => {
                self.links.remove(&link.link_id);
            }
        }
    }

    pub fn remove_id(&mut self, id: u32) {
        if let Some(stream) = self.streams.remove(&id) {
            self.ports.retain(|_, port| port.node_id != stream.node_id);
            self.links.retain(|_, link| {
                link.output_node_id != stream.node_id && link.input_node_id != stream.node_id
            });
            return;
        }
        if let Some(port) = self.ports.remove(&id) {
            self.links.retain(|_, link| {
                link.output_port_id != port.port_id && link.input_port_id != port.port_id
            });
            return;
        }
        self.links.remove(&id);
    }

    pub fn stream(&self, node_id: u32) -> Option<&DiscoveredStream> {
        self.streams.get(&node_id)
    }

    pub fn streams(&self) -> impl ExactSizeIterator<Item = &DiscoveredStream> {
        self.streams.values()
    }

    pub fn ports(&self) -> impl ExactSizeIterator<Item = &DiscoveredPort> {
        self.ports.values()
    }

    pub fn links(&self) -> impl ExactSizeIterator<Item = &DiscoveredLink> {
        self.links.values()
    }

    pub fn contains_link(&self, output_port_id: u32, input_port_id: u32) -> bool {
        self.links.values().any(|link| {
            link.output_port_id == output_port_id && link.input_port_id == input_port_id
        })
    }
}

pub fn track_graph(registry: &RegistryRc) -> (Rc<RefCell<GraphState>>, RegistryListener) {
    let state = Rc::new(RefCell::new(GraphState::default()));
    let added_state = Rc::clone(&state);
    let removed_state = Rc::clone(&state);
    let listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(properties) = global.props.as_ref() else {
                return;
            };
            if let Some(object) =
                discover_graph_object(&global.type_, global.id, |key| properties.get(key))
            {
                added_state.borrow_mut().insert(object);
            }
        })
        .global_remove(move |id| removed_state.borrow_mut().remove_id(id))
        .register();
    (state, listener)
}

pub fn domain_for_media_class(media_class: &str) -> Option<SignalDomain> {
    match media_class {
        "Stream/Output/Audio" => Some(SignalDomain::Playback),
        "Stream/Input/Audio" => Some(SignalDomain::Capture),
        _ => None,
    }
}

fn parse_id(property: Option<&str>) -> Option<u32> {
    property?.parse().ok()
}

fn discover_graph_object<'a, F>(type_: &ObjectType, id: u32, property: F) -> Option<GraphObject>
where
    F: Fn(&str) -> Option<&'a str>,
{
    let owned = |key: &str| property(key).map(str::to_owned);
    match type_ {
        ObjectType::Node => Some(GraphObject::Stream(DiscoveredStream {
            node_id: id,
            domain: property("media.class").and_then(domain_for_media_class)?,
            application_id: owned("application.id"),
            application_name: owned("application.name"),
            process_binary: owned("application.process.binary"),
            media_name: owned("media.name"),
        })),
        ObjectType::Port => Some(GraphObject::Port(DiscoveredPort {
            port_id: id,
            node_id: parse_id(property("node.id"))?,
            direction: match property("port.direction")? {
                "in" => PortDirection::Input,
                "out" => PortDirection::Output,
                _ => return None,
            },
            name: owned("port.name"),
            channel: owned("audio.channel"),
            format_dsp: owned("format.dsp"),
        })),
        ObjectType::Link => Some(GraphObject::Link(DiscoveredLink {
            link_id: id,
            output_node_id: parse_id(property("link.output.node"))?,
            output_port_id: parse_id(property("link.output.port"))?,
            input_node_id: parse_id(property("link.input.node"))?,
            input_port_id: parse_id(property("link.input.port"))?,
        })),
        _ => None,
    }
}

pub fn snapshot_streams() -> Result<Vec<DiscoveredStream>, pipewire::Error> {
    let main_loop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    let streams = Rc::new(RefCell::new(Vec::new()));

    let callback_streams = Rc::clone(&streams);
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ != ObjectType::Node {
                return;
            }
            let Some(properties) = global.props.as_ref() else {
                return;
            };
            let Some(domain) = properties
                .get("media.class")
                .and_then(domain_for_media_class)
            else {
                return;
            };
            let property = |key: &str| properties.get(key).map(str::to_owned);
            callback_streams.borrow_mut().push(DiscoveredStream {
                node_id: global.id,
                domain,
                application_id: property("application.id"),
                application_name: property("application.name"),
                process_binary: property("application.process.binary"),
                media_name: property("media.name"),
            });
        })
        .register();

    let callback_loop = main_loop.clone();
    let _core_listener = core
        .add_listener_local()
        .done(move |_, _| callback_loop.quit())
        .register();
    core.sync(0)?;
    main_loop.run();

    let snapshot = streams.borrow().clone();
    Ok(snapshot)
}

pub fn snapshot_graph() -> Result<Vec<GraphObject>, pipewire::Error> {
    let main_loop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    let objects = Rc::new(RefCell::new(Vec::new()));

    let callback_objects = Rc::clone(&objects);
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(properties) = global.props.as_ref() else {
                return;
            };
            if let Some(object) =
                discover_graph_object(&global.type_, global.id, |key| properties.get(key))
            {
                callback_objects.borrow_mut().push(object);
            }
        })
        .register();

    let callback_loop = main_loop.clone();
    let _core_listener = core
        .add_listener_local()
        .done(move |_, _| callback_loop.quit())
        .register();
    core.sync(0)?;
    main_loop.run();

    let snapshot = objects.borrow().clone();
    Ok(snapshot)
}

pub fn monitor_streams<F>(callback: F) -> Result<(), pipewire::Error>
where
    F: FnMut(RegistryEvent) + 'static,
{
    let main_loop = MainLoopRc::new(None)?;
    let main_loop_weak = main_loop.downgrade();
    let _sig_int = main_loop.loop_().add_signal_local(Signal::INT, move || {
        if let Some(main_loop) = main_loop_weak.upgrade() {
            main_loop.quit();
        }
    });
    let main_loop_weak = main_loop.downgrade();
    let _sig_term = main_loop.loop_().add_signal_local(Signal::TERM, move || {
        if let Some(main_loop) = main_loop_weak.upgrade() {
            main_loop.quit();
        }
    });

    let context = ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    let streams = Rc::new(RefCell::new(HashMap::<u32, DiscoveredStream>::new()));
    let callback = Rc::new(RefCell::new(callback));

    let added_streams = Rc::clone(&streams);
    let added_callback = Rc::clone(&callback);
    let removed_streams = Rc::clone(&streams);
    let removed_callback = Rc::clone(&callback);
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ != ObjectType::Node {
                return;
            }
            let Some(properties) = global.props.as_ref() else {
                return;
            };
            let Some(domain) = properties
                .get("media.class")
                .and_then(domain_for_media_class)
            else {
                return;
            };
            let property = |key: &str| properties.get(key).map(str::to_owned);
            let stream = DiscoveredStream {
                node_id: global.id,
                domain,
                application_id: property("application.id"),
                application_name: property("application.name"),
                process_binary: property("application.process.binary"),
                media_name: property("media.name"),
            };
            added_streams
                .borrow_mut()
                .insert(stream.node_id, stream.clone());
            added_callback.borrow_mut()(RegistryEvent::Added(stream));
        })
        .global_remove(move |node_id| {
            if let Some(stream) = removed_streams.borrow_mut().remove(&node_id) {
                removed_callback.borrow_mut()(RegistryEvent::Removed(stream));
            }
        })
        .register();

    main_loop.run();
    Ok(())
}

pub fn monitor_graph<F>(callback: F) -> Result<(), pipewire::Error>
where
    F: FnMut(GraphEvent) + 'static,
{
    let main_loop = MainLoopRc::new(None)?;
    let main_loop_weak = main_loop.downgrade();
    let _sig_int = main_loop.loop_().add_signal_local(Signal::INT, move || {
        if let Some(main_loop) = main_loop_weak.upgrade() {
            main_loop.quit();
        }
    });
    let main_loop_weak = main_loop.downgrade();
    let _sig_term = main_loop.loop_().add_signal_local(Signal::TERM, move || {
        if let Some(main_loop) = main_loop_weak.upgrade() {
            main_loop.quit();
        }
    });

    let context = ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    let objects = Rc::new(RefCell::new(HashMap::<u32, GraphObject>::new()));
    let callback = Rc::new(RefCell::new(callback));

    let added_objects = Rc::clone(&objects);
    let added_callback = Rc::clone(&callback);
    let removed_objects = Rc::clone(&objects);
    let removed_callback = Rc::clone(&callback);
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(properties) = global.props.as_ref() else {
                return;
            };
            let Some(object) =
                discover_graph_object(&global.type_, global.id, |key| properties.get(key))
            else {
                return;
            };
            added_objects
                .borrow_mut()
                .insert(object.id(), object.clone());
            added_callback.borrow_mut()(GraphEvent::Added(object));
        })
        .global_remove(move |id| {
            if let Some(object) = removed_objects.borrow_mut().remove(&id) {
                removed_callback.borrow_mut()(GraphEvent::Removed(object));
            }
        })
        .register();

    main_loop.run();
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn classifies_only_application_audio_streams() {
        assert_eq!(
            domain_for_media_class("Stream/Output/Audio"),
            Some(SignalDomain::Playback)
        );
        assert_eq!(
            domain_for_media_class("Stream/Input/Audio"),
            Some(SignalDomain::Capture)
        );
        assert_eq!(domain_for_media_class("Stream/Input/Audio/Internal"), None);
        assert_eq!(domain_for_media_class("Audio/Source"), None);
        assert_eq!(domain_for_media_class("Audio/Sink"), None);
    }

    #[test]
    fn discovers_audio_ports_from_registry_properties() {
        let properties = HashMap::from([
            ("node.id", "41"),
            ("port.direction", "out"),
            ("port.name", "output_FL"),
            ("audio.channel", "FL"),
            ("format.dsp", "32 bit float mono audio"),
        ]);

        assert_eq!(
            discover_graph_object(&ObjectType::Port, 52, |key| properties.get(key).copied()),
            Some(GraphObject::Port(DiscoveredPort {
                port_id: 52,
                node_id: 41,
                direction: PortDirection::Output,
                name: Some("output_FL".to_owned()),
                channel: Some("FL".to_owned()),
                format_dsp: Some("32 bit float mono audio".to_owned()),
            }))
        );
    }

    #[test]
    fn discovers_links_from_registry_properties() {
        let properties = HashMap::from([
            ("link.output.node", "10"),
            ("link.output.port", "11"),
            ("link.input.node", "20"),
            ("link.input.port", "21"),
        ]);

        assert_eq!(
            discover_graph_object(&ObjectType::Link, 30, |key| properties.get(key).copied()),
            Some(GraphObject::Link(DiscoveredLink {
                link_id: 30,
                output_node_id: 10,
                output_port_id: 11,
                input_node_id: 20,
                input_port_id: 21,
            }))
        );
    }

    #[test]
    fn rejects_incomplete_graph_objects() {
        let properties = HashMap::from([("node.id", "invalid")]);
        assert_eq!(
            discover_graph_object(&ObjectType::Port, 1, |key| properties.get(key).copied()),
            None
        );
    }

    #[test]
    fn graph_state_removes_dependent_objects_with_a_node() {
        let stream = DiscoveredStream {
            node_id: 10,
            domain: SignalDomain::Playback,
            application_id: None,
            application_name: None,
            process_binary: None,
            media_name: None,
        };
        let port = DiscoveredPort {
            port_id: 11,
            node_id: 10,
            direction: PortDirection::Output,
            name: None,
            channel: Some("FL".to_owned()),
            format_dsp: None,
        };
        let link = DiscoveredLink {
            link_id: 12,
            output_node_id: 10,
            output_port_id: 11,
            input_node_id: 20,
            input_port_id: 21,
        };
        let mut state = GraphState::default();
        state.insert(GraphObject::Stream(stream.clone()));
        state.insert(GraphObject::Port(port));
        state.insert(GraphObject::Link(link));

        state.apply(GraphEvent::Removed(GraphObject::Stream(stream)));

        assert_eq!(state.streams().len(), 0);
        assert_eq!(state.ports().len(), 0);
        assert_eq!(state.links().len(), 0);
    }

    #[test]
    fn graph_state_matches_links_by_port_endpoints() {
        let mut state = GraphState::default();
        state.insert(GraphObject::Link(DiscoveredLink {
            link_id: 12,
            output_node_id: 10,
            output_port_id: 11,
            input_node_id: 20,
            input_port_id: 21,
        }));

        assert!(state.contains_link(11, 21));
        assert!(!state.contains_link(21, 11));
    }

    #[test]
    fn removing_a_port_id_removes_its_links() {
        let mut state = GraphState::default();
        state.insert(GraphObject::Port(DiscoveredPort {
            port_id: 11,
            node_id: 10,
            direction: PortDirection::Output,
            name: None,
            channel: Some("FL".to_owned()),
            format_dsp: None,
        }));
        state.insert(GraphObject::Link(DiscoveredLink {
            link_id: 12,
            output_node_id: 10,
            output_port_id: 11,
            input_node_id: 20,
            input_port_id: 21,
        }));

        state.remove_id(11);

        assert_eq!(state.ports().len(), 0);
        assert_eq!(state.links().len(), 0);
    }

    #[test]
    #[ignore = "requires a live PipeWire user session"]
    fn live_graph_snapshot_has_resolvable_links() {
        let graph = snapshot_graph().unwrap();
        let ports: HashMap<_, _> = graph
            .iter()
            .filter_map(|object| match object {
                GraphObject::Port(port) => Some((port.port_id, port)),
                _ => None,
            })
            .collect();
        let links: Vec<_> = graph
            .iter()
            .filter_map(|object| match object {
                GraphObject::Link(link) => Some(link),
                _ => None,
            })
            .collect();

        assert!(!ports.is_empty());
        assert!(!links.is_empty());
        for link in links {
            assert_eq!(ports[&link.output_port_id].node_id, link.output_node_id);
            assert_eq!(ports[&link.input_port_id].node_id, link.input_node_id);
        }
    }
}
