// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub mod daemon;
pub mod gain;
pub mod ipc;
pub mod meter;
pub mod pipewire_backend;
pub mod pipewire_filter;
pub mod pipewire_links;
pub mod pipewire_route_backend;
pub mod route_transaction;
pub mod routing;
pub mod runtime_config;
pub mod stream_control;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControllerConfig {
    pub target_lufs: f32,
    pub silence_gate_lufs: f32,
    pub deadband_lu: f32,
    pub maximum_boost_db: f32,
    pub maximum_cut_db: f32,
    pub boost_rate_db_per_second: f32,
    pub cut_rate_db_per_second: f32,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            target_lufs: -13.0,
            silence_gate_lufs: -50.0,
            deadband_lu: 0.75,
            maximum_boost_db: 18.0,
            maximum_cut_db: 24.0,
            boost_rate_db_per_second: 1.0,
            cut_rate_db_per_second: 3.0,
        }
    }
}

impl ControllerConfig {
    pub fn capture_default() -> Self {
        Self {
            target_lufs: -18.0,
            silence_gate_lufs: -55.0,
            deadband_lu: 1.0,
            maximum_boost_db: 12.0,
            maximum_cut_db: 18.0,
            boost_rate_db_per_second: 0.5,
            cut_rate_db_per_second: 3.0,
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
            self.cut_rate_db_per_second,
        ];
        if values.iter().any(|value| !value.is_finite()) {
            return Err("all controller values must be finite");
        }
        if self.silence_gate_lufs >= self.target_lufs {
            return Err("silence gate must be below the target loudness");
        }
        if self.deadband_lu < 0.0
            || self.maximum_boost_db < 0.0
            || self.maximum_cut_db < 0.0
            || self.boost_rate_db_per_second <= 0.0
            || self.cut_rate_db_per_second <= 0.0
        {
            return Err("limits must be non-negative and rates must be positive");
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
    pub lufs: f32,
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
    streams: HashMap<String, StreamState>,
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserConfig {
    pub defaults: ApplicationPolicyOverride,
    pub applications: HashMap<String, ApplicationPolicyOverride>,
}

impl UserConfig {
    pub fn from_toml(source: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(source)
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

    pub fn apply_user_config(&mut self, config: UserConfig) {
        self.default_policy = config.defaults.resolve(ApplicationPolicy::default());
        self.policies = config
            .applications
            .into_iter()
            .map(|(application_id, policy)| (application_id, policy.resolve(self.default_policy)))
            .collect();
    }

    pub fn stream(&self, domain: SignalDomain, stream_id: &str) -> Option<StreamState> {
        match domain {
            SignalDomain::Playback => self.playback.stream(stream_id),
            SignalDomain::Capture => self.capture.stream(stream_id),
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
        let state = self.streams.entry(stream_id.to_owned()).or_default();
        if !observation.lufs.is_finite()
            || !observation.elapsed_seconds.is_finite()
            || observation.elapsed_seconds <= 0.0
            || observation.lufs < self.config.silence_gate_lufs
        {
            return Decision::Silence {
                gain_db: state.gain_db,
            };
        }

        state.last_lufs = Some(observation.lufs);
        let error_db = self.config.target_lufs - (observation.lufs + state.gain_db);
        if error_db.abs() <= self.config.deadband_lu {
            return Decision::Hold {
                gain_db: state.gain_db,
            };
        }

        let previous_db = state.gain_db;
        let desired_gain_db = (self.config.target_lufs - observation.lufs)
            .clamp(-self.config.maximum_cut_db, self.config.maximum_boost_db);
        let gain_delta_db = desired_gain_db - state.gain_db;
        let rate = if gain_delta_db > 0.0 {
            self.config.boost_rate_db_per_second
        } else {
            self.config.cut_rate_db_per_second
        };
        let maximum_step = rate * observation.elapsed_seconds;
        let step = gain_delta_db.clamp(-maximum_step, maximum_step);
        state.gain_db =
            (state.gain_db + step).clamp(-self.config.maximum_cut_db, self.config.maximum_boost_db);

        Decision::Adjust {
            previous_db,
            gain_db: state.gain_db,
        }
    }

    pub fn remove_stream(&mut self, stream_id: &str) -> Option<StreamState> {
        self.streams.remove(stream_id)
    }

    pub fn stream(&self, stream_id: &str) -> Option<StreamState> {
        self.streams.get(stream_id).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller() -> Controller {
        Controller::new(ControllerConfig::default()).unwrap()
    }

    #[test]
    fn rate_limits_boost_and_cut_independently() {
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
                gain_db: 1.0
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
                previous_db: 1.0,
                gain_db: -2.0
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
            Decision::Silence { gain_db: 2.0 }
        );
    }

    #[test]
    fn deadband_prevents_gain_hunting() {
        let mut controller = controller();
        assert_eq!(
            controller.observe(
                "music",
                Observation {
                    lufs: -13.5,
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
        assert_eq!(controller.stream("game").unwrap().gain_db, 1.0);
        assert_eq!(controller.stream("browser").unwrap().gain_db, -3.0);
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
        assert_eq!(controller.stream("quiet").unwrap().gain_db, 18.0);
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
        assert_eq!(controller.stream("source").unwrap().gain_db, 7.0);
        assert_eq!(
            controller.observe(
                "source",
                Observation {
                    lufs: -20.0,
                    elapsed_seconds: 1.0,
                }
            ),
            Decision::Hold { gain_db: 7.0 }
        );
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
            0.5
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
        controllers.apply_user_config(config);
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
