// SPDX-License-Identifier: AGPL-3.0-or-later

//! Allocation-free loudness gain envelope, independent of the peak limiter.
//! Source readings must describe momentary loudness before applying this gain.
//! This is loudness control, not hearing protection.

// Ignore meter/rounding noise when deciding whether a new cut earns a hold.
const CUT_EPSILON_DB: f32 = 0.0001;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FastCutConfig {
    pub target_lufs: f32,
    pub silence_gate_lufs: f32,
    pub deadband_lu: f32,
    /// Non-negative configured boost limit.
    pub maximum_boost_db: f32,
    /// Non-negative cut magnitude, e.g. 24 for a -24 dB floor.
    pub maximum_cut_db: f32,
    /// Non-negative configured recovery rate in dB/s.
    pub boost_rate_db_per_second: f32,
    /// Non-negative duration of a newly triggered source cut's ceiling.
    pub hold_seconds: f32,
}

#[derive(Debug)]
pub struct FastCut {
    config: FastCutConfig,
    // Wider accumulators avoid per-buffer rounding stalls and timer drift.
    gain_db: f64,
    held_ceiling_db: Option<f64>,
    remaining_hold_seconds: f64,
    reduction_sequence: u64,
}

impl FastCut {
    pub fn new(config: FastCutConfig) -> Result<Self, &'static str> {
        let values = [
            config.target_lufs,
            config.silence_gate_lufs,
            config.deadband_lu,
            config.maximum_boost_db,
            config.maximum_cut_db,
            config.boost_rate_db_per_second,
            config.hold_seconds,
        ];
        if values.iter().any(|value| !value.is_finite()) {
            return Err("all fast-cut values must be finite");
        }
        if config.silence_gate_lufs >= config.target_lufs {
            return Err("silence gate must be below the target loudness");
        }
        if config.deadband_lu < 0.0
            || config.maximum_boost_db < 0.0
            || config.maximum_cut_db < 0.0
            || config.boost_rate_db_per_second < 0.0
            || config.hold_seconds < 0.0
        {
            return Err("fast-cut limits, recovery rate, and hold duration must be non-negative");
        }
        Ok(Self {
            config,
            gain_db: 0.0,
            held_ceiling_db: None,
            remaining_hold_seconds: 0.0,
            reduction_sequence: 0,
        })
    }

    /// Returns the actual gain, applying requested reductions immediately.
    ///
    /// Elapsed processed-audio time ages the previous hold. A new source cut
    /// starts a full hold at this call; it does not age by that same interval.
    /// Only time beyond an existing hold may recover above its ceiling.
    /// Recovery below that ceiling is still rate-limited.
    ///
    /// Invalid elapsed time is zero. Non-finite requests preserve the current
    /// gain, but do not suppress source cuts. Missing, non-finite, or gated
    /// source readings never generate boost requests; explicit requested gains
    /// can still recover at the configured rate without a source reading.
    pub fn process(
        &mut self,
        requested_gain_db: f32,
        source_lufs: Option<f32>,
        elapsed_seconds: f32,
    ) -> f32 {
        let elapsed_seconds = if elapsed_seconds.is_finite() && elapsed_seconds > 0.0 {
            f64::from(elapsed_seconds)
        } else {
            0.0
        };
        let held_seconds = elapsed_seconds.min(self.remaining_hold_seconds);
        let previous_ceiling_db = self.held_ceiling_db;
        self.remaining_hold_seconds = (self.remaining_hold_seconds - elapsed_seconds).max(0.0);
        if self.remaining_hold_seconds == 0.0 {
            self.held_ceiling_db = None;
        }

        let minimum_gain_db = -f64::from(self.config.maximum_cut_db);
        let maximum_gain_db = f64::from(self.config.maximum_boost_db);
        let requested_gain_db = if requested_gain_db.is_finite() {
            f64::from(requested_gain_db).clamp(minimum_gain_db, maximum_gain_db)
        } else {
            self.gain_db
        };
        self.gain_db = self.gain_db.min(requested_gain_db);

        let source_lufs =
            source_lufs.filter(|lufs| lufs.is_finite() && *lufs >= self.config.silence_gate_lufs);
        let source_ceiling_db = source_lufs.map(|lufs| {
            (f64::from(self.config.target_lufs) - f64::from(lufs))
                .clamp(minimum_gain_db, maximum_gain_db)
        });
        if let (Some(source_lufs), Some(source_ceiling_db)) = (source_lufs, source_ceiling_db) {
            let above_deadband = f64::from(source_lufs) + self.gain_db
                > f64::from(self.config.target_lufs) + f64::from(self.config.deadband_lu);
            if above_deadband && self.gain_db - source_ceiling_db > f64::from(CUT_EPSILON_DB) {
                self.gain_db = source_ceiling_db;
                self.held_ceiling_db = Some(source_ceiling_db);
                self.remaining_hold_seconds = f64::from(self.config.hold_seconds);
                self.reduction_sequence = self.reduction_sequence.wrapping_add(1);
                return self.gain_db();
            }
        }

        // Do not rebound into a sustained loud source as its hold expires.
        // This ceiling only limits recovery; it cannot cut within the deadband.
        let recovery_target_db = source_ceiling_db.map_or(requested_gain_db, |ceiling_db| {
            requested_gain_db.min(ceiling_db.max(self.gain_db))
        });
        if let Some(ceiling_db) = previous_ceiling_db {
            self.recover_toward(recovery_target_db.min(ceiling_db), held_seconds);
        }
        self.recover_toward(recovery_target_db, elapsed_seconds - held_seconds);
        self.gain_db()
    }

    /// Restores unity gain and clears both hold and reduction history.
    pub fn reset(&mut self) {
        self.gain_db = 0.0;
        self.held_ceiling_db = None;
        self.remaining_hold_seconds = 0.0;
        self.reduction_sequence = 0;
    }

    pub fn gain_db(&self) -> f32 {
        self.gain_db as f32
    }

    pub fn restore(
        &mut self,
        gain_db: f32,
        remaining_hold_seconds: f32,
        reduction_sequence: u64,
    ) -> Result<(), &'static str> {
        if !gain_db.is_finite()
            || !remaining_hold_seconds.is_finite()
            || remaining_hold_seconds < 0.0
        {
            return Err("restored gain must be finite and hold must be finite and non-negative");
        }
        // Preserve an existing deeper cut across a config reload rather than
        // jumping upward to a newly narrowed cut limit.
        self.gain_db = f64::from(gain_db.min(self.config.maximum_boost_db));
        self.remaining_hold_seconds =
            f64::from(remaining_hold_seconds.min(self.config.hold_seconds));
        self.held_ceiling_db = (self.remaining_hold_seconds > 0.0).then_some(self.gain_db);
        self.reduction_sequence = reduction_sequence;
        Ok(())
    }

    pub fn remaining_hold_seconds(&self) -> f32 {
        self.remaining_hold_seconds as f32
    }

    /// Wrapping history of source-triggered reductions, retained after expiry.
    /// Ordinary main-controller requests and recovery never increment it.
    pub fn reduction_sequence(&self) -> u64 {
        self.reduction_sequence
    }

    fn recover_toward(&mut self, target_gain_db: f64, elapsed_seconds: f64) {
        let maximum_step = f64::from(self.config.boost_rate_db_per_second) * elapsed_seconds;
        self.gain_db += (target_gain_db - self.gain_db).max(0.0).min(maximum_step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> FastCutConfig {
        FastCutConfig {
            target_lufs: -16.0,
            silence_gate_lufs: -50.0,
            deadband_lu: 0.75,
            maximum_boost_db: 6.0,
            maximum_cut_db: 24.0,
            boost_rate_db_per_second: 0.2,
            hold_seconds: 1.0,
        }
    }

    fn envelope() -> FastCut {
        FastCut::new(config()).unwrap()
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.00001,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn starts_at_unity_and_cuts_immediately_to_the_absolute_source_target() {
        let mut fast_cut = envelope();
        assert_eq!(fast_cut.gain_db(), 0.0);
        assert_eq!(fast_cut.remaining_hold_seconds(), 0.0);
        assert_eq!(fast_cut.reduction_sequence(), 0);
        assert_eq!(fast_cut.process(6.0, Some(-4.0), 0.01), -12.0);
        assert_eq!(fast_cut.remaining_hold_seconds(), 1.0);
        assert_eq!(fast_cut.reduction_sequence(), 1);
    }

    #[test]
    fn restored_filter_preserves_its_cut_hold_and_handover_generation() {
        let mut replacement = envelope();
        replacement.restore(-12.0, 0.5, 42).unwrap();
        assert_eq!(replacement.process(6.0, Some(-40.0), 0.1), -12.0);
        assert_eq!(replacement.reduction_sequence(), 42);
        assert_close(replacement.remaining_hold_seconds(), 0.4);
        replacement.process(6.0, Some(-40.0), 0.4);
        assert_close(replacement.process(6.0, Some(-40.0), 0.1), -11.98);
    }

    #[test]
    fn a_restored_deeper_cut_recovers_slowly_instead_of_clamping_upward() {
        let mut replacement = envelope();
        replacement.restore(-30.0, 0.0, 1).unwrap();
        assert_close(replacement.process(6.0, Some(-40.0), 0.1), -29.98);
        let before = replacement.gain_db();
        assert!(replacement.restore(f32::NAN, 0.0, 2).is_err());
        assert!(replacement.restore(0.0, -1.0, 2).is_err());
        assert_eq!(replacement.gain_db(), before);
    }

    #[test]
    fn considers_previously_applied_gain_even_when_the_source_is_below_target() {
        let mut fast_cut = envelope();
        assert_eq!(fast_cut.process(6.0, Some(-40.0), 30.0), 6.0);
        assert_eq!(fast_cut.process(6.0, Some(-18.0), 0.01), 2.0);
        assert_eq!(fast_cut.reduction_sequence(), 1);

        let mut already_cut = envelope();
        assert_eq!(already_cut.process(-20.0, None, 0.0), -20.0);
        assert_eq!(already_cut.process(-20.0, Some(-4.0), 0.01), -20.0);
        assert_eq!(already_cut.reduction_sequence(), 0);
    }

    #[test]
    fn respects_the_deadband_without_cutting_or_boosting_to_the_source_target() {
        let mut fast_cut = envelope();
        assert_eq!(fast_cut.process(0.0, Some(-16.0), 0.1), 0.0);
        assert_eq!(fast_cut.process(0.0, Some(-15.25), 0.1), 0.0);
        assert_eq!(fast_cut.process(0.0, Some(-15.5), 0.1), 0.0);
        assert_eq!(fast_cut.reduction_sequence(), 0);
        assert_close(fast_cut.process(0.0, Some(-15.24), 0.1), -0.76);
        assert_eq!(fast_cut.reduction_sequence(), 1);
    }

    #[test]
    fn holds_for_exactly_one_second_then_recovers_without_a_jump() {
        let mut fast_cut = envelope();
        assert_eq!(fast_cut.process(0.0, Some(-4.0), 0.0), -12.0);
        for remaining in [0.75, 0.5, 0.25, 0.0] {
            assert_eq!(fast_cut.process(0.0, Some(-40.0), 0.25), -12.0);
            assert_eq!(fast_cut.remaining_hold_seconds(), remaining);
        }
        assert_close(fast_cut.process(0.0, Some(-40.0), 0.1), -11.98);
        assert_eq!(fast_cut.reduction_sequence(), 1);
    }

    #[test]
    fn crossing_expiry_uses_only_the_time_after_the_hold_for_recovery() {
        let mut fast_cut = envelope();
        fast_cut.process(0.0, Some(-4.0), 0.0);
        assert_eq!(fast_cut.process(0.0, None, 0.75), -12.0);
        assert_close(fast_cut.process(0.0, None, 0.5), -11.95);
        assert_eq!(fast_cut.remaining_hold_seconds(), 0.0);
    }

    #[test]
    fn equal_or_weaker_readings_do_not_extend_the_strongest_hold() {
        let mut fast_cut = envelope();
        fast_cut.process(6.0, Some(-4.0), 0.0);
        assert_eq!(fast_cut.process(6.0, Some(-4.0), 0.25), -12.0);
        assert_eq!(fast_cut.remaining_hold_seconds(), 0.75);
        assert_eq!(fast_cut.process(6.0, Some(-8.0), 0.25), -12.0);
        assert_eq!(fast_cut.remaining_hold_seconds(), 0.5);
        assert_eq!(fast_cut.process(6.0, Some(-40.0), 0.5), -12.0);
        assert_eq!(fast_cut.remaining_hold_seconds(), 0.0);
        assert_eq!(fast_cut.reduction_sequence(), 1);
    }

    #[test]
    fn overlapping_stronger_cuts_restart_a_full_hold() {
        let mut fast_cut = envelope();
        fast_cut.process(6.0, Some(-8.0), 0.0);
        assert_eq!(fast_cut.process(6.0, Some(-40.0), 0.75), -8.0);
        assert_eq!(fast_cut.process(6.0, Some(-2.0), 0.125), -14.0);
        assert_eq!(fast_cut.remaining_hold_seconds(), 1.0);
        assert_eq!(fast_cut.reduction_sequence(), 2);
        assert_eq!(fast_cut.process(6.0, Some(-6.0), 0.75), -14.0);
        assert_eq!(fast_cut.remaining_hold_seconds(), 0.25);
        assert_eq!(fast_cut.process(6.0, None, 0.25), -14.0);
        assert_eq!(fast_cut.remaining_hold_seconds(), 0.0);
        assert_close(fast_cut.process(6.0, None, 0.1), -13.98);
    }

    #[test]
    fn ordinary_requests_cannot_escape_the_held_ceiling_but_can_cut_further() {
        let mut fast_cut = envelope();
        fast_cut.process(0.0, Some(-4.0), 0.0);
        assert_eq!(fast_cut.process(6.0, Some(-40.0), 0.25), -12.0);
        assert_eq!(fast_cut.process(-14.0, Some(-40.0), 0.25), -14.0);
        assert_close(fast_cut.process(6.0, Some(-40.0), 0.25), -13.95);
        assert_eq!(fast_cut.remaining_hold_seconds(), 0.25);
        assert_eq!(fast_cut.reduction_sequence(), 1);
    }

    #[test]
    fn held_ceiling_limits_recovery_even_with_a_faster_configured_rate() {
        let mut settings = config();
        settings.hold_seconds = 100.0;
        settings.boost_rate_db_per_second = 100.0;
        let mut fast_cut = FastCut::new(settings).unwrap();
        fast_cut.process(0.0, Some(-4.0), 0.0);
        fast_cut.process(-12.25, None, 0.0);
        assert_eq!(fast_cut.process(6.0, None, 50.0), -12.0);
        assert_eq!(fast_cut.remaining_hold_seconds(), 50.0);
        assert_eq!(fast_cut.reduction_sequence(), 1);
    }

    #[test]
    fn a_sustained_loud_source_never_rebounds_or_retriggers_after_expiry() {
        let mut fast_cut = envelope();
        fast_cut.process(6.0, Some(-4.0), 0.0);
        for _ in 0..2_000 {
            assert_eq!(fast_cut.process(6.0, Some(-4.0), 0.01), -12.0);
        }
        assert_eq!(fast_cut.remaining_hold_seconds(), 0.0);
        assert_eq!(fast_cut.reduction_sequence(), 1);
        assert_close(fast_cut.process(6.0, Some(-40.0), 4.0), -11.2);
        assert_eq!(fast_cut.process(6.0, Some(-4.0), 0.1), -12.0);
        assert_eq!(fast_cut.remaining_hold_seconds(), 1.0);
        assert_eq!(fast_cut.reduction_sequence(), 2);
    }

    #[test]
    fn tiny_source_variations_do_not_extend_the_hold_or_increment_the_sequence() {
        let mut settings = config();
        settings.deadband_lu = 0.0;
        let mut fast_cut = FastCut::new(settings).unwrap();
        fast_cut.process(0.0, Some(-4.0), 0.0);
        for _ in 0..4 {
            assert_eq!(fast_cut.process(0.0, Some(-3.99999), 0.25), -12.0);
        }
        assert_eq!(fast_cut.remaining_hold_seconds(), 0.0);
        assert_eq!(fast_cut.reduction_sequence(), 1);
        assert_close(fast_cut.process(0.0, Some(-3.999), 0.01), -12.001);
        assert_eq!(fast_cut.remaining_hold_seconds(), 1.0);
        assert_eq!(fast_cut.reduction_sequence(), 2);
    }

    #[test]
    fn delayed_main_controller_can_inherit_actual_gain_after_the_hold_expires() {
        let mut fast_cut = envelope();
        let previous_sequence = fast_cut.reduction_sequence();
        let stale_request = 6.0;
        fast_cut.process(stale_request, Some(-4.0), 0.0);
        assert_close(fast_cut.process(stale_request, None, 1.5), -11.9);
        assert_eq!(fast_cut.remaining_hold_seconds(), 0.0);
        assert_ne!(fast_cut.reduction_sequence(), previous_sequence);

        let inherited_gain_db = fast_cut.gain_db();
        let adopted_sequence = fast_cut.reduction_sequence();
        assert_eq!(
            fast_cut.process(inherited_gain_db, None, 0.1),
            inherited_gain_db
        );
        assert_close(
            fast_cut.process(inherited_gain_db + 0.02, None, 0.1),
            -11.88,
        );
        assert_eq!(fast_cut.reduction_sequence(), adopted_sequence);
    }

    #[test]
    fn ordinary_requested_changes_never_increment_the_reduction_sequence() {
        let mut fast_cut = envelope();
        assert_eq!(fast_cut.process(-10.0, Some(-4.0), 0.0), -12.0);
        let sequence = fast_cut.reduction_sequence();
        assert_eq!(fast_cut.process(-18.0, Some(-4.0), 0.25), -18.0);
        assert_eq!(fast_cut.process(-24.0, None, 0.25), -24.0);
        assert_close(fast_cut.process(-22.0, None, 0.25), -23.95);
        assert_eq!(fast_cut.reduction_sequence(), sequence);

        let mut ordinary = envelope();
        ordinary.process(-20.0, Some(-4.0), 0.0);
        ordinary.process(-24.0, None, 0.0);
        ordinary.process(6.0, Some(-40.0), 0.1);
        assert_eq!(ordinary.reduction_sequence(), 0);
        assert_eq!(ordinary.remaining_hold_seconds(), 0.0);
    }

    #[test]
    fn reduction_sequence_wraps_only_on_a_source_cut() {
        let mut fast_cut = envelope();
        fast_cut.reduction_sequence = u64::MAX;
        fast_cut.process(-2.0, None, 0.0);
        assert_eq!(fast_cut.reduction_sequence(), u64::MAX);
        fast_cut.process(0.0, Some(-4.0), 0.0);
        assert_eq!(fast_cut.reduction_sequence(), 0);
    }

    #[test]
    fn requested_rises_are_slow_and_cut_requests_are_immediate() {
        let mut fast_cut = envelope();
        assert_close(fast_cut.process(6.0, Some(-40.0), 0.1), 0.02);
        assert_eq!(fast_cut.process(-10.0, Some(-40.0), 0.0), -10.0);
        assert_close(fast_cut.process(6.0, Some(-40.0), 0.1), -9.98);
        assert_eq!(fast_cut.process(-100.0, None, 0.0), -24.0);
        assert_eq!(fast_cut.reduction_sequence(), 0);
    }

    #[test]
    fn quiet_loud_transitions_respect_boost_and_direction_specific_cut_bounds() {
        for maximum_cut_db in [24.0, 18.0] {
            let mut settings = config();
            settings.maximum_cut_db = maximum_cut_db;
            let mut fast_cut = FastCut::new(settings).unwrap();
            assert_eq!(fast_cut.process(100.0, Some(-40.0), 100.0), 6.0);
            assert_eq!(fast_cut.process(100.0, Some(20.0), 0.01), -maximum_cut_db);
            assert_eq!(fast_cut.process(100.0, Some(20.0), 1.0), -maximum_cut_db);
            assert_eq!(fast_cut.remaining_hold_seconds(), 0.0);
            assert_eq!(fast_cut.reduction_sequence(), 1);
            assert_close(
                fast_cut.process(100.0, Some(-40.0), 0.1),
                -maximum_cut_db + 0.02,
            );
        }
    }

    #[test]
    fn independent_playback_and_capture_envelopes_keep_their_own_configuration() {
        let mut playback = envelope();
        let mut capture_config = config();
        capture_config.target_lufs = -18.0;
        capture_config.silence_gate_lufs = -55.0;
        capture_config.deadband_lu = 1.0;
        capture_config.maximum_cut_db = 18.0;
        let mut capture = FastCut::new(capture_config).unwrap();
        assert_eq!(playback.process(0.0, Some(-4.0), 0.0), -12.0);
        assert_eq!(capture.process(0.0, Some(-4.0), 0.0), -14.0);
        assert_eq!(playback.process(0.0, Some(20.0), 0.0), -24.0);
        assert_eq!(capture.process(0.0, Some(20.0), 0.0), -18.0);
        playback.reset();
        assert_eq!(capture.gain_db(), -18.0);
        assert_eq!(capture.remaining_hold_seconds(), 1.0);
        assert_eq!(capture.reduction_sequence(), 2);
    }

    #[test]
    fn default_config_keeps_six_db_boost_and_point_two_db_per_second_recovery() {
        let mut fast_cut = envelope();
        assert_close(fast_cut.process(100.0, Some(-40.0), 1.0), 0.2);
        assert_eq!(fast_cut.process(100.0, Some(-40.0), 100.0), 6.0);
    }

    #[test]
    fn configured_boost_and_recovery_limits_honor_larger_and_stricter_values() {
        let mut settings = config();
        settings.maximum_boost_db = 18.0;
        settings.boost_rate_db_per_second = 3.0;
        let mut fast_cut = FastCut::new(settings).unwrap();
        assert_eq!(fast_cut.process(100.0, Some(-40.0), 1.0), 3.0);
        assert_eq!(fast_cut.process(100.0, Some(-40.0), 100.0), 18.0);
        assert_eq!(fast_cut.process(100.0, Some(-4.0), 0.01), -12.0);
        assert_eq!(fast_cut.process(100.0, Some(-40.0), 1.0), -12.0);
        assert_eq!(fast_cut.process(100.0, Some(-40.0), 1.0), -9.0);
        assert_eq!(fast_cut.reduction_sequence(), 1);

        settings.maximum_boost_db = 2.0;
        settings.boost_rate_db_per_second = 0.1;
        let mut stricter = FastCut::new(settings).unwrap();
        assert_close(stricter.process(18.0, Some(-40.0), 1.0), 0.1);
        assert_eq!(stricter.process(18.0, Some(-40.0), 100.0), 2.0);
    }

    #[test]
    fn source_readings_never_generate_automatic_boost_requests() {
        for source in [
            None,
            Some(-51.0),
            Some(-50.0),
            Some(-40.0),
            Some(f32::NAN),
            Some(f32::INFINITY),
            Some(f32::NEG_INFINITY),
        ] {
            let mut fast_cut = envelope();
            assert_eq!(fast_cut.process(0.0, source, 100.0), 0.0);
            assert_eq!(fast_cut.process(-10.0, source, 0.0), -10.0);
            assert_eq!(fast_cut.process(-10.0, source, 100.0), -10.0);
            assert_eq!(fast_cut.reduction_sequence(), 0);
            assert_close(fast_cut.process(6.0, source, 0.1), -9.98);
        }
    }

    #[test]
    fn invalid_elapsed_time_neither_releases_nor_ages_a_hold_but_allows_cuts() {
        for elapsed in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut fast_cut = envelope();
            assert_eq!(fast_cut.process(6.0, None, elapsed), 0.0);
            assert_eq!(fast_cut.process(6.0, Some(-4.0), elapsed), -12.0);
            assert_eq!(fast_cut.process(6.0, None, elapsed), -12.0);
            assert_eq!(fast_cut.remaining_hold_seconds(), 1.0);
            assert_eq!(fast_cut.process(-20.0, None, elapsed), -20.0);
            assert_eq!(fast_cut.remaining_hold_seconds(), 1.0);
            assert_eq!(fast_cut.reduction_sequence(), 1);
        }
    }

    #[test]
    fn non_finite_requests_preserve_gain_without_disabling_source_cuts() {
        for request in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut fast_cut = envelope();
            assert_eq!(fast_cut.process(request, Some(-40.0), 100.0), 0.0);
            assert_eq!(fast_cut.process(request, Some(-4.0), 0.0), -12.0);
            assert_eq!(fast_cut.process(request, None, 100.0), -12.0);
            assert_eq!(fast_cut.reduction_sequence(), 1);
        }
    }

    #[test]
    fn small_buffer_recovery_accumulates_without_rounding_stalls() {
        let mut settings = config();
        settings.boost_rate_db_per_second = 0.001;
        let mut fast_cut = FastCut::new(settings).unwrap();
        fast_cut.process(-24.0, None, 0.0);
        for _ in 0..1_000 {
            fast_cut.process(0.0, None, 0.001);
        }
        assert_close(fast_cut.gain_db(), -23.999);
    }

    #[test]
    fn finite_extreme_inputs_stay_finite_and_within_the_gain_bounds() {
        let mut fast_cut = envelope();
        assert_eq!(fast_cut.process(f32::MAX, Some(f32::MAX), f32::MAX), -24.0);
        assert_eq!(fast_cut.process(f32::MAX, Some(f32::MIN), f32::MAX), 6.0);
        assert_eq!(fast_cut.process(f32::MIN, None, f32::MAX), -24.0);
    }

    #[test]
    fn reset_discards_gain_hold_and_reduction_history() {
        let mut fast_cut = envelope();
        fast_cut.process(0.0, Some(-4.0), 0.0);
        fast_cut.reset();
        assert_eq!(fast_cut.gain_db(), 0.0);
        assert_eq!(fast_cut.remaining_hold_seconds(), 0.0);
        assert_eq!(fast_cut.reduction_sequence(), 0);
        assert_close(fast_cut.process(6.0, Some(-40.0), 0.1), 0.02);
        assert_eq!(fast_cut.process(0.0, Some(-4.0), 0.0), -12.0);
        assert_eq!(fast_cut.reduction_sequence(), 1);
    }

    #[test]
    fn validates_every_configuration_field() {
        let fields: [fn(&mut FastCutConfig) -> &mut f32; 7] = [
            |config| &mut config.target_lufs,
            |config| &mut config.silence_gate_lufs,
            |config| &mut config.deadband_lu,
            |config| &mut config.maximum_boost_db,
            |config| &mut config.maximum_cut_db,
            |config| &mut config.boost_rate_db_per_second,
            |config| &mut config.hold_seconds,
        ];
        for field in fields {
            for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                let mut settings = config();
                *field(&mut settings) = invalid;
                assert!(FastCut::new(settings).is_err());
            }
        }
        for field in &fields[2..] {
            let mut settings = config();
            *field(&mut settings) = -1.0;
            assert!(FastCut::new(settings).is_err());
        }
        let mut settings = config();
        settings.silence_gate_lufs = settings.target_lufs;
        assert!(FastCut::new(settings).is_err());
        settings.silence_gate_lufs += 1.0;
        assert!(FastCut::new(settings).is_err());
    }

    #[test]
    fn zero_limits_rate_and_hold_have_defined_behavior() {
        let mut settings = config();
        settings.maximum_boost_db = 0.0;
        settings.maximum_cut_db = 0.0;
        settings.boost_rate_db_per_second = 0.0;
        settings.hold_seconds = 0.0;
        let mut fast_cut = FastCut::new(settings).unwrap();
        assert_eq!(fast_cut.process(6.0, Some(-40.0), 100.0), 0.0);
        assert_eq!(fast_cut.process(-24.0, Some(20.0), 0.0), 0.0);
        assert_eq!(fast_cut.remaining_hold_seconds(), 0.0);
        assert_eq!(fast_cut.reduction_sequence(), 0);
    }
}
