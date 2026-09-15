// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::routing::{LinkSpec, OriginalLink, RoutePlan};

pub trait RouteBackend {
    type LinkSet;
    type Error;

    /// Create all links and confirm that PipeWire accepted them. On error, no
    /// created links may remain.
    fn create_links(&mut self, specs: &[LinkSpec]) -> Result<Self::LinkSet, Self::Error>;

    /// Remove all original links. On error, any links already removed by this
    /// call must be restored before returning.
    fn remove_originals(&mut self, links: &[OriginalLink]) -> Result<(), Self::Error>;

    /// Restore direct links and retain their ownership after this call. This is
    /// used during failed installation, where no ActiveRoute can own them.
    fn restore_originals(&mut self, specs: &[LinkSpec]) -> Result<(), Self::Error>;

    fn set_filter_active(&mut self, filter_node_id: u32, active: bool) -> Result<(), Self::Error>;

    fn destroy_links(&mut self, links: Self::LinkSet);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionOperation {
    StageReplacement,
    RemoveOriginal,
    ActivateFilter,
    RestoreOriginal,
    DeactivateFilter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransitionError<E> {
    pub operation: TransitionOperation,
    pub error: E,
    pub rollback_error: Option<E>,
}

pub struct ActiveRoute<L> {
    plan: RoutePlan,
    replacements: L,
}

pub struct BypassedRoute<L> {
    pub plan: RoutePlan,
    pub direct_links: L,
}

pub struct BypassError<L, E> {
    pub route: ActiveRoute<L>,
    pub transition: TransitionError<E>,
}

impl<L> ActiveRoute<L> {
    pub fn plan(&self) -> &RoutePlan {
        &self.plan
    }
}

pub fn install<B: RouteBackend>(
    backend: &mut B,
    plan: RoutePlan,
) -> Result<ActiveRoute<B::LinkSet>, TransitionError<B::Error>> {
    let transition = plan.insertion();
    let replacements =
        backend
            .create_links(&transition.stage)
            .map_err(|error| TransitionError {
                operation: TransitionOperation::StageReplacement,
                error,
                rollback_error: None,
            })?;

    if let Err(error) = backend.remove_originals(&transition.cutover) {
        backend.destroy_links(replacements);
        return Err(TransitionError {
            operation: TransitionOperation::RemoveOriginal,
            error,
            rollback_error: None,
        });
    }

    if let Err(error) = backend.set_filter_active(plan.filter_node_id, true) {
        let originals: Vec<_> = transition.cutover.iter().map(|link| link.spec).collect();
        let rollback = backend.restore_originals(&originals);
        backend.destroy_links(replacements);
        return match rollback {
            Ok(()) => Err(TransitionError {
                operation: TransitionOperation::ActivateFilter,
                error,
                rollback_error: None,
            }),
            Err(rollback_error) => Err(TransitionError {
                operation: TransitionOperation::ActivateFilter,
                error,
                rollback_error: Some(rollback_error),
            }),
        };
    }

    Ok(ActiveRoute { plan, replacements })
}

pub fn bypass<B: RouteBackend<LinkSet = L>, L>(
    backend: &mut B,
    route: ActiveRoute<L>,
) -> Result<BypassedRoute<L>, BypassError<L, B::Error>> {
    let teardown = route.plan.teardown();
    let direct_links = match backend.create_links(&teardown.restore) {
        Ok(links) => links,
        Err(error) => {
            return Err(BypassError {
                route,
                transition: TransitionError {
                    operation: TransitionOperation::RestoreOriginal,
                    error,
                    rollback_error: None,
                },
            });
        }
    };
    if let Err(error) = backend.set_filter_active(route.plan.filter_node_id, false) {
        backend.destroy_links(direct_links);
        return Err(BypassError {
            route,
            transition: TransitionError {
                operation: TransitionOperation::DeactivateFilter,
                error,
                rollback_error: None,
            },
        });
    }
    backend.destroy_links(route.replacements);
    Ok(BypassedRoute {
        plan: route.plan,
        direct_links,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SignalDomain,
        pipewire_backend::{DiscoveredLink, DiscoveredPort, PortDirection},
        routing::plan_route,
    };

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Failure {
        Create(usize),
        Remove,
        Activate,
        Deactivate,
    }

    #[derive(Default)]
    struct FakeBackend {
        failure: Option<Failure>,
        creates: usize,
        events: Vec<&'static str>,
    }

    impl RouteBackend for FakeBackend {
        type LinkSet = usize;
        type Error = Failure;

        fn create_links(&mut self, _specs: &[LinkSpec]) -> Result<Self::LinkSet, Self::Error> {
            self.creates += 1;
            self.events.push("create");
            if self.failure == Some(Failure::Create(self.creates)) {
                Err(Failure::Create(self.creates))
            } else {
                Ok(self.creates)
            }
        }

        fn remove_originals(&mut self, _links: &[OriginalLink]) -> Result<(), Self::Error> {
            self.events.push("remove-original");
            if self.failure == Some(Failure::Remove) {
                Err(Failure::Remove)
            } else {
                Ok(())
            }
        }

        fn restore_originals(&mut self, _specs: &[LinkSpec]) -> Result<(), Self::Error> {
            self.creates += 1;
            self.events.push("restore");
            if self.failure == Some(Failure::Create(self.creates)) {
                Err(Failure::Create(self.creates))
            } else {
                Ok(())
            }
        }

        fn set_filter_active(
            &mut self,
            _filter_node_id: u32,
            active: bool,
        ) -> Result<(), Self::Error> {
            self.events
                .push(if active { "activate" } else { "deactivate" });
            match (active, self.failure) {
                (true, Some(Failure::Activate)) => Err(Failure::Activate),
                (false, Some(Failure::Deactivate)) => Err(Failure::Deactivate),
                _ => Ok(()),
            }
        }

        fn destroy_links(&mut self, _links: Self::LinkSet) {
            self.events.push("destroy");
        }
    }

    fn plan() -> RoutePlan {
        let ports = [
            DiscoveredPort {
                port_id: 11,
                node_id: 10,
                direction: PortDirection::Output,
                name: None,
                channel: Some("FL".to_owned()),
                format_dsp: None,
            },
            DiscoveredPort {
                port_id: 31,
                node_id: 30,
                direction: PortDirection::Input,
                name: None,
                channel: Some("FL".to_owned()),
                format_dsp: None,
            },
            DiscoveredPort {
                port_id: 32,
                node_id: 30,
                direction: PortDirection::Output,
                name: None,
                channel: Some("FL".to_owned()),
                format_dsp: None,
            },
        ];
        plan_route(
            SignalDomain::Playback,
            10,
            30,
            &ports,
            &[DiscoveredLink {
                link_id: 40,
                output_node_id: 10,
                output_port_id: 11,
                input_node_id: 20,
                input_port_id: 21,
            }],
        )
        .unwrap()
    }

    #[test]
    fn installs_only_after_staging_and_cutover() {
        let mut backend = FakeBackend::default();
        let route = install(&mut backend, plan()).unwrap();

        assert_eq!(route.plan().filter_node_id, 30);
        assert_eq!(backend.events, ["create", "remove-original", "activate"]);
    }

    #[test]
    fn failed_cutover_releases_replacements() {
        let mut backend = FakeBackend {
            failure: Some(Failure::Remove),
            ..Default::default()
        };
        let error = install(&mut backend, plan()).err().unwrap();

        assert_eq!(error.operation, TransitionOperation::RemoveOriginal);
        assert_eq!(backend.events, ["create", "remove-original", "destroy"]);
    }

    #[test]
    fn failed_activation_restores_original_before_releasing_replacements() {
        let mut backend = FakeBackend {
            failure: Some(Failure::Activate),
            ..Default::default()
        };
        let error = install(&mut backend, plan()).err().unwrap();

        assert_eq!(error.operation, TransitionOperation::ActivateFilter);
        assert_eq!(
            backend.events,
            [
                "create",
                "remove-original",
                "activate",
                "restore",
                "destroy"
            ]
        );
    }

    #[test]
    fn bypass_restores_direct_route_before_deactivating_filter() {
        let mut backend = FakeBackend::default();
        let route = install(&mut backend, plan()).unwrap();
        backend.events.clear();

        let bypassed = bypass(&mut backend, route).ok().unwrap();

        assert_eq!(bypassed.direct_links, 2);
        assert_eq!(backend.events, ["create", "deactivate", "destroy"]);
    }

    #[test]
    fn failed_deactivation_keeps_the_active_route_owned() {
        let mut backend = FakeBackend::default();
        let route = install(&mut backend, plan()).unwrap();
        backend.events.clear();
        backend.failure = Some(Failure::Deactivate);

        let error = bypass(&mut backend, route).err().unwrap();

        assert_eq!(
            error.transition.operation,
            TransitionOperation::DeactivateFilter
        );
        assert_eq!(error.route.plan().filter_node_id, 30);
        assert_eq!(backend.events, ["create", "deactivate", "destroy"]);
    }
}
