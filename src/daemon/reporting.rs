// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashMap;

use crate::{
    normalization::{ControllerBank, ControllerConfig, Decision, SignalDomain},
    pipewire::{
        filter::FilterState,
        graph::GraphState,
        routing::{RouteHealth, route_health},
    },
    process_metrics,
    status::{
        ControlStatus, DaemonStatus, RouteStatus, SkippedStreamStatus, StreamLifecycle,
        StreamStatus,
    },
};

use super::ManagedStream;

pub(super) fn snapshot(
    enabled: bool,
    controllers: &ControllerBank,
    managed: &HashMap<u32, ManagedStream>,
    skipped: &HashMap<u32, SkippedStreamStatus>,
    graph: &GraphState,
) -> DaemonStatus {
    let mut streams: Vec<_> = managed
        .iter()
        .map(|(node_id, managed)| stream_status(*node_id, managed, controllers, graph))
        .collect();
    streams.sort_by_key(|stream| stream.node_id);
    let mut skipped_streams: Vec<_> = skipped.values().cloned().collect();
    skipped_streams.sort_by_key(|stream| stream.node_id);

    DaemonStatus {
        enabled,
        managed: streams.len(),
        active: streams
            .iter()
            .filter(|stream| stream.lifecycle == StreamLifecycle::Active)
            .count(),
        skipped: skipped_streams.len(),
        process: process_metrics::read(),
        streams,
        skipped_streams,
    }
}

fn stream_status(
    node_id: u32,
    managed: &ManagedStream,
    controllers: &ControllerBank,
    graph: &GraphState,
) -> StreamStatus {
    let (filter_node_id, filter_state, filter_error, lifecycle, route, control) = match managed {
        ManagedStream::Connecting {
            filter, control, ..
        } => {
            let (state, error) = filter.state();
            (
                filter.node_id(),
                filter_state_name(state),
                error,
                StreamLifecycle::Connecting,
                RouteStatus::Connecting,
                control,
            )
        }
        ManagedStream::Active {
            filter,
            control,
            route,
            ..
        } => {
            let (state, error) = filter.state();
            (
                filter.node_id(),
                filter_state_name(state),
                error,
                StreamLifecycle::Active,
                route_status(route_health(route.plan(), graph)),
                control,
            )
        }
    };
    let update = control.last_update();
    let meter = update.map(|update| update.meter);
    let controller_config = controllers.config(control.domain());
    let gain_db = update
        .map(|update| update.decision.target_gain_db())
        .unwrap_or(0.0);
    let gain_clamped = gain_is_limited(
        controller_config,
        gain_db,
        meter.map(|meter| meter.source_loudness_lufs),
    );

    StreamStatus {
        node_id,
        filter_node_id,
        filter_state: Some(filter_state),
        filter_error,
        domain: domain_name(control.domain()).to_owned(),
        application: control.application_id().to_owned(),
        meter_sequence: meter.map(|meter| meter.sequence),
        lifecycle,
        route,
        control: update
            .map(|update| {
                control_status(
                    update.decision,
                    update.meter.output_loudness_lufs,
                    controller_config,
                    gain_clamped,
                )
            })
            .unwrap_or(ControlStatus::Waiting),
        target_lufs: controller_config.target_lufs,
        source_lufs: meter.map(|meter| meter.source_loudness_lufs),
        source_peak_dbtp: meter.and_then(|meter| meter.source_true_peak_dbtp),
        output_lufs: meter.and_then(|meter| meter.output_loudness_lufs),
        output_peak_dbtp: meter.and_then(|meter| meter.output_true_peak_dbtp),
        gain_db,
        gain_clamped,
        limiter_db: meter.map(|meter| meter.limiter_reduction_db).unwrap_or(0.0),
        limiter_max_db: meter
            .map(|meter| meter.maximum_limiter_reduction_db)
            .unwrap_or(0.0),
    }
}

fn filter_state_name(state: FilterState) -> String {
    match state {
        FilterState::Error => "error".to_owned(),
        FilterState::Unconnected => "unconnected".to_owned(),
        FilterState::Connecting => "connecting".to_owned(),
        FilterState::Paused => "paused".to_owned(),
        FilterState::Streaming => "streaming".to_owned(),
        FilterState::Unknown(raw) => format!("unknown({raw})"),
    }
}

fn gain_is_limited(config: ControllerConfig, gain_db: f32, source_lufs: Option<f32>) -> bool {
    const TOLERANCE_DB: f32 = 0.001;
    (gain_db - config.maximum_boost_db).abs() < TOLERANCE_DB
        || (gain_db + config.maximum_cut_db).abs() < TOLERANCE_DB
        || source_lufs.is_some_and(|source_lufs| {
            let required_gain_db = config.target_lufs - source_lufs;
            required_gain_db > config.maximum_boost_db + TOLERANCE_DB
                || required_gain_db < -config.maximum_cut_db - TOLERANCE_DB
        })
}

fn control_status(
    decision: Decision,
    output_lufs: Option<f32>,
    config: ControllerConfig,
    gain_clamped: bool,
) -> ControlStatus {
    match decision {
        Decision::Bypass => ControlStatus::Bypass,
        Decision::Silence { .. } => ControlStatus::Silence,
        Decision::Hold { .. }
            if !gain_clamped
                && output_lufs.is_some_and(|output| {
                    (output - config.target_lufs).abs() > config.deadband_lu
                }) =>
        {
            ControlStatus::Converging
        }
        Decision::Hold { .. } => ControlStatus::Settled,
        Decision::Adjust { .. } => ControlStatus::Converging,
    }
}

fn route_status(health: RouteHealth) -> RouteStatus {
    match health {
        RouteHealth::Healthy => RouteStatus::Healthy,
        RouteHealth::Superseded => RouteStatus::Superseded,
        RouteHealth::EndpointsGone => RouteStatus::Broken,
        RouteHealth::Broken => RouteStatus::Broken,
    }
}

fn domain_name(domain: SignalDomain) -> &'static str {
    match domain {
        SignalDomain::Playback => "playback",
        SignalDomain::Capture => "capture",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_gain_limited_before_slew_reaches_the_limit() {
        let config = ControllerConfig::default();

        assert!(gain_is_limited(config, 3.0, Some(-40.0)));
        assert!(gain_is_limited(
            config,
            config.maximum_boost_db,
            Some(-20.0)
        ));
        assert!(!gain_is_limited(config, 3.0, Some(-16.0)));
        assert!(!gain_is_limited(config, 0.0, None));
    }

    #[test]
    fn reports_post_filter_meter_settling_after_gain_stops() {
        let config = ControllerConfig::default();
        let hold = Decision::Hold { gain_db: 4.0 };

        assert_eq!(
            control_status(hold, Some(-12.0), config, false),
            ControlStatus::Converging
        );
        assert_eq!(
            control_status(hold, Some(-15.5), config, false),
            ControlStatus::Settled
        );
        assert_eq!(
            control_status(hold, Some(-12.0), config, true),
            ControlStatus::Settled
        );
    }
}
