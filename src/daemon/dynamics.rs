// SPDX-License-Identifier: AGPL-3.0-or-later

use std::time::Instant;

use crate::{
    normalization::{ControllerBank, SignalDomain},
    pipewire::filter::MeterSnapshot,
};

pub(super) struct DynamicsCheckpoint {
    pub(super) gain_db: f32,
    pub(super) sequence: u64,
    recorded_at: Instant,
    hold_seconds: f32,
}

impl DynamicsCheckpoint {
    pub(super) fn new(snapshot: MeterSnapshot, now: Instant) -> Self {
        Self {
            gain_db: snapshot.applied_gain_db,
            sequence: snapshot.fast_cut_sequence,
            recorded_at: now,
            hold_seconds: snapshot.fast_cut_hold_seconds,
        }
    }

    pub(super) fn remaining_hold(&self, now: Instant) -> f32 {
        (f64::from(self.hold_seconds)
            - now
                .saturating_duration_since(self.recorded_at)
                .as_secs_f64())
        .max(0.0) as f32
    }
}

pub(super) fn settings_unchanged(
    current: &ControllerBank,
    next: &ControllerBank,
    domain: SignalDomain,
    application: &str,
) -> bool {
    current.config(domain) == next.config(domain)
        && current.policy_for(application).enables(domain)
            == next.policy_for(application).enables(domain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalization::{ApplicationPolicy, UserConfig};
    use std::time::Duration;

    #[test]
    fn an_unrelated_capture_policy_change_preserves_playback_filters() {
        let current = ControllerBank::defaults();
        let mut next = ControllerBank::defaults();
        next.set_policy(
            "player",
            ApplicationPolicy {
                normalize_playback: true,
                normalize_capture: false,
            },
        );
        assert!(settings_unchanged(
            &current,
            &next,
            SignalDomain::Playback,
            "player"
        ));
        assert!(!settings_unchanged(
            &current,
            &next,
            SignalDomain::Capture,
            "player"
        ));
        assert!(settings_unchanged(
            &current,
            &next,
            SignalDomain::Capture,
            "other"
        ));
    }

    #[test]
    fn a_directional_config_change_replaces_only_its_own_filters() {
        let current = ControllerBank::defaults();
        let mut next = ControllerBank::defaults();
        next.apply_user_config(UserConfig::from_toml("[capture]\ntarget_lufs = -20.0\n").unwrap())
            .unwrap();
        assert!(settings_unchanged(
            &current,
            &next,
            SignalDomain::Playback,
            "player"
        ));
        assert!(!settings_unchanged(
            &current,
            &next,
            SignalDomain::Capture,
            "player"
        ));
    }

    #[test]
    fn replacement_hold_is_aged_instead_of_starting_another_full_second() {
        let start = Instant::now();
        let checkpoint = DynamicsCheckpoint::new(
            MeterSnapshot {
                sequence: 10,
                source_loudness_lufs: -4.0,
                source_momentary_lufs: Some(-4.0),
                source_true_peak_dbtp: None,
                output_loudness_lufs: Some(-16.0),
                output_true_peak_dbtp: None,
                limiter_reduction_db: 0.0,
                maximum_limiter_reduction_db: 0.0,
                applied_gain_db: -12.0,
                fast_cut_hold_seconds: 0.75,
                fast_cut_sequence: 42,
            },
            start,
        );
        assert_eq!(
            checkpoint.remaining_hold(start + Duration::from_millis(500)),
            0.25
        );
        assert_eq!(
            checkpoint.remaining_hold(start + Duration::from_secs(2)),
            0.0
        );
        assert_eq!(checkpoint.gain_db, -12.0);
        assert_eq!(checkpoint.sequence, 42);
    }
}
