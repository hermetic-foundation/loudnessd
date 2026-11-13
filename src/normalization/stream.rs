// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{ControllerBank, Decision, Observation, SignalDomain};
use crate::pipewire::filter::{ConnectedFilter, MeterSnapshot};

const MAX_OBSERVATION_SECONDS: f32 = 0.1;

pub trait NormalizationEndpoint {
    fn latest_meter_snapshot(&self) -> Option<MeterSnapshot>;
    fn set_target_gain_db(&self, target_gain_db: f32) -> Result<(), &'static str>;
}

impl NormalizationEndpoint for ConnectedFilter {
    fn latest_meter_snapshot(&self) -> Option<MeterSnapshot> {
        self.latest_meter_snapshot()
    }

    fn set_target_gain_db(&self, target_gain_db: f32) -> Result<(), &'static str> {
        self.set_target_gain_db(target_gain_db)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlUpdate {
    pub meter: MeterSnapshot,
    pub decision: Decision,
}

#[derive(Debug)]
pub struct StreamControl {
    domain: SignalDomain,
    application_id: String,
    stream_id: String,
    last_sequence: Option<u64>,
    last_update: Option<ControlUpdate>,
    last_fast_cut_sequence: u64,
}

impl StreamControl {
    pub fn new(
        domain: SignalDomain,
        application_id: impl Into<String>,
        stream_id: impl Into<String>,
    ) -> Self {
        Self {
            domain,
            application_id: application_id.into(),
            stream_id: stream_id.into(),
            last_sequence: None,
            last_update: None,
            last_fast_cut_sequence: 0,
        }
    }

    pub fn domain(&self) -> SignalDomain {
        self.domain
    }

    pub fn application_id(&self) -> &str {
        &self.application_id
    }

    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    pub fn last_update(&self) -> Option<ControlUpdate> {
        self.last_update
    }

    pub fn update<E: NormalizationEndpoint>(
        &mut self,
        endpoint: &E,
        controllers: &mut ControllerBank,
        elapsed_seconds: f32,
    ) -> Result<Option<ControlUpdate>, &'static str> {
        let snapshot = endpoint.latest_meter_snapshot();
        if snapshot.is_none_or(|snapshot| self.last_sequence == Some(snapshot.sequence)) {
            // Age the history during silence or a paused callback without
            // treating the last finite measurement as fresh audio.
            controllers.age_stream_history(self.domain, &self.stream_id, elapsed_seconds);
            return Ok(None);
        }
        let snapshot = snapshot.expect("a fresh snapshot was checked above");
        let observed_seconds =
            if elapsed_seconds.is_finite() && elapsed_seconds > MAX_OBSERVATION_SECONDS {
                // Unknown audio ages history but cannot authorize gain recovery.
                controllers.age_stream_history(
                    self.domain,
                    &self.stream_id,
                    elapsed_seconds - MAX_OBSERVATION_SECONDS,
                );
                MAX_OBSERVATION_SECONDS
            } else {
                elapsed_seconds
            };
        if snapshot.fast_cut_sequence != self.last_fast_cut_sequence {
            controllers.adopt_gain_reduction(
                self.domain,
                &self.stream_id,
                snapshot.applied_gain_db,
            );
            self.last_fast_cut_sequence = snapshot.fast_cut_sequence;
        }

        let decision = controllers.observe(
            self.domain,
            &self.application_id,
            &self.stream_id,
            Observation {
                lufs: snapshot.source_momentary_lufs.unwrap_or(f32::NEG_INFINITY),
                elapsed_seconds: observed_seconds,
            },
        );
        endpoint.set_target_gain_db(decision.target_gain_db())?;
        self.last_sequence = Some(snapshot.sequence);
        let update = ControlUpdate {
            meter: snapshot,
            decision,
        };
        self.last_update = Some(update);
        Ok(Some(update))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::normalization::ApplicationPolicy;

    #[derive(Default)]
    struct FakeEndpoint {
        snapshot: Cell<Option<MeterSnapshot>>,
        target_gain_db: Cell<f32>,
    }

    impl NormalizationEndpoint for FakeEndpoint {
        fn latest_meter_snapshot(&self) -> Option<MeterSnapshot> {
            self.snapshot.get()
        }

        fn set_target_gain_db(&self, target_gain_db: f32) -> Result<(), &'static str> {
            self.target_gain_db.set(target_gain_db);
            Ok(())
        }
    }

    fn snapshot(sequence: u64, source_loudness_lufs: f32) -> MeterSnapshot {
        MeterSnapshot {
            sequence,
            source_loudness_lufs,
            source_momentary_lufs: Some(source_loudness_lufs),
            source_true_peak_dbtp: Some(-3.0),
            output_loudness_lufs: Some(-13.0),
            output_true_peak_dbtp: Some(-1.0),
            limiter_reduction_db: 0.0,
            maximum_limiter_reduction_db: 0.0,
            applied_gain_db: 0.0,
            fast_cut_hold_seconds: 0.0,
            fast_cut_sequence: 0,
        }
    }

    #[test]
    fn applies_each_meter_sequence_once() {
        let endpoint = FakeEndpoint::default();
        endpoint.snapshot.set(Some(snapshot(1, -23.0)));
        let mut control = StreamControl::new(SignalDomain::Playback, "player", "stream-1");
        let mut controllers = ControllerBank::defaults();

        let update = control
            .update(&endpoint, &mut controllers, 1.0)
            .unwrap()
            .unwrap();

        assert_eq!(update.meter.sequence, 1);
        assert_eq!(update.meter.source_loudness_lufs, -23.0);
        assert_eq!(update.meter.output_loudness_lufs, Some(-13.0));
        assert!((endpoint.target_gain_db.get() - 0.02).abs() < 0.0001);
        assert_eq!(control.domain(), SignalDomain::Playback);
        assert_eq!(control.application_id(), "player");
        assert_eq!(control.stream_id(), "stream-1");
        assert_eq!(control.last_update(), Some(update));
        assert_eq!(
            control.update(&endpoint, &mut controllers, 1.0).unwrap(),
            None
        );
        let state = controllers
            .stream(SignalDomain::Playback, "stream-1")
            .unwrap();
        assert!((state.gain_db - 0.02).abs() < 0.0001);
    }

    #[test]
    fn bypassed_direction_restores_unity_gain() {
        let endpoint = FakeEndpoint {
            snapshot: Cell::new(Some(snapshot(1, -23.0))),
            target_gain_db: Cell::new(6.0),
        };
        let mut control = StreamControl::new(SignalDomain::Capture, "player", "stream-1");
        let mut controllers = ControllerBank::defaults();
        controllers.set_policy(
            "player",
            ApplicationPolicy {
                normalize_playback: true,
                normalize_capture: false,
            },
        );

        let update = control
            .update(&endpoint, &mut controllers, 1.0)
            .unwrap()
            .unwrap();

        assert_eq!(update.decision, Decision::Bypass);
        assert_eq!(endpoint.target_gain_db.get(), 0.0);
        assert_eq!(controllers.stream(SignalDomain::Capture, "stream-1"), None);
    }

    #[test]
    fn inherits_each_fast_cut_once_and_recovers_without_a_jump() {
        let endpoint = FakeEndpoint::default();
        let mut control = StreamControl::new(SignalDomain::Playback, "player", "stream");
        let mut controllers = ControllerBank::defaults();
        controllers.observe(
            SignalDomain::Playback,
            "player",
            "stream",
            Observation {
                lufs: -30.0,
                elapsed_seconds: 30.0,
            },
        );
        let mut reading = snapshot(1, -30.0);
        reading.applied_gain_db = -12.0;
        reading.fast_cut_sequence = 1;
        reading.fast_cut_hold_seconds = 1.0;
        endpoint.snapshot.set(Some(reading));

        control.update(&endpoint, &mut controllers, 0.1).unwrap();
        assert!((endpoint.target_gain_db.get() - -11.98).abs() < 0.0001);
        assert_eq!(controllers.stream(SignalDomain::Capture, "stream"), None);

        reading.sequence = 2;
        endpoint.snapshot.set(Some(reading));
        control.update(&endpoint, &mut controllers, 0.1).unwrap();
        assert!((endpoint.target_gain_db.get() - -11.96).abs() < 0.0001);
    }

    #[test]
    fn inherits_a_cut_even_if_its_hold_expired_before_the_main_loop_resumes() {
        let endpoint = FakeEndpoint::default();
        let mut control = StreamControl::new(SignalDomain::Playback, "player", "stream");
        let mut controllers = ControllerBank::defaults();
        controllers.observe(
            SignalDomain::Playback,
            "player",
            "stream",
            Observation {
                lufs: -30.0,
                elapsed_seconds: 30.0,
            },
        );
        let mut reading = snapshot(1, -30.0);
        reading.applied_gain_db = -11.6;
        reading.fast_cut_sequence = 1;
        reading.fast_cut_hold_seconds = 0.0;
        endpoint.snapshot.set(Some(reading));
        control.update(&endpoint, &mut controllers, 0.1).unwrap();
        assert!((endpoint.target_gain_db.get() - -11.58).abs() < 0.0001);
    }

    #[test]
    fn history_uses_momentary_loudness_not_the_older_short_term_value() {
        let endpoint = FakeEndpoint::default();
        let mut control = StreamControl::new(SignalDomain::Playback, "player", "stream");
        let mut controllers = ControllerBank::defaults();
        let mut reading = snapshot(1, -30.0);
        reading.source_momentary_lufs = Some(-4.0);
        endpoint.snapshot.set(Some(reading));
        control.update(&endpoint, &mut controllers, 0.1).unwrap();
        assert_eq!(endpoint.target_gain_db.get(), -12.0);
    }

    #[test]
    fn a_stale_snapshot_expires_history_without_replaying_audio() {
        let endpoint = FakeEndpoint::default();
        let mut control = StreamControl::new(SignalDomain::Playback, "player", "stream");
        let mut controllers = ControllerBank::defaults();
        endpoint.snapshot.set(Some(snapshot(1, -4.0)));
        control.update(&endpoint, &mut controllers, 0.1).unwrap();
        assert_eq!(
            controllers.reference_lufs(SignalDomain::Playback, "stream"),
            Some(-4.0)
        );
        let previous_state = controllers.stream(SignalDomain::Playback, "stream");
        assert_eq!(
            control.update(&endpoint, &mut controllers, 30.0).unwrap(),
            None
        );
        assert_eq!(
            controllers.reference_lufs(SignalDomain::Playback, "stream"),
            None
        );
        assert_eq!(
            controllers.stream(SignalDomain::Playback, "stream"),
            previous_state
        );
        assert_eq!(endpoint.target_gain_db.get(), -12.0);
    }

    #[test]
    fn absent_snapshots_only_age_existing_history_without_changing_gain() {
        let endpoint = FakeEndpoint::default();
        let mut control = StreamControl::new(SignalDomain::Playback, "player", "stream");
        let mut controllers = ControllerBank::defaults();

        assert_eq!(
            control.update(&endpoint, &mut controllers, 30.0).unwrap(),
            None
        );
        assert_eq!(controllers.stream(SignalDomain::Playback, "stream"), None);

        controllers.observe(
            SignalDomain::Playback,
            "player",
            "stream",
            Observation {
                lufs: -30.0,
                elapsed_seconds: 30.0,
            },
        );
        let previous_state = controllers.stream(SignalDomain::Playback, "stream");
        assert_eq!(
            control.update(&endpoint, &mut controllers, 30.0).unwrap(),
            None
        );
        assert_eq!(
            controllers.reference_lufs(SignalDomain::Playback, "stream"),
            None
        );
        assert_eq!(
            controllers.stream(SignalDomain::Playback, "stream"),
            previous_state
        );
        assert_eq!(endpoint.target_gain_db.get(), 0.0);
        assert_eq!(control.last_update(), None);
    }

    #[test]
    fn a_stale_snapshot_does_not_recreate_a_removed_stream() {
        let endpoint = FakeEndpoint::default();
        let mut control = StreamControl::new(SignalDomain::Playback, "player", "stream");
        let mut controllers = ControllerBank::defaults();
        endpoint.snapshot.set(Some(snapshot(1, -4.0)));
        control.update(&endpoint, &mut controllers, 0.1).unwrap();
        controllers.retain_streams(|_, _| false);

        assert_eq!(
            control.update(&endpoint, &mut controllers, 30.0).unwrap(),
            None
        );
        assert_eq!(controllers.stream(SignalDomain::Playback, "stream"), None);
        assert_eq!(endpoint.target_gain_db.get(), -12.0);
    }

    #[test]
    fn fresh_snapshots_only_authorize_recovery_for_observed_time() {
        for (elapsed_seconds, expected_gain_db) in [(0.05, 0.01), (0.1, 0.02), (10.0, 0.02)] {
            let endpoint = FakeEndpoint::default();
            let mut control = StreamControl::new(SignalDomain::Playback, "player", "stream");
            let mut controllers = ControllerBank::defaults();
            endpoint.snapshot.set(Some(snapshot(1, -30.0)));

            control
                .update(&endpoint, &mut controllers, elapsed_seconds)
                .unwrap();
            assert!((endpoint.target_gain_db.get() - expected_gain_db).abs() < 0.0001);
            assert_eq!(
                controllers.reference_lufs(SignalDomain::Playback, "stream"),
                Some(-30.0)
            );
        }
    }

    #[test]
    fn a_fresh_snapshot_ages_the_full_unobserved_gap() {
        let endpoint = FakeEndpoint::default();
        let mut control = StreamControl::new(SignalDomain::Playback, "player", "stream");
        let mut controllers = ControllerBank::defaults();
        controllers.observe(
            SignalDomain::Playback,
            "player",
            "stream",
            Observation {
                lufs: -4.0,
                elapsed_seconds: 30.0,
            },
        );
        endpoint.snapshot.set(Some(snapshot(1, -30.0)));

        control.update(&endpoint, &mut controllers, 10.0).unwrap();
        assert_eq!(
            controllers.reference_lufs(SignalDomain::Playback, "stream"),
            Some(-4.0)
        );
        assert_eq!(
            control.update(&endpoint, &mut controllers, 20.1).unwrap(),
            None
        );
        assert_eq!(
            controllers.reference_lufs(SignalDomain::Playback, "stream"),
            Some(-30.0)
        );
        assert_eq!(endpoint.target_gain_db.get(), -12.0);
        assert_eq!(
            control.update(&endpoint, &mut controllers, 10.1).unwrap(),
            None
        );
        assert_eq!(
            controllers.reference_lufs(SignalDomain::Playback, "stream"),
            None
        );
    }

    #[test]
    fn invalid_elapsed_time_does_not_become_a_valid_observation() {
        for elapsed_seconds in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let endpoint = FakeEndpoint::default();
            let mut control = StreamControl::new(SignalDomain::Playback, "player", "stream");
            let mut controllers = ControllerBank::defaults();
            endpoint.snapshot.set(Some(snapshot(1, -30.0)));

            let update = control
                .update(&endpoint, &mut controllers, elapsed_seconds)
                .unwrap()
                .unwrap();
            assert_eq!(update.decision, Decision::Silence { gain_db: 0.0 });
            assert_eq!(endpoint.target_gain_db.get(), 0.0);
            assert_eq!(
                controllers.reference_lufs(SignalDomain::Playback, "stream"),
                None
            );
        }
    }

    #[test]
    fn a_delayed_short_loud_sample_keeps_quiet_p90_and_adopts_the_fast_cut() {
        for hold_seconds in [1.0, 0.0] {
            let endpoint = FakeEndpoint::default();
            let mut control = StreamControl::new(SignalDomain::Playback, "player", "stream");
            let mut controllers = ControllerBank::defaults();
            for sequence in 1..=300 {
                endpoint.snapshot.set(Some(snapshot(sequence, -30.0)));
                control.update(&endpoint, &mut controllers, 0.1).unwrap();
            }

            let mut reading = snapshot(301, -4.0);
            reading.applied_gain_db = -12.0;
            reading.fast_cut_sequence = 1;
            reading.fast_cut_hold_seconds = hold_seconds;
            endpoint.snapshot.set(Some(reading));

            let update = control
                .update(&endpoint, &mut controllers, 10.0)
                .unwrap()
                .unwrap();
            assert_eq!(
                controllers.reference_lufs(SignalDomain::Playback, "stream"),
                Some(-30.0)
            );
            assert!(matches!(
                update.decision,
                Decision::Adjust {
                    previous_db: -12.0,
                    ..
                }
            ));
            assert!((endpoint.target_gain_db.get() - -11.98).abs() < 0.0001);
            assert_eq!(controllers.stream(SignalDomain::Capture, "stream"), None);

            reading.sequence += 1;
            reading.source_momentary_lufs = Some(-30.0);
            endpoint.snapshot.set(Some(reading));
            control.update(&endpoint, &mut controllers, 0.1).unwrap();
            assert!((endpoint.target_gain_db.get() - -11.96).abs() < 0.0001);
        }
    }

    fn simulate_fast_cut_handover(delay_main_loop: bool) {
        use crate::normalization::dsp::fast_cut::{FastCut, FastCutConfig};

        let config = crate::normalization::ControllerConfig::default();
        let mut dynamics = FastCut::new(FastCutConfig {
            target_lufs: config.target_lufs,
            silence_gate_lufs: config.silence_gate_lufs,
            deadband_lu: config.deadband_lu,
            maximum_boost_db: config.maximum_boost_db,
            maximum_cut_db: config.maximum_cut_db,
            boost_rate_db_per_second: config.boost_rate_db_per_second,
            hold_seconds: config.fast_cut_hold_seconds,
        })
        .unwrap();
        let endpoint = FakeEndpoint::default();
        let mut control = StreamControl::new(SignalDomain::Playback, "player", "stream");
        let mut controllers = ControllerBank::defaults();
        let mut previous_gain = 0.0;
        let mut main_elapsed = 0.0;
        for frame in 0..15_000 {
            let source = if (2_000..2_020).contains(&frame) {
                -4.0
            } else {
                -34.0
            };
            let actual_gain = dynamics.process(endpoint.target_gain_db.get(), Some(source), 0.01);
            assert!(
                actual_gain <= previous_gain + 0.00201,
                "upward jump at frame {frame}: {previous_gain} -> {actual_gain}"
            );
            assert!(actual_gain <= 6.0);
            if (2_000..2_100).contains(&frame) {
                assert!(
                    (actual_gain - -12.0).abs() < 0.0001,
                    "one-second hold released at frame {frame}: {actual_gain}"
                );
            }
            let mut reading = snapshot(frame + 1, source);
            reading.applied_gain_db = actual_gain;
            reading.fast_cut_sequence = dynamics.reduction_sequence();
            reading.fast_cut_hold_seconds = dynamics.remaining_hold_seconds();
            endpoint.snapshot.set(Some(reading));
            main_elapsed += 0.01;
            let delayed = delay_main_loop && (2_000..2_400).contains(&frame);
            if (frame + 1) % 10 == 0 && !delayed {
                control
                    .update(&endpoint, &mut controllers, main_elapsed)
                    .unwrap();
                if delay_main_loop && frame == 2_409 {
                    assert!((endpoint.target_gain_db.get() - actual_gain - 0.02).abs() < 0.0001);
                }
                main_elapsed = 0.0;
            }
            previous_gain = actual_gain;
        }
        assert!((dynamics.gain_db() - 6.0).abs() < 0.001);
        assert_eq!(
            controllers.reference_lufs(SignalDomain::Playback, "stream"),
            Some(-34.0)
        );
        assert_eq!(controllers.stream(SignalDomain::Capture, "stream"), None);
    }

    #[test]
    fn fast_cut_and_percentile_controller_recover_together_over_simulated_minutes() {
        simulate_fast_cut_handover(false);
    }

    #[test]
    fn delayed_main_loop_never_undoes_the_audio_path_cut() {
        simulate_fast_cut_handover(true);
    }
}
