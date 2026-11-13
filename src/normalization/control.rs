// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::history::{LoudnessHistory, MAX_HISTORY_WINDOW_SECONDS};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControllerConfig {
    pub target_lufs: f32,
    pub silence_gate_lufs: f32,
    pub deadband_lu: f32,
    pub maximum_boost_db: f32,
    pub maximum_cut_db: f32,
    pub boost_rate_db_per_second: f32,
    /// None applies required percentile-based cuts immediately.
    pub cut_rate_db_per_second: Option<f32>,
    pub history_window_seconds: f32,
    pub history_percentile: f32,
    /// Audio-path hold only; the main percentile controller adds no hold.
    pub fast_cut_hold_seconds: f32,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ControllerConfigOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_lufs: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silence_gate_lufs: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadband_lu: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum_boost_db: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum_cut_db: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boost_rate_db_per_second: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut_rate_db_per_second: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_window_seconds: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_percentile: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast_cut_hold_seconds: Option<f32>,
}

impl ControllerConfigOverride {
    pub fn resolve(self, inherited: ControllerConfig) -> Result<ControllerConfig, &'static str> {
        ControllerConfig {
            target_lufs: self.target_lufs.unwrap_or(inherited.target_lufs),
            silence_gate_lufs: self
                .silence_gate_lufs
                .unwrap_or(inherited.silence_gate_lufs),
            deadband_lu: self.deadband_lu.unwrap_or(inherited.deadband_lu),
            maximum_boost_db: self.maximum_boost_db.unwrap_or(inherited.maximum_boost_db),
            maximum_cut_db: self.maximum_cut_db.unwrap_or(inherited.maximum_cut_db),
            boost_rate_db_per_second: self
                .boost_rate_db_per_second
                .unwrap_or(inherited.boost_rate_db_per_second),
            cut_rate_db_per_second: self
                .cut_rate_db_per_second
                .or(inherited.cut_rate_db_per_second),
            history_window_seconds: self
                .history_window_seconds
                .unwrap_or(inherited.history_window_seconds),
            history_percentile: self
                .history_percentile
                .unwrap_or(inherited.history_percentile),
            fast_cut_hold_seconds: self
                .fast_cut_hold_seconds
                .unwrap_or(inherited.fast_cut_hold_seconds),
        }
        .validate()
    }
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            target_lufs: -16.0,
            silence_gate_lufs: -50.0,
            deadband_lu: 0.75,
            maximum_boost_db: 6.0,
            maximum_cut_db: 24.0,
            boost_rate_db_per_second: 0.2,
            cut_rate_db_per_second: None,
            history_window_seconds: 30.0,
            history_percentile: 90.0,
            fast_cut_hold_seconds: 1.0,
        }
    }
}

impl ControllerConfig {
    pub fn capture_default() -> Self {
        Self {
            target_lufs: -18.0,
            silence_gate_lufs: -55.0,
            deadband_lu: 1.0,
            maximum_boost_db: 6.0,
            maximum_cut_db: 18.0,
            boost_rate_db_per_second: 0.2,
            cut_rate_db_per_second: None,
            history_window_seconds: 30.0,
            history_percentile: 90.0,
            fast_cut_hold_seconds: 1.0,
        }
    }
}

impl ControllerConfig {
    pub fn validate(self) -> Result<Self, &'static str> {
        let values = [
            self.target_lufs,
            self.silence_gate_lufs,
            self.deadband_lu,
            self.maximum_boost_db,
            self.maximum_cut_db,
            self.boost_rate_db_per_second,
            self.history_window_seconds,
            self.history_percentile,
            self.fast_cut_hold_seconds,
        ];
        if values.iter().any(|value| !value.is_finite())
            || self
                .cut_rate_db_per_second
                .is_some_and(|rate| !rate.is_finite())
        {
            return Err("all controller values must be finite");
        }
        if self.silence_gate_lufs >= self.target_lufs {
            return Err("silence gate must be below the target loudness");
        }
        if self.deadband_lu < 0.0
            || self.maximum_boost_db < 0.0
            || self.maximum_cut_db < 0.0
            || self.boost_rate_db_per_second <= 0.0
            || self.cut_rate_db_per_second.is_some_and(|rate| rate <= 0.0)
        {
            return Err("limits must be non-negative and rates must be positive");
        }
        if self.history_window_seconds <= 0.0
            || self.history_window_seconds > MAX_HISTORY_WINDOW_SECONDS
        {
            return Err("history window must be positive and at most 300 seconds");
        }
        if self.history_percentile <= 0.0 || self.history_percentile > 100.0 {
            return Err("history percentile must be greater than 0 and at most 100");
        }
        if self.fast_cut_hold_seconds < 0.0 {
            return Err("fast cut hold must be non-negative");
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StreamState {
    pub gain_db: f32,
    pub last_lufs: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Observation {
    /// Pre-gain 400 ms momentary LUFS, not the standard 3 s meter reading.
    pub lufs: f32,
    /// Elapsed control time. Valid time also ages silent/invalid observations.
    pub elapsed_seconds: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decision {
    Bypass,
    Silence { gain_db: f32 },
    Hold { gain_db: f32 },
    Adjust { previous_db: f32, gain_db: f32 },
}

impl Decision {
    pub fn target_gain_db(self) -> f32 {
        match self {
            Self::Bypass => 0.0,
            Self::Silence { gain_db } | Self::Hold { gain_db } | Self::Adjust { gain_db, .. } => {
                gain_db
            }
        }
    }
}

#[derive(Debug)]
pub struct Controller {
    config: ControllerConfig,
    streams: HashMap<String, ControlledStream>,
}

#[derive(Debug)]
struct ControlledStream {
    state: StreamState,
    history: LoudnessHistory,
}

impl ControlledStream {
    fn new(config: ControllerConfig) -> Self {
        Self {
            state: StreamState::default(),
            history: LoudnessHistory::new(config.history_window_seconds),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalDomain {
    Playback,
    Capture,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplicationPolicy {
    pub normalize_playback: bool,
    pub normalize_capture: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationPolicyOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playback: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<bool>,
}

impl ApplicationPolicyOverride {
    pub fn resolve(self, inherited: ApplicationPolicy) -> ApplicationPolicy {
        ApplicationPolicy {
            normalize_playback: self.playback.unwrap_or(inherited.normalize_playback),
            normalize_capture: self.capture.unwrap_or(inherited.normalize_capture),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserConfig {
    pub playback: ControllerConfigOverride,
    pub capture: ControllerConfigOverride,
    pub defaults: ApplicationPolicyOverride,
    pub applications: BTreeMap<String, ApplicationPolicyOverride>,
}

impl UserConfig {
    pub fn from_toml(source: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(source)
    }

    pub fn controller_configs(&self) -> Result<(ControllerConfig, ControllerConfig), &'static str> {
        Ok((
            self.playback.resolve(ControllerConfig::default())?,
            self.capture.resolve(ControllerConfig::capture_default())?,
        ))
    }
}

impl Default for ApplicationPolicy {
    fn default() -> Self {
        Self {
            normalize_playback: true,
            normalize_capture: true,
        }
    }
}

impl ApplicationPolicy {
    pub fn enables(self, domain: SignalDomain) -> bool {
        match domain {
            SignalDomain::Playback => self.normalize_playback,
            SignalDomain::Capture => self.normalize_capture,
        }
    }
}

#[derive(Debug)]
pub struct ControllerBank {
    playback: Controller,
    capture: Controller,
    policies: HashMap<String, ApplicationPolicy>,
    default_policy: ApplicationPolicy,
}

impl ControllerBank {
    pub fn new(
        playback_config: ControllerConfig,
        capture_config: ControllerConfig,
    ) -> Result<Self, &'static str> {
        Ok(Self {
            playback: Controller::new(playback_config)?,
            capture: Controller::new(capture_config)?,
            policies: HashMap::new(),
            default_policy: ApplicationPolicy::default(),
        })
    }

    pub fn defaults() -> Self {
        Self::new(
            ControllerConfig::default(),
            ControllerConfig::capture_default(),
        )
        .expect("built-in controller configurations are valid")
    }

    pub fn observe(
        &mut self,
        domain: SignalDomain,
        application_id: &str,
        stream_id: &str,
        observation: Observation,
    ) -> Decision {
        let policy = self
            .policies
            .get(application_id)
            .copied()
            .unwrap_or(self.default_policy);
        if !policy.enables(domain) {
            return Decision::Bypass;
        }
        match domain {
            SignalDomain::Playback => self.playback.observe(stream_id, observation),
            SignalDomain::Capture => self.capture.observe(stream_id, observation),
        }
    }

    pub fn set_policy(&mut self, application_id: impl Into<String>, policy: ApplicationPolicy) {
        self.policies.insert(application_id.into(), policy);
    }

    pub fn policy_for(&self, application_id: &str) -> ApplicationPolicy {
        self.policies
            .get(application_id)
            .copied()
            .unwrap_or(self.default_policy)
    }

    pub fn apply_user_config(&mut self, config: UserConfig) -> Result<(), &'static str> {
        let (playback, capture) = config.controller_configs()?;
        self.playback.set_config(playback);
        self.capture.set_config(capture);
        self.default_policy = config.defaults.resolve(ApplicationPolicy::default());
        self.policies = config
            .applications
            .into_iter()
            .map(|(application_id, policy)| (application_id, policy.resolve(self.default_policy)))
            .collect();
        Ok(())
    }

    pub fn stream(&self, domain: SignalDomain, stream_id: &str) -> Option<StreamState> {
        match domain {
            SignalDomain::Playback => self.playback.stream(stream_id),
            SignalDomain::Capture => self.capture.stream(stream_id),
        }
    }

    /// Inherit an actual audio-path cut before the next observation.
    pub fn adopt_gain_reduction(&mut self, domain: SignalDomain, stream_id: &str, gain_db: f32) {
        match domain {
            SignalDomain::Playback => self.playback.adopt_gain_reduction(stream_id, gain_db),
            SignalDomain::Capture => self.capture.adopt_gain_reduction(stream_id, gain_db),
        }
    }

    /// Live percentile of accepted pre-gain momentary observations, not a
    /// current-block maximum or a 3 s meter value.
    pub fn reference_lufs(&self, domain: SignalDomain, stream_id: &str) -> Option<f32> {
        match domain {
            SignalDomain::Playback => self.playback.reference_lufs(stream_id),
            SignalDomain::Capture => self.capture.reference_lufs(stream_id),
        }
    }

    /// Age only an existing stream's history, without changing gain or last
    /// LUFS. Non-positive/non-finite elapsed time and unknown streams are no-ops.
    pub fn age_stream_history(
        &mut self,
        domain: SignalDomain,
        stream_id: &str,
        elapsed_seconds: f32,
    ) {
        let controller = match domain {
            SignalDomain::Playback => &mut self.playback,
            SignalDomain::Capture => &mut self.capture,
        };
        if let Some(stream) = controller.streams.get_mut(stream_id) {
            stream.history.observe(None, elapsed_seconds);
        }
    }

    /// Age existing streams not handled during this interval, regardless of
    /// application policy. Mark already observed or individually aged streams
    /// as observed to avoid counting elapsed time twice.
    pub fn age_unobserved_streams(
        &mut self,
        elapsed_seconds: f32,
        mut observed: impl FnMut(SignalDomain, &str) -> bool,
    ) {
        if !elapsed_seconds.is_finite() || elapsed_seconds <= 0.0 {
            return;
        }
        for (domain, streams) in [
            (SignalDomain::Playback, &mut self.playback.streams),
            (SignalDomain::Capture, &mut self.capture.streams),
        ] {
            for (stream_id, stream) in streams {
                if !observed(domain, stream_id) {
                    stream.history.observe(None, elapsed_seconds);
                }
            }
        }
    }

    /// Remove ended streams, including their gain state and loudness history.
    pub fn retain_streams(&mut self, mut keep: impl FnMut(SignalDomain, &str) -> bool) {
        self.playback
            .streams
            .retain(|stream_id, _| keep(SignalDomain::Playback, stream_id));
        self.capture
            .streams
            .retain(|stream_id, _| keep(SignalDomain::Capture, stream_id));
    }

    pub fn config(&self, domain: SignalDomain) -> ControllerConfig {
        match domain {
            SignalDomain::Playback => self.playback.config(),
            SignalDomain::Capture => self.capture.config(),
        }
    }
}

impl Controller {
    pub fn new(config: ControllerConfig) -> Result<Self, &'static str> {
        Ok(Self {
            config: config.validate()?,
            streams: HashMap::new(),
        })
    }

    pub fn observe(&mut self, stream_id: &str, observation: Observation) -> Decision {
        let config = self.config;
        let stream = self
            .streams
            .entry(stream_id.to_owned())
            .or_insert_with(|| ControlledStream::new(config));
        let state = &mut stream.state;
        if !observation.elapsed_seconds.is_finite() || observation.elapsed_seconds <= 0.0 {
            return Decision::Silence {
                gain_db: state.gain_db,
            };
        }

        let elapsed = f64::from(observation.elapsed_seconds);
        let audible = observation.lufs.is_finite() && observation.lufs >= config.silence_gate_lufs;
        stream.history.observe(
            audible.then_some(observation.lufs),
            observation.elapsed_seconds,
        );
        if !audible {
            return Decision::Silence {
                gain_db: state.gain_db,
            };
        }
        state.last_lufs = Some(observation.lufs);
        let reference = stream
            .history
            .percentile(config.history_percentile)
            .expect("a valid observation supplies history");

        let error_db =
            f64::from(config.target_lufs) - f64::from(reference) - f64::from(state.gain_db);
        let desired_gain_db = (f64::from(config.target_lufs) - f64::from(reference)).clamp(
            -f64::from(config.maximum_cut_db),
            f64::from(config.maximum_boost_db),
        );
        let previous_db = state.gain_db;
        let gain_delta_db = desired_gain_db - f64::from(previous_db);
        let outside_limits = previous_db > config.maximum_boost_db;
        if (error_db.abs() <= f64::from(config.deadband_lu) && !outside_limits)
            || gain_delta_db == 0.0
        {
            return Decision::Hold {
                gain_db: state.gain_db,
            };
        }

        let next_gain_db = if gain_delta_db < 0.0 {
            config
                .cut_rate_db_per_second
                .map(|rate| desired_gain_db.max(f64::from(previous_db) - f64::from(rate) * elapsed))
                .unwrap_or(desired_gain_db)
        } else {
            desired_gain_db
                .min(f64::from(previous_db) + f64::from(config.boost_rate_db_per_second) * elapsed)
        };
        // Do not clamp an adopted, deeper audio-path cut upward to the normal
        // cut ceiling: it must recover slowly. The audio path owns the cut hold.
        state.gain_db = next_gain_db as f32;
        if state.gain_db == previous_db {
            return Decision::Hold {
                gain_db: state.gain_db,
            };
        }
        Decision::Adjust {
            previous_db,
            gain_db: state.gain_db,
        }
    }

    /// Adopt finite cuts only. A new stream starts no higher than unity; actual
    /// cuts beyond the configured ceiling are retained, with no invented LUFS.
    pub fn adopt_gain_reduction(&mut self, stream_id: &str, gain_db: f32) {
        if !gain_db.is_finite() {
            return;
        }
        let config = self.config;
        let stream = self
            .streams
            .entry(stream_id.to_owned())
            .or_insert_with(|| ControlledStream::new(config));
        if gain_db < stream.state.gain_db {
            stream.state.gain_db = gain_db;
        }
    }

    pub fn reference_lufs(&self, stream_id: &str) -> Option<f32> {
        self.streams
            .get(stream_id)?
            .history
            .percentile(self.config.history_percentile)
    }

    pub fn remove_stream(&mut self, stream_id: &str) -> Option<StreamState> {
        self.streams.remove(stream_id).map(|stream| stream.state)
    }

    pub fn stream(&self, stream_id: &str) -> Option<StreamState> {
        self.streams.get(stream_id).map(|stream| stream.state)
    }

    pub fn config(&self) -> ControllerConfig {
        self.config
    }

    fn set_config(&mut self, config: ControllerConfig) {
        self.config = config;
        for stream in self.streams.values_mut() {
            stream
                .history
                .set_window_seconds(config.history_window_seconds);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller() -> Controller {
        Controller::new(ControllerConfig::default()).unwrap()
    }

    fn sample(lufs: f32, elapsed_seconds: f32) -> Observation {
        Observation {
            lufs,
            elapsed_seconds,
        }
    }

    fn run_for(controller: &mut Controller, stream_id: &str, lufs: f32, seconds: u32) {
        for _ in 0..seconds * 10 {
            controller.observe(stream_id, sample(lufs, 0.1));
        }
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
    }

    #[test]
    fn twenty_second_quiet_loud_quiet_sections_do_not_pump_gain() {
        let mut controller = controller();
        run_for(&mut controller, "source", -35.0, 20);
        assert_close(controller.stream("source").unwrap().gain_db, 4.0);
        assert_eq!(controller.reference_lufs("source"), Some(-35.0));

        run_for(&mut controller, "source", -6.0, 20);
        assert_eq!(controller.stream("source").unwrap().gain_db, -10.0);
        assert_eq!(controller.reference_lufs("source"), Some(-6.0));

        run_for(&mut controller, "source", -35.0, 20);
        assert_eq!(controller.stream("source").unwrap().gain_db, -10.0);
        assert_eq!(controller.reference_lufs("source"), Some(-6.0));

        run_for(&mut controller, "source", -35.0, 8);
        assert_eq!(controller.reference_lufs("source"), Some(-35.0));
        let recovered = controller.stream("source").unwrap().gain_db;
        assert!(recovered > -10.0 && recovered <= -9.7);
    }

    #[test]
    fn a_single_brief_loud_block_does_not_replace_the_percentile_or_cut() {
        let mut controller = controller();
        run_for(&mut controller, "source", -16.0, 9);
        assert_eq!(controller.stream("source").unwrap().gain_db, 0.0);
        assert_eq!(
            controller.observe("source", sample(-4.0, 0.1)),
            Decision::Hold { gain_db: 0.0 }
        );
        assert_eq!(controller.reference_lufs("source"), Some(-16.0));
        assert_eq!(controller.stream("source").unwrap().last_lufs, Some(-4.0));
    }

    #[test]
    fn duration_weighted_percentile_drives_immediate_required_cuts() {
        let mut controller = controller();
        controller.observe("source", sample(-16.0, 9.0));
        assert_eq!(
            controller.observe("source", sample(-4.0, 1.0)),
            Decision::Hold { gain_db: 0.0 }
        );
        assert_eq!(controller.reference_lufs("source"), Some(-16.0));
        assert_eq!(
            controller.observe("source", sample(-4.0, 0.1)),
            Decision::Adjust {
                previous_db: 0.0,
                gain_db: -12.0
            }
        );
        assert_eq!(controller.reference_lufs("source"), Some(-4.0));
    }

    #[test]
    fn equivalent_quiet_chunking_preserves_slow_boost() {
        let mut whole = controller();
        let mut chunks = controller();
        whole.observe("source", sample(-30.0, 20.0));
        run_for(&mut chunks, "source", -30.0, 20);
        assert_close(whole.stream("source").unwrap().gain_db, 4.0);
        assert_close(chunks.stream("source").unwrap().gain_db, 4.0);
        assert_eq!(
            whole.reference_lufs("source"),
            chunks.reference_lufs("source")
        );
    }

    #[test]
    fn silence_and_invalid_lufs_age_history_but_never_raise_gain() {
        for lufs in [-80.0, f32::NEG_INFINITY, f32::INFINITY, f32::NAN] {
            let mut controller = controller();
            controller.observe("source", sample(-4.0, 1.0));
            let before = controller.stream("source").unwrap();
            assert_eq!(
                controller.observe("source", sample(lufs, 29.0)),
                Decision::Silence { gain_db: -12.0 }
            );
            assert_eq!(controller.reference_lufs("source"), Some(-4.0));
            assert_eq!(
                controller.observe("source", sample(lufs, 1.0)),
                Decision::Silence { gain_db: -12.0 }
            );
            assert_eq!(controller.reference_lufs("source"), None);
            assert_eq!(controller.stream("source"), Some(before));
            controller.observe("source", sample(-30.0, 0.1));
            assert_close(controller.stream("source").unwrap().gain_db, -11.98);
            assert_eq!(controller.reference_lufs("source"), Some(-30.0));
        }
    }

    #[test]
    fn invalid_elapsed_neither_ages_history_nor_changes_state() {
        let mut controller = controller();
        controller.observe("source", sample(-4.0, 1.0));
        let before = controller.stream("source").unwrap();
        for elapsed in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                controller.observe("source", sample(-30.0, elapsed)),
                Decision::Silence { gain_db: -12.0 }
            );
            assert_eq!(controller.stream("source"), Some(before));
            assert_eq!(controller.reference_lufs("source"), Some(-4.0));
        }
    }

    #[test]
    fn adoption_has_no_fictitious_history_or_extra_recovery_hold() {
        let mut controller = controller();
        controller.adopt_gain_reduction("source", -12.0);
        assert_eq!(
            controller.stream("source"),
            Some(StreamState {
                gain_db: -12.0,
                last_lufs: None
            })
        );
        assert_eq!(controller.reference_lufs("source"), None);
        assert!(matches!(
            controller.observe("source", sample(-30.0, 0.1)),
            Decision::Adjust {
                previous_db: -12.0,
                ..
            }
        ));
        assert_close(controller.stream("source").unwrap().gain_db, -11.98);
        assert_eq!(controller.reference_lufs("source"), Some(-30.0));

        let before = controller.stream("source").unwrap();
        for gain in [-6.0, 0.0, 6.0, f32::NAN, f32::NEG_INFINITY, f32::INFINITY] {
            controller.adopt_gain_reduction("source", gain);
            assert_eq!(controller.stream("source"), Some(before));
            assert_eq!(controller.reference_lufs("source"), Some(-30.0));
        }
    }

    #[test]
    fn adoption_lowers_positive_gain_and_preserves_existing_history() {
        let mut controller = controller();
        run_for(&mut controller, "source", -30.0, 20);
        controller.adopt_gain_reduction("source", 2.0);
        assert_eq!(controller.stream("source").unwrap().gain_db, 2.0);
        assert_eq!(controller.stream("source").unwrap().last_lufs, Some(-30.0));
        assert_eq!(controller.reference_lufs("source"), Some(-30.0));
        controller.adopt_gain_reduction("source", 3.0);
        assert_eq!(controller.stream("source").unwrap().gain_db, 2.0);
        controller.observe("source", sample(-30.0, 0.1));
        assert_close(controller.stream("source").unwrap().gain_db, 2.02);
    }

    #[test]
    fn adopted_deeper_cuts_recover_without_jumping_to_the_cut_ceiling() {
        let mut controller = controller();
        controller.adopt_gain_reduction("source", -40.0);
        controller.observe("source", sample(-30.0, 0.1));
        assert_close(controller.stream("source").unwrap().gain_db, -39.98);
        controller.observe("source", sample(f32::NEG_INFINITY, 30.0));
        assert_close(controller.stream("source").unwrap().gain_db, -39.98);
        assert_eq!(controller.reference_lufs("source"), None);
    }

    #[test]
    fn adoption_never_initializes_above_unity_or_with_invalid_gain() {
        let mut controller = controller();
        controller.adopt_gain_reduction("source", 6.0);
        assert_eq!(controller.stream("source"), Some(StreamState::default()));
        assert_eq!(controller.reference_lufs("source"), None);
        for gain in [f32::NAN, f32::NEG_INFINITY, f32::INFINITY] {
            controller.adopt_gain_reduction("missing", gain);
        }
        assert_eq!(controller.stream("missing"), None);
    }

    #[test]
    fn supplied_positive_legacy_cut_rate_remains_supported() {
        let mut controller = Controller::new(ControllerConfig {
            cut_rate_db_per_second: Some(3.0),
            ..ControllerConfig::default()
        })
        .unwrap();
        controller.observe("source", sample(-30.0, 1.0));
        assert_eq!(
            controller.observe("source", sample(-4.0, 1.0)),
            Decision::Adjust {
                previous_db: 0.2,
                gain_db: -2.8
            }
        );
    }

    #[test]
    fn history_and_cut_rate_overrides_resolve_and_inherit() {
        let defaults = ControllerConfig::default();
        assert_eq!(
            ControllerConfigOverride::default().resolve(defaults),
            Ok(defaults)
        );
        let inherited = ControllerConfig {
            cut_rate_db_per_second: Some(2.0),
            ..defaults
        };
        assert_eq!(
            ControllerConfigOverride::default().resolve(inherited),
            Ok(inherited)
        );
        let config = UserConfig::from_toml(
            r#"
            [playback]
            history_window_seconds = 300.0
            history_percentile = 75.0
            fast_cut_hold_seconds = 0.0
            cut_rate_db_per_second = 3.0
            "#,
        )
        .unwrap();
        let (playback, capture) = config.controller_configs().unwrap();
        assert_eq!(playback.history_window_seconds, 300.0);
        assert_eq!(playback.history_percentile, 75.0);
        assert_eq!(playback.fast_cut_hold_seconds, 0.0);
        assert_eq!(playback.cut_rate_db_per_second, Some(3.0));
        assert_eq!(capture.cut_rate_db_per_second, None);
        assert_eq!(capture.history_window_seconds, 30.0);
    }

    #[test]
    fn new_configuration_values_must_be_finite_and_bounded() {
        let defaults = ControllerConfig::default();
        for value in [f32::NAN, f32::NEG_INFINITY, f32::INFINITY] {
            for config in [
                ControllerConfig {
                    history_window_seconds: value,
                    ..defaults
                },
                ControllerConfig {
                    history_percentile: value,
                    ..defaults
                },
                ControllerConfig {
                    fast_cut_hold_seconds: value,
                    ..defaults
                },
                ControllerConfig {
                    cut_rate_db_per_second: Some(value),
                    ..defaults
                },
            ] {
                assert_eq!(
                    config.validate(),
                    Err("all controller values must be finite")
                );
            }
        }
        for value in [-1.0, 0.0, 300.1] {
            assert!(
                ControllerConfig {
                    history_window_seconds: value,
                    ..defaults
                }
                .validate()
                .is_err()
            );
        }
        for value in [-1.0, 0.0, 100.1] {
            assert!(
                ControllerConfig {
                    history_percentile: value,
                    ..defaults
                }
                .validate()
                .is_err()
            );
        }
        for value in [-1.0, 0.0] {
            assert!(
                ControllerConfig {
                    cut_rate_db_per_second: Some(value),
                    ..defaults
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            ControllerConfig {
                fast_cut_hold_seconds: -0.1,
                ..defaults
            }
            .validate()
            .is_err()
        );
        assert!(
            ControllerConfig {
                history_window_seconds: 300.0,
                history_percentile: 100.0,
                fast_cut_hold_seconds: 0.0,
                ..defaults
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn changing_history_settings_reuses_and_trims_real_observations() {
        let mut bank = ControllerBank::defaults();
        bank.observe(
            SignalDomain::Playback,
            "player",
            "source",
            sample(-4.0, 2.0),
        );
        bank.observe(
            SignalDomain::Playback,
            "player",
            "source",
            sample(-30.0, 8.0),
        );
        assert_eq!(
            bank.reference_lufs(SignalDomain::Playback, "source"),
            Some(-4.0)
        );
        bank.apply_user_config(UserConfig {
            playback: ControllerConfigOverride {
                history_percentile: Some(50.0),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            bank.reference_lufs(SignalDomain::Playback, "source"),
            Some(-30.0)
        );
        bank.apply_user_config(UserConfig {
            playback: ControllerConfigOverride {
                history_window_seconds: Some(5.0),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            bank.reference_lufs(SignalDomain::Playback, "source"),
            Some(-30.0)
        );
        assert_eq!(
            bank.stream(SignalDomain::Playback, "source")
                .unwrap()
                .gain_db,
            -12.0
        );
        bank.observe(
            SignalDomain::Playback,
            "player",
            "source",
            sample(f32::NEG_INFINITY, 5.0),
        );
        assert_eq!(bank.reference_lufs(SignalDomain::Playback, "source"), None);
    }

    #[test]
    fn invalid_capture_config_does_not_partially_trim_playback_history() {
        let mut bank = ControllerBank::defaults();
        bank.observe(
            SignalDomain::Playback,
            "player",
            "source",
            sample(-4.0, 2.0),
        );
        bank.observe(
            SignalDomain::Playback,
            "player",
            "source",
            sample(-30.0, 8.0),
        );
        let before = bank.stream(SignalDomain::Playback, "source");
        assert!(
            bank.apply_user_config(UserConfig {
                playback: ControllerConfigOverride {
                    history_window_seconds: Some(5.0),
                    ..Default::default()
                },
                capture: ControllerConfigOverride {
                    history_percentile: Some(0.0),
                    ..Default::default()
                },
                ..Default::default()
            })
            .is_err()
        );
        assert_eq!(
            bank.config(SignalDomain::Playback).history_window_seconds,
            30.0
        );
        assert_eq!(
            bank.reference_lufs(SignalDomain::Playback, "source"),
            Some(-4.0)
        );
        assert_eq!(bank.stream(SignalDomain::Playback, "source"), before);
    }

    #[test]
    fn streams_and_directions_isolate_history_adoption_and_elapsed_time() {
        let mut bank = ControllerBank::defaults();
        bank.observe(
            SignalDomain::Playback,
            "player",
            "shared",
            sample(-4.0, 1.0),
        );
        bank.observe(
            SignalDomain::Capture,
            "player",
            "shared",
            sample(-30.0, 1.0),
        );
        bank.observe(
            SignalDomain::Playback,
            "player",
            "other",
            sample(-25.0, 1.0),
        );
        bank.adopt_gain_reduction(SignalDomain::Capture, "shared", -6.0);
        bank.observe(
            SignalDomain::Playback,
            "player",
            "other",
            sample(f32::NEG_INFINITY, 30.0),
        );
        assert_eq!(
            bank.reference_lufs(SignalDomain::Playback, "shared"),
            Some(-4.0)
        );
        assert_eq!(
            bank.reference_lufs(SignalDomain::Capture, "shared"),
            Some(-30.0)
        );
        assert_eq!(bank.reference_lufs(SignalDomain::Playback, "other"), None);
        assert_eq!(
            bank.stream(SignalDomain::Playback, "shared")
                .unwrap()
                .gain_db,
            -12.0
        );
        assert_eq!(
            bank.stream(SignalDomain::Capture, "shared")
                .unwrap()
                .gain_db,
            -6.0
        );
        assert_eq!(
            bank.stream(SignalDomain::Playback, "other")
                .unwrap()
                .gain_db,
            0.2
        );
    }

    #[test]
    fn bypassed_history_expires_before_the_first_quiet_observation() {
        for domain in [SignalDomain::Playback, SignalDomain::Capture] {
            for bulk in [false, true] {
                for (elapsed, ticks) in [(60.0, 1), (0.1, 600)] {
                    let mut bank = ControllerBank::defaults();
                    bank.observe(domain, "player", "source", sample(-4.0, 1.0));
                    let before = bank.stream(domain, "source").unwrap();
                    bank.set_policy(
                        "player",
                        ApplicationPolicy {
                            normalize_playback: false,
                            normalize_capture: false,
                        },
                    );
                    assert_eq!(
                        bank.observe(domain, "player", "source", sample(-30.0, 60.0)),
                        Decision::Bypass
                    );
                    for _ in 0..ticks {
                        if bulk {
                            bank.age_unobserved_streams(elapsed, |_, _| false);
                        } else {
                            bank.age_stream_history(domain, "source", elapsed);
                        }
                    }
                    assert_eq!(bank.reference_lufs(domain, "source"), None);
                    assert_eq!(bank.stream(domain, "source"), Some(before));

                    bank.set_policy("player", ApplicationPolicy::default());
                    bank.observe(domain, "player", "source", sample(-30.0, 0.1));
                    assert_eq!(bank.reference_lufs(domain, "source"), Some(-30.0));
                    assert_close(
                        bank.stream(domain, "source").unwrap().gain_db,
                        before.gain_db + 0.02,
                    );
                }
            }
        }
    }

    #[test]
    fn individual_history_aging_isolates_streams_and_directions() {
        let mut bank = ControllerBank::defaults();
        let streams = [
            (SignalDomain::Playback, "shared", -4.0),
            (SignalDomain::Capture, "shared", -8.0),
            (SignalDomain::Playback, "other", -30.0),
        ];
        let before: Vec<_> = streams
            .into_iter()
            .map(|(domain, stream_id, lufs)| {
                bank.observe(domain, "player", stream_id, sample(lufs, 1.0));
                (domain, stream_id, bank.stream(domain, stream_id).unwrap())
            })
            .collect();

        bank.age_stream_history(SignalDomain::Playback, "shared", 29.0);
        assert_eq!(
            bank.reference_lufs(SignalDomain::Playback, "shared"),
            Some(-4.0)
        );
        bank.age_stream_history(SignalDomain::Playback, "shared", 1.0);
        assert_eq!(bank.reference_lufs(SignalDomain::Playback, "shared"), None);
        assert_eq!(
            bank.reference_lufs(SignalDomain::Capture, "shared"),
            Some(-8.0)
        );
        assert_eq!(
            bank.reference_lufs(SignalDomain::Playback, "other"),
            Some(-30.0)
        );
        for (domain, stream_id, state) in before {
            assert_eq!(bank.stream(domain, stream_id), Some(state));
        }
    }

    #[test]
    fn bulk_history_aging_skips_observed_streams_by_direction_and_id() {
        let mut bank = ControllerBank::defaults();
        let streams = [
            (SignalDomain::Playback, "shared", -4.0),
            (SignalDomain::Capture, "shared", -8.0),
            (SignalDomain::Playback, "other", -30.0),
            (SignalDomain::Capture, "other", -20.0),
        ];
        let before: Vec<_> = streams
            .into_iter()
            .map(|(domain, stream_id, lufs)| {
                bank.observe(domain, "player", stream_id, sample(lufs, 1.0));
                (domain, stream_id, bank.stream(domain, stream_id).unwrap())
            })
            .collect();

        let is_observed = |domain, stream_id: &str| {
            (domain == SignalDomain::Playback && stream_id == "shared")
                || (domain == SignalDomain::Capture && stream_id == "other")
        };
        let mut visited = 0;
        for elapsed in [29.0, 1.0] {
            bank.age_unobserved_streams(elapsed, |domain, stream_id| {
                visited += 1;
                is_observed(domain, stream_id)
            });
        }
        assert_eq!(visited, 8);
        for (domain, stream_id, state) in before {
            assert_eq!(bank.stream(domain, stream_id), Some(state));
            let reference = if is_observed(domain, stream_id) {
                state.last_lufs
            } else {
                None
            };
            assert_eq!(bank.reference_lufs(domain, stream_id), reference);
        }
    }

    #[test]
    fn history_aging_ignores_invalid_elapsed_time() {
        let mut bank = ControllerBank::defaults();
        for domain in [SignalDomain::Playback, SignalDomain::Capture] {
            bank.observe(domain, "player", "shared", sample(-4.0, 1.0));
        }
        let playback = bank.stream(SignalDomain::Playback, "shared");
        let capture = bank.stream(SignalDomain::Capture, "shared");
        for elapsed in [0.0, -1.0, f32::NAN, f32::NEG_INFINITY, f32::INFINITY] {
            bank.age_stream_history(SignalDomain::Playback, "shared", elapsed);
            bank.age_unobserved_streams(elapsed, |_, _| false);
            assert_eq!(
                bank.reference_lufs(SignalDomain::Playback, "shared"),
                Some(-4.0)
            );
            assert_eq!(
                bank.reference_lufs(SignalDomain::Capture, "shared"),
                Some(-4.0)
            );
            assert_eq!(bank.stream(SignalDomain::Playback, "shared"), playback);
            assert_eq!(bank.stream(SignalDomain::Capture, "shared"), capture);
        }
        bank.age_stream_history(SignalDomain::Playback, "shared", 29.0);
        assert_eq!(
            bank.reference_lufs(SignalDomain::Playback, "shared"),
            Some(-4.0)
        );
        bank.age_stream_history(SignalDomain::Playback, "shared", 1.0);
        assert_eq!(bank.reference_lufs(SignalDomain::Playback, "shared"), None);
        assert_eq!(
            bank.reference_lufs(SignalDomain::Capture, "shared"),
            Some(-4.0)
        );
        assert_eq!(bank.stream(SignalDomain::Playback, "shared"), playback);
        assert_eq!(bank.stream(SignalDomain::Capture, "shared"), capture);
    }

    #[test]
    fn history_aging_never_creates_stream_state() {
        let mut bank = ControllerBank::defaults();
        for domain in [SignalDomain::Playback, SignalDomain::Capture] {
            for elapsed in [60.0, 0.0, -1.0, f32::NAN, f32::INFINITY] {
                bank.age_stream_history(domain, "missing", elapsed);
            }
        }
        let mut visited = 0;
        bank.age_unobserved_streams(60.0, |_, _| {
            visited += 1;
            false
        });
        assert_eq!(visited, 0);
        assert!(bank.playback.streams.is_empty() && bank.capture.streams.is_empty());

        bank.observe(SignalDomain::Capture, "player", "shared", sample(-4.0, 1.0));
        bank.age_stream_history(SignalDomain::Playback, "shared", 60.0);
        assert_eq!(bank.stream(SignalDomain::Playback, "shared"), None);
        assert_eq!(
            bank.reference_lufs(SignalDomain::Capture, "shared"),
            Some(-4.0)
        );
    }

    #[test]
    fn retain_streams_prunes_bypassed_history_and_isolates_directions() {
        let mut bank = ControllerBank::defaults();
        for domain in [SignalDomain::Playback, SignalDomain::Capture] {
            bank.observe(domain, "player", "recycled", sample(-4.0, 1.0));
            bank.observe(domain, "player", "keep", sample(-30.0, 1.0));
        }
        bank.set_policy(
            "player",
            ApplicationPolicy {
                normalize_playback: false,
                normalize_capture: false,
            },
        );
        assert_eq!(
            bank.observe(
                SignalDomain::Playback,
                "player",
                "recycled",
                sample(-30.0, 1.0)
            ),
            Decision::Bypass
        );
        let mut visited = 0;
        bank.retain_streams(|domain, stream_id| {
            visited += 1;
            domain == SignalDomain::Capture || stream_id == "keep"
        });
        assert_eq!(visited, 4);
        assert_eq!(bank.stream(SignalDomain::Playback, "recycled"), None);
        assert_eq!(
            bank.reference_lufs(SignalDomain::Playback, "recycled"),
            None
        );
        assert_eq!(
            bank.reference_lufs(SignalDomain::Capture, "recycled"),
            Some(-4.0)
        );
        assert_eq!(
            bank.reference_lufs(SignalDomain::Playback, "keep"),
            Some(-30.0)
        );
        assert_eq!(
            bank.reference_lufs(SignalDomain::Capture, "keep"),
            Some(-30.0)
        );

        bank.set_policy("player", ApplicationPolicy::default());
        bank.observe(
            SignalDomain::Playback,
            "player",
            "recycled",
            sample(-30.0, 0.1),
        );
        assert_close(
            bank.stream(SignalDomain::Playback, "recycled")
                .unwrap()
                .gain_db,
            0.02,
        );
        assert_eq!(
            bank.reference_lufs(SignalDomain::Playback, "recycled"),
            Some(-30.0)
        );
        assert_eq!(
            bank.reference_lufs(SignalDomain::Capture, "recycled"),
            Some(-4.0)
        );
        bank.retain_streams(|_, _| false);
        assert!(bank.playback.streams.is_empty() && bank.capture.streams.is_empty());
    }

    #[test]
    fn removing_a_stream_discards_gain_and_history_together() {
        let mut controller = controller();
        controller.observe("source", sample(-4.0, 1.0));
        assert_eq!(
            controller.remove_stream("source"),
            Some(StreamState {
                gain_db: -12.0,
                last_lufs: Some(-4.0)
            })
        );
        assert_eq!(controller.reference_lufs("source"), None);
        controller.observe("source", sample(-30.0, 0.1));
        assert_close(controller.stream("source").unwrap().gain_db, 0.02);
    }

    #[test]
    fn defaults_limit_boost_but_make_required_cuts_immediate() {
        let mut controller = controller();
        assert_eq!(
            controller.observe(
                "game",
                Observation {
                    lufs: -24.0,
                    elapsed_seconds: 1.0
                }
            ),
            Decision::Adjust {
                previous_db: 0.0,
                gain_db: 0.2
            }
        );
        assert_eq!(
            controller.observe(
                "game",
                Observation {
                    lufs: -4.0,
                    elapsed_seconds: 1.0
                }
            ),
            Decision::Adjust {
                previous_db: 0.2,
                gain_db: -12.0
            }
        );
    }

    #[test]
    fn silence_preserves_existing_gain() {
        let mut controller = controller();
        controller.observe(
            "game",
            Observation {
                lufs: -24.0,
                elapsed_seconds: 2.0,
            },
        );
        assert_eq!(
            controller.observe(
                "game",
                Observation {
                    lufs: -80.0,
                    elapsed_seconds: 2.0
                }
            ),
            Decision::Silence { gain_db: 0.4 }
        );
    }

    #[test]
    fn deadband_prevents_gain_hunting() {
        let mut controller = controller();
        assert_eq!(
            controller.observe(
                "music",
                Observation {
                    lufs: -16.5,
                    elapsed_seconds: 1.0
                }
            ),
            Decision::Hold { gain_db: 0.0 }
        );
    }

    #[test]
    fn streams_have_independent_state() {
        let mut controller = controller();
        controller.observe(
            "game",
            Observation {
                lufs: -30.0,
                elapsed_seconds: 1.0,
            },
        );
        controller.observe(
            "browser",
            Observation {
                lufs: -4.0,
                elapsed_seconds: 1.0,
            },
        );
        assert_eq!(controller.stream("game").unwrap().gain_db, 0.2);
        assert_eq!(controller.stream("browser").unwrap().gain_db, -12.0);
    }

    #[test]
    fn gain_is_clamped_to_configured_limits() {
        let mut controller = controller();
        for _ in 0..100 {
            controller.observe(
                "quiet",
                Observation {
                    lufs: -50.0,
                    elapsed_seconds: 1.0,
                },
            );
            controller.observe(
                "loud",
                Observation {
                    lufs: 20.0,
                    elapsed_seconds: 1.0,
                },
            );
        }
        assert_eq!(controller.stream("quiet").unwrap().gain_db, 6.0);
        assert_eq!(controller.stream("loud").unwrap().gain_db, -24.0);
    }

    #[test]
    fn stable_source_converges_without_gain_runaway() {
        let mut controller = controller();
        for _ in 0..20 {
            controller.observe(
                "source",
                Observation {
                    lufs: -20.0,
                    elapsed_seconds: 1.0,
                },
            );
        }
        let gain_db = controller.stream("source").unwrap().gain_db;
        assert!((3.25..=4.0).contains(&gain_db));
        assert_eq!(
            controller.observe(
                "source",
                Observation {
                    lufs: -20.0,
                    elapsed_seconds: 1.0,
                }
            ),
            Decision::Hold { gain_db }
        );
    }

    #[test]
    fn directional_defaults_use_conservative_independent_targets() {
        assert_eq!(ControllerConfig::default().target_lufs, -16.0);
        assert_eq!(ControllerConfig::capture_default().target_lufs, -18.0);
        for config in [
            ControllerConfig::default(),
            ControllerConfig::capture_default(),
        ] {
            assert_eq!(config.maximum_boost_db, 6.0);
            assert_eq!(config.boost_rate_db_per_second, 0.2);
            assert_eq!(config.cut_rate_db_per_second, None);
            assert_eq!(config.history_window_seconds, 30.0);
            assert_eq!(config.history_percentile, 90.0);
            assert_eq!(config.fast_cut_hold_seconds, 1.0);
        }
    }

    #[test]
    fn user_config_overrides_directional_controller_settings() {
        let config = UserConfig::from_toml(
            r#"
                [playback]
                target_lufs = -17.5
                maximum_boost_db = 8.0

                [capture]
                target_lufs = -20.0
                boost_rate_db_per_second = 0.25
            "#,
        )
        .unwrap();
        let mut controllers = ControllerBank::defaults();

        controllers.apply_user_config(config).unwrap();

        let playback = controllers.config(SignalDomain::Playback);
        assert_eq!(playback.target_lufs, -17.5);
        assert_eq!(playback.maximum_boost_db, 8.0);
        assert_eq!(playback.silence_gate_lufs, -50.0);
        let capture = controllers.config(SignalDomain::Capture);
        assert_eq!(capture.target_lufs, -20.0);
        assert_eq!(capture.boost_rate_db_per_second, 0.25);
        assert_eq!(capture.maximum_boost_db, 6.0);
    }

    #[test]
    fn invalid_directional_controller_settings_are_rejected_atomically() {
        let config = UserConfig::from_toml(
            r#"
                [playback]
                target_lufs = -60.0
            "#,
        )
        .unwrap();
        let mut controllers = ControllerBank::defaults();
        let before = controllers.config(SignalDomain::Playback);

        assert_eq!(
            controllers.apply_user_config(config),
            Err("silence gate must be below the target loudness")
        );
        assert_eq!(controllers.config(SignalDomain::Playback), before);
    }

    #[test]
    fn playback_gate_ignores_quiet_auxiliary_streams() {
        let mut controller = controller();
        assert_eq!(
            controller.observe(
                "browser-utility",
                Observation {
                    lufs: -56.2,
                    elapsed_seconds: 1.0,
                },
            ),
            Decision::Silence { gain_db: 0.0 }
        );
    }

    #[test]
    fn capture_and_playback_state_never_mix() {
        let mut controllers = ControllerBank::defaults();
        let observation = Observation {
            lufs: -30.0,
            elapsed_seconds: 1.0,
        };
        controllers.observe(SignalDomain::Capture, "browser", "shared-id", observation);

        assert_eq!(
            controllers
                .stream(SignalDomain::Capture, "shared-id")
                .unwrap()
                .gain_db,
            0.2
        );
        assert_eq!(
            controllers.stream(SignalDomain::Playback, "shared-id"),
            None
        );
    }

    #[test]
    fn application_policy_controls_directions_independently() {
        let mut controllers = ControllerBank::defaults();
        controllers.set_policy(
            "game",
            ApplicationPolicy {
                normalize_playback: true,
                normalize_capture: false,
            },
        );
        let observation = Observation {
            lufs: -30.0,
            elapsed_seconds: 1.0,
        };

        assert!(matches!(
            controllers.observe(SignalDomain::Playback, "game", "output", observation),
            Decision::Adjust { .. }
        ));
        assert_eq!(
            controllers.observe(SignalDomain::Capture, "game", "microphone", observation),
            Decision::Bypass
        );
        assert_eq!(
            controllers.stream(SignalDomain::Capture, "microphone"),
            None
        );
    }

    #[test]
    fn policy_lookup_resolves_application_and_default_settings() {
        let mut controllers = ControllerBank::defaults();
        let override_policy = ApplicationPolicy {
            normalize_playback: false,
            normalize_capture: true,
        };
        controllers.set_policy("recorder", override_policy);

        assert_eq!(controllers.policy_for("recorder"), override_policy);
        assert_eq!(
            controllers.policy_for("other"),
            ApplicationPolicy::default()
        );
    }

    #[test]
    fn user_config_supports_independent_direction_overrides() {
        let config = UserConfig::from_toml(
            r#"
                [defaults]
                playback = true
                capture = false

                [applications.recorder]
                playback = false
                capture = true

                [applications.game]
                playback = false
            "#,
        )
        .unwrap();
        let mut controllers = ControllerBank::defaults();
        controllers.apply_user_config(config).unwrap();
        let observation = Observation {
            lufs: -30.0,
            elapsed_seconds: 1.0,
        };

        assert_eq!(
            controllers.observe(SignalDomain::Capture, "browser", "mic", observation),
            Decision::Bypass
        );
        assert_eq!(
            controllers.observe(SignalDomain::Playback, "recorder", "monitor", observation),
            Decision::Bypass
        );
        assert!(matches!(
            controllers.observe(SignalDomain::Capture, "recorder", "mic", observation),
            Decision::Adjust { .. }
        ));
        assert_eq!(
            controllers.observe(SignalDomain::Capture, "game", "mic", observation),
            Decision::Bypass
        );
    }
}
