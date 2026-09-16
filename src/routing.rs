// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use serde::{Deserialize, Serialize};

use crate::{
    SignalDomain,
    pipewire_backend::{DiscoveredLink, DiscoveredPort, GraphState, PortDirection},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LinkEndpoint {
    pub node_id: u32,
    pub port_id: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LinkSpec {
    pub output: LinkEndpoint,
    pub input: LinkEndpoint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelRoute {
    pub channel: String,
    pub original_link_id: u32,
    pub original: LinkSpec,
    pub into_filter: LinkSpec,
    pub out_of_filter: LinkSpec,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoutePlan {
    pub stream_node_id: u32,
    pub filter_node_id: u32,
    pub channels: Vec<ChannelRoute>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteTransition {
    pub stage: Vec<LinkSpec>,
    pub cutover: Vec<OriginalLink>,
    pub activate_filter: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OriginalLink {
    pub link_id: u32,
    pub spec: LinkSpec,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteTeardown {
    pub restore: Vec<LinkSpec>,
    pub deactivate_filter: bool,
    pub remove_owned_links: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteHealth {
    Healthy,
    Superseded,
    EndpointsGone,
    Broken,
}

impl RoutePlan {
    pub fn insertion(&self) -> RouteTransition {
        RouteTransition {
            stage: self
                .channels
                .iter()
                .flat_map(|route| [route.into_filter, route.out_of_filter])
                .collect(),
            cutover: self
                .channels
                .iter()
                .map(|route| OriginalLink {
                    link_id: route.original_link_id,
                    spec: route.original,
                })
                .collect(),
            activate_filter: true,
        }
    }

    pub fn teardown(&self) -> RouteTeardown {
        RouteTeardown {
            restore: self.channels.iter().map(|route| route.original).collect(),
            deactivate_filter: true,
            remove_owned_links: true,
        }
    }
}

pub fn route_health(plan: &RoutePlan, graph: &GraphState) -> RouteHealth {
    let replacements = plan.insertion().stage;
    if replacements
        .iter()
        .all(|spec| graph.contains_link(spec.output.port_id, spec.input.port_id))
    {
        return RouteHealth::Healthy;
    }

    let has_new_direct_route = graph.links().any(|link| {
        link.output_node_id != plan.filter_node_id
            && link.input_node_id != plan.filter_node_id
            && (link.output_node_id == plan.stream_node_id
                || link.input_node_id == plan.stream_node_id)
    });
    if has_new_direct_route {
        return RouteHealth::Superseded;
    }

    let original_endpoints_present = plan.channels.iter().all(|channel| {
        graph.contains_port(
            channel.original.output.node_id,
            channel.original.output.port_id,
            PortDirection::Output,
        ) && graph.contains_port(
            channel.original.input.node_id,
            channel.original.input.port_id,
            PortDirection::Input,
        )
    });
    if original_endpoints_present {
        RouteHealth::Broken
    } else {
        RouteHealth::EndpointsGone
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoutePlanError {
    NoStreamPorts,
    MissingChannel {
        port_id: u32,
    },
    DuplicateStreamChannel {
        channel: String,
    },
    MissingRoute {
        port_id: u32,
    },
    AmbiguousRoute {
        port_id: u32,
    },
    MissingFilterPort {
        channel: String,
        direction: PortDirection,
    },
    DuplicateFilterPort {
        channel: String,
        direction: PortDirection,
    },
}

impl fmt::Display for RoutePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoStreamPorts => formatter.write_str("stream has no audio ports"),
            Self::MissingChannel { port_id } => {
                write!(formatter, "port {port_id} has no audio channel label")
            }
            Self::DuplicateStreamChannel { channel } => {
                write!(formatter, "stream has duplicate audio channel {channel}")
            }
            Self::MissingRoute { port_id } => {
                write!(formatter, "port {port_id} has no direct route")
            }
            Self::AmbiguousRoute { port_id } => {
                write!(formatter, "port {port_id} has more than one direct route")
            }
            Self::MissingFilterPort { channel, direction } => write!(
                formatter,
                "filter has no {} port for channel {channel}",
                direction_name(*direction)
            ),
            Self::DuplicateFilterPort { channel, direction } => write!(
                formatter,
                "filter has duplicate {} ports for channel {channel}",
                direction_name(*direction)
            ),
        }
    }
}

impl std::error::Error for RoutePlanError {}

fn direction_name(direction: PortDirection) -> &'static str {
    match direction {
        PortDirection::Input => "input",
        PortDirection::Output => "output",
    }
}

pub fn plan_route(
    domain: SignalDomain,
    stream_node_id: u32,
    filter_node_id: u32,
    ports: &[DiscoveredPort],
    links: &[DiscoveredLink],
) -> Result<RoutePlan, RoutePlanError> {
    let stream_direction = match domain {
        SignalDomain::Playback => PortDirection::Output,
        SignalDomain::Capture => PortDirection::Input,
    };
    let mut stream_ports: Vec<_> = ports
        .iter()
        .filter(|port| port.node_id == stream_node_id && port.direction == stream_direction)
        .collect();
    if stream_ports.is_empty() {
        return Err(RoutePlanError::NoStreamPorts);
    }
    stream_ports.sort_by_key(|port| port.port_id);

    let filter_ports = index_filter_ports(filter_node_id, ports)?;
    let mut channels = HashSet::new();
    let mut routes = Vec::with_capacity(stream_ports.len());
    for stream_port in stream_ports {
        let channel = stream_port
            .channel
            .as_deref()
            .ok_or(RoutePlanError::MissingChannel {
                port_id: stream_port.port_id,
            })?;
        if !channels.insert(channel) {
            return Err(RoutePlanError::DuplicateStreamChannel {
                channel: channel.to_owned(),
            });
        }

        let connected: Vec<_> = links
            .iter()
            .filter(|link| match domain {
                SignalDomain::Playback => link.output_port_id == stream_port.port_id,
                SignalDomain::Capture => link.input_port_id == stream_port.port_id,
            })
            .collect();
        let original = match connected.as_slice() {
            [] => {
                return Err(RoutePlanError::MissingRoute {
                    port_id: stream_port.port_id,
                });
            }
            [link] => *link,
            _ => {
                return Err(RoutePlanError::AmbiguousRoute {
                    port_id: stream_port.port_id,
                });
            }
        };
        let filter_input = filter_port(&filter_ports, channel, PortDirection::Input)?;
        let filter_output = filter_port(&filter_ports, channel, PortDirection::Output)?;
        let original_spec = LinkSpec {
            output: LinkEndpoint {
                node_id: original.output_node_id,
                port_id: original.output_port_id,
            },
            input: LinkEndpoint {
                node_id: original.input_node_id,
                port_id: original.input_port_id,
            },
        };
        routes.push(ChannelRoute {
            channel: channel.to_owned(),
            original_link_id: original.link_id,
            original: original_spec,
            into_filter: LinkSpec {
                output: original_spec.output,
                input: LinkEndpoint {
                    node_id: filter_node_id,
                    port_id: filter_input.port_id,
                },
            },
            out_of_filter: LinkSpec {
                output: LinkEndpoint {
                    node_id: filter_node_id,
                    port_id: filter_output.port_id,
                },
                input: original_spec.input,
            },
        });
    }

    Ok(RoutePlan {
        stream_node_id,
        filter_node_id,
        channels: routes,
    })
}

type FilterPortIndex<'a> = HashMap<(&'a str, PortDirection), &'a DiscoveredPort>;

fn index_filter_ports(
    filter_node_id: u32,
    ports: &[DiscoveredPort],
) -> Result<FilterPortIndex<'_>, RoutePlanError> {
    let mut indexed = HashMap::new();
    for port in ports.iter().filter(|port| port.node_id == filter_node_id) {
        let Some(channel) = port.channel.as_deref() else {
            return Err(RoutePlanError::MissingChannel {
                port_id: port.port_id,
            });
        };
        if indexed.insert((channel, port.direction), port).is_some() {
            return Err(RoutePlanError::DuplicateFilterPort {
                channel: channel.to_owned(),
                direction: port.direction,
            });
        }
    }
    Ok(indexed)
}

fn filter_port<'a>(
    ports: &FilterPortIndex<'a>,
    channel: &str,
    direction: PortDirection,
) -> Result<&'a DiscoveredPort, RoutePlanError> {
    ports
        .get(&(channel, direction))
        .copied()
        .ok_or_else(|| RoutePlanError::MissingFilterPort {
            channel: channel.to_owned(),
            direction,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(id: u32, node: u32, direction: PortDirection, channel: &str) -> DiscoveredPort {
        DiscoveredPort {
            port_id: id,
            node_id: node,
            direction,
            name: None,
            channel: Some(channel.to_owned()),
            format_dsp: None,
        }
    }

    fn link(
        id: u32,
        output_node: u32,
        output_port: u32,
        input_node: u32,
        input_port: u32,
    ) -> DiscoveredLink {
        DiscoveredLink {
            link_id: id,
            output_node_id: output_node,
            output_port_id: output_port,
            input_node_id: input_node,
            input_port_id: input_port,
        }
    }

    fn graph_with(objects: impl IntoIterator<Item = DiscoveredLink>) -> GraphState {
        let mut graph = GraphState::default();
        for link in objects {
            graph.insert(crate::pipewire_backend::GraphObject::Link(link));
        }
        graph
    }

    #[test]
    fn plans_playback_between_application_and_sink() {
        let ports = [
            port(11, 10, PortDirection::Output, "FL"),
            port(21, 20, PortDirection::Input, "FL"),
            port(31, 30, PortDirection::Input, "FL"),
            port(32, 30, PortDirection::Output, "FL"),
        ];
        let plan = plan_route(
            SignalDomain::Playback,
            10,
            30,
            &ports,
            &[link(40, 10, 11, 20, 21)],
        )
        .unwrap();

        assert_eq!(plan.channels[0].original_link_id, 40);
        assert_eq!(
            plan.channels[0].into_filter,
            LinkSpec {
                output: LinkEndpoint {
                    node_id: 10,
                    port_id: 11
                },
                input: LinkEndpoint {
                    node_id: 30,
                    port_id: 31
                },
            }
        );
        assert_eq!(
            plan.channels[0].out_of_filter,
            LinkSpec {
                output: LinkEndpoint {
                    node_id: 30,
                    port_id: 32
                },
                input: LinkEndpoint {
                    node_id: 20,
                    port_id: 21
                },
            }
        );
    }

    #[test]
    fn reports_route_health_from_live_link_topology() {
        let plan = RoutePlan {
            stream_node_id: 10,
            filter_node_id: 30,
            channels: vec![ChannelRoute {
                channel: "FL".to_owned(),
                original_link_id: 40,
                original: LinkSpec {
                    output: LinkEndpoint {
                        node_id: 10,
                        port_id: 11,
                    },
                    input: LinkEndpoint {
                        node_id: 20,
                        port_id: 21,
                    },
                },
                into_filter: LinkSpec {
                    output: LinkEndpoint {
                        node_id: 10,
                        port_id: 11,
                    },
                    input: LinkEndpoint {
                        node_id: 30,
                        port_id: 31,
                    },
                },
                out_of_filter: LinkSpec {
                    output: LinkEndpoint {
                        node_id: 30,
                        port_id: 32,
                    },
                    input: LinkEndpoint {
                        node_id: 20,
                        port_id: 21,
                    },
                },
            }],
        };
        let mut healthy = GraphState::default();
        for discovered_port in [
            port(11, 10, PortDirection::Output, "FL"),
            port(21, 20, PortDirection::Input, "FL"),
            port(31, 30, PortDirection::Input, "FL"),
            port(32, 30, PortDirection::Output, "FL"),
        ] {
            healthy.insert(crate::pipewire_backend::GraphObject::Port(discovered_port));
        }
        for discovered_link in [link(41, 10, 11, 30, 31), link(42, 30, 32, 20, 21)] {
            healthy.insert(crate::pipewire_backend::GraphObject::Link(discovered_link));
        }
        assert_eq!(route_health(&plan, &healthy), RouteHealth::Healthy);

        healthy.remove_id(21);
        assert_eq!(route_health(&plan, &healthy), RouteHealth::EndpointsGone);
        healthy.insert(crate::pipewire_backend::GraphObject::Port(port(
            21,
            99,
            PortDirection::Input,
            "FL",
        )));
        assert_eq!(route_health(&plan, &healthy), RouteHealth::EndpointsGone);

        let superseded = graph_with([link(43, 10, 11, 50, 51)]);
        assert_eq!(route_health(&plan, &superseded), RouteHealth::Superseded);

        let mut broken = GraphState::default();
        broken.insert(crate::pipewire_backend::GraphObject::Port(port(
            11,
            10,
            PortDirection::Output,
            "FL",
        )));
        broken.insert(crate::pipewire_backend::GraphObject::Port(port(
            21,
            20,
            PortDirection::Input,
            "FL",
        )));
        assert_eq!(route_health(&plan, &broken), RouteHealth::Broken);

        assert_eq!(
            route_health(&plan, &GraphState::default()),
            RouteHealth::EndpointsGone
        );
    }

    #[test]
    fn plans_capture_between_source_and_application() {
        let ports = [
            port(11, 10, PortDirection::Output, "FR"),
            port(21, 20, PortDirection::Input, "FR"),
            port(31, 30, PortDirection::Input, "FR"),
            port(32, 30, PortDirection::Output, "FR"),
        ];
        let plan = plan_route(
            SignalDomain::Capture,
            20,
            30,
            &ports,
            &[link(40, 10, 11, 20, 21)],
        )
        .unwrap();

        assert_eq!(plan.channels[0].into_filter.output.node_id, 10);
        assert_eq!(plan.channels[0].out_of_filter.input.node_id, 20);
    }

    #[test]
    fn rejects_ambiguous_routes_without_mutation() {
        let ports = [
            port(11, 10, PortDirection::Output, "FL"),
            port(31, 30, PortDirection::Input, "FL"),
            port(32, 30, PortDirection::Output, "FL"),
        ];
        let error = plan_route(
            SignalDomain::Playback,
            10,
            30,
            &ports,
            &[link(40, 10, 11, 20, 21), link(41, 10, 11, 22, 23)],
        )
        .unwrap_err();

        assert_eq!(error, RoutePlanError::AmbiguousRoute { port_id: 11 });
    }

    #[test]
    fn rejects_incomplete_filter_channel_pairs() {
        let ports = [
            port(11, 10, PortDirection::Output, "FL"),
            port(31, 30, PortDirection::Input, "FL"),
        ];
        let error = plan_route(
            SignalDomain::Playback,
            10,
            30,
            &ports,
            &[link(40, 10, 11, 20, 21)],
        )
        .unwrap_err();

        assert_eq!(
            error,
            RoutePlanError::MissingFilterPort {
                channel: "FL".to_owned(),
                direction: PortDirection::Output,
            }
        );
    }

    #[test]
    fn route_errors_have_stable_human_readable_messages() {
        assert_eq!(
            RoutePlanError::AmbiguousRoute { port_id: 11 }.to_string(),
            "port 11 has more than one direct route"
        );
        assert_eq!(
            RoutePlanError::MissingFilterPort {
                channel: "FL".to_owned(),
                direction: PortDirection::Input,
            }
            .to_string(),
            "filter has no input port for channel FL"
        );
    }

    #[test]
    fn insertion_stages_every_filter_link_before_cutover() {
        let ports = [
            port(11, 10, PortDirection::Output, "FL"),
            port(12, 10, PortDirection::Output, "FR"),
            port(31, 30, PortDirection::Input, "FL"),
            port(32, 30, PortDirection::Output, "FL"),
            port(33, 30, PortDirection::Input, "FR"),
            port(34, 30, PortDirection::Output, "FR"),
        ];
        let plan = plan_route(
            SignalDomain::Playback,
            10,
            30,
            &ports,
            &[link(40, 10, 11, 20, 21), link(41, 10, 12, 20, 22)],
        )
        .unwrap();

        let transition = plan.insertion();
        assert_eq!(transition.stage.len(), 4);
        assert_eq!(transition.cutover.len(), 2);
        assert!(transition.activate_filter);
        assert_eq!(transition.cutover[0].link_id, 40);
        assert_eq!(transition.cutover[1].link_id, 41);
    }

    #[test]
    fn teardown_restores_originals_before_removing_owned_graph() {
        let ports = [
            port(11, 10, PortDirection::Output, "FL"),
            port(31, 30, PortDirection::Input, "FL"),
            port(32, 30, PortDirection::Output, "FL"),
        ];
        let plan = plan_route(
            SignalDomain::Playback,
            10,
            30,
            &ports,
            &[link(40, 10, 11, 20, 21)],
        )
        .unwrap();

        let teardown = plan.teardown();
        assert_eq!(teardown.restore, vec![plan.channels[0].original]);
        assert!(teardown.deactivate_filter);
        assert!(teardown.remove_owned_links);
    }
}
