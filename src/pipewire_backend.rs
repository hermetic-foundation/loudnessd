// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use pipewire::{context::ContextRc, loop_::Signal, main_loop::MainLoopRc, types::ObjectType};

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

pub fn domain_for_media_class(media_class: &str) -> Option<SignalDomain> {
    match media_class {
        "Stream/Output/Audio" => Some(SignalDomain::Playback),
        "Stream/Input/Audio" => Some(SignalDomain::Capture),
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

#[cfg(test)]
mod tests {
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
}
