// SPDX-License-Identifier: AGPL-3.0-or-later

use std::time::{Duration, Instant};

use pipewire::loop_::Timeout;

use super::{Daemon, ManagedStream};
use crate::{
    pipewire_backend::{GraphState, PortDirection},
    pipewire_links::OwnedLinks,
    routing::{LinkSpec, RoutePlan},
};

impl Daemon {
    pub(super) fn prune_retained_direct_links(&mut self) {
        let graph = self.graph.borrow();
        self.retained_direct_links
            .retain(|links| links.ids().any(|id| graph.contains_link_id(id)));
    }

    fn direct_specs(&self, extra: Option<&RoutePlan>) -> Vec<LinkSpec> {
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

    pub(super) fn sync_recovery_journal(&self, extra: Option<&RoutePlan>) -> bool {
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

    pub(super) fn recover_prior_routes(&mut self) {
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

        let deadline = Instant::now() + Duration::from_secs(2);
        while !recovery_endpoints_present(&self.graph.borrow(), &specs) && Instant::now() < deadline
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
                let deadline = Instant::now() + Duration::from_secs(2);
                while !missing.iter().all(|spec| {
                    self.graph
                        .borrow()
                        .contains_link(spec.output.port_id, spec.input.port_id)
                }) && Instant::now() < deadline
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
}

fn recovery_endpoints_present(graph: &GraphState, specs: &[LinkSpec]) -> bool {
    specs.iter().all(|spec| {
        graph.contains_port(
            spec.output.node_id,
            spec.output.port_id,
            PortDirection::Output,
        ) && graph.contains_port(spec.input.node_id, spec.input.port_id, PortDirection::Input)
    })
}
