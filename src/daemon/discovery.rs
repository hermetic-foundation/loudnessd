// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashSet;

use crate::{
    SignalDomain,
    pipewire_backend::{DiscoveredStream, GraphState, PortDirection},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ChannelReadiness {
    Pending,
    Ready(Vec<String>),
    Skipped(String),
}

pub(super) fn channel_readiness(stream: &DiscoveredStream, graph: &GraphState) -> ChannelReadiness {
    let direction = match stream.domain {
        SignalDomain::Playback => PortDirection::Output,
        SignalDomain::Capture => PortDirection::Input,
    };
    let ports: Vec<_> = graph
        .ports()
        .filter(|port| port.node_id == stream.node_id && port.direction == direction)
        .collect();
    if ports.is_empty() {
        return ChannelReadiness::Pending;
    }
    if ports.len() > 2 {
        return ChannelReadiness::Skipped(format!(
            "unsupported channel count {}; only mono and stereo are supported",
            ports.len()
        ));
    }

    let links: Vec<_> = graph.links().collect();
    let mut channels = HashSet::new();
    for port in ports {
        let Some(channel) = port.channel.as_ref() else {
            return ChannelReadiness::Skipped(format!(
                "stream port {} has no audio channel label",
                port.port_id
            ));
        };
        if !channels.insert(channel.clone()) {
            return ChannelReadiness::Skipped(format!(
                "stream has duplicate audio channel {channel}"
            ));
        }
        let route_count = links
            .iter()
            .filter(|link| match stream.domain {
                SignalDomain::Playback => link.output_port_id == port.port_id,
                SignalDomain::Capture => link.input_port_id == port.port_id,
            })
            .count();
        match route_count {
            0 => return ChannelReadiness::Pending,
            1 => {}
            count => {
                return ChannelReadiness::Skipped(format!(
                    "stream port {} has {count} routes; exactly one is required",
                    port.port_id
                ));
            }
        }
    }

    let mut channels: Vec<_> = channels.into_iter().collect();
    channels.sort();
    ChannelReadiness::Ready(channels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipewire_backend::{DiscoveredLink, DiscoveredPort, GraphObject, PortDirection};

    fn stream(domain: SignalDomain) -> DiscoveredStream {
        DiscoveredStream {
            node_id: 10,
            domain,
            application_id: Some("org.example.Player".to_owned()),
            application_name: None,
            process_binary: None,
            media_name: None,
        }
    }

    fn port(id: u32, direction: PortDirection, channel: Option<&str>) -> GraphObject {
        GraphObject::Port(DiscoveredPort {
            port_id: id,
            node_id: 10,
            direction,
            name: None,
            channel: channel.map(str::to_owned),
            format_dsp: None,
        })
    }

    fn link(id: u32, output_port_id: u32) -> GraphObject {
        GraphObject::Link(DiscoveredLink {
            link_id: id,
            output_node_id: 10,
            output_port_id,
            input_node_id: 20,
            input_port_id: 100 + id,
        })
    }

    #[test]
    fn waits_for_ports_and_links_to_appear() {
        let mut graph = GraphState::default();
        assert_eq!(
            channel_readiness(&stream(SignalDomain::Playback), &graph),
            ChannelReadiness::Pending
        );

        graph.insert(port(11, PortDirection::Output, Some("FL")));
        assert_eq!(
            channel_readiness(&stream(SignalDomain::Playback), &graph),
            ChannelReadiness::Pending
        );
    }

    #[test]
    fn accepts_complete_mono_and_stereo_routes() {
        let mut mono = GraphState::default();
        mono.insert(port(11, PortDirection::Output, Some("FL")));
        mono.insert(link(1, 11));
        assert_eq!(
            channel_readiness(&stream(SignalDomain::Playback), &mono),
            ChannelReadiness::Ready(vec!["FL".to_owned()])
        );

        let mut stereo = mono;
        stereo.insert(port(12, PortDirection::Output, Some("FR")));
        stereo.insert(link(2, 12));
        assert_eq!(
            channel_readiness(&stream(SignalDomain::Playback), &stereo),
            ChannelReadiness::Ready(vec!["FL".to_owned(), "FR".to_owned()])
        );

        let mut capture = GraphState::default();
        capture.insert(port(21, PortDirection::Input, Some("MONO")));
        capture.insert(GraphObject::Link(DiscoveredLink {
            link_id: 3,
            output_node_id: 20,
            output_port_id: 120,
            input_node_id: 10,
            input_port_id: 21,
        }));
        assert_eq!(
            channel_readiness(&stream(SignalDomain::Capture), &capture),
            ChannelReadiness::Ready(vec!["MONO".to_owned()])
        );
    }

    #[test]
    fn explains_permanently_unsupported_topology() {
        let mut missing_label = GraphState::default();
        missing_label.insert(port(11, PortDirection::Output, None));
        assert_eq!(
            channel_readiness(&stream(SignalDomain::Playback), &missing_label),
            ChannelReadiness::Skipped("stream port 11 has no audio channel label".to_owned())
        );

        let mut ambiguous = GraphState::default();
        ambiguous.insert(port(11, PortDirection::Output, Some("FL")));
        ambiguous.insert(link(1, 11));
        ambiguous.insert(link(2, 11));
        assert_eq!(
            channel_readiness(&stream(SignalDomain::Playback), &ambiguous),
            ChannelReadiness::Skipped(
                "stream port 11 has 2 routes; exactly one is required".to_owned()
            )
        );

        let mut multichannel = GraphState::default();
        for (id, channel) in [(11, "FL"), (12, "FR"), (13, "FC")] {
            multichannel.insert(port(id, PortDirection::Output, Some(channel)));
        }
        assert_eq!(
            channel_readiness(&stream(SignalDomain::Playback), &multichannel),
            ChannelReadiness::Skipped(
                "unsupported channel count 3; only mono and stereo are supported".to_owned()
            )
        );
    }
}
