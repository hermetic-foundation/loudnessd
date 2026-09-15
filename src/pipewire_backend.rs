// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cell::RefCell, rc::Rc};

use pipewire::{context::ContextRc, main_loop::MainLoopRc, types::ObjectType};

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
