// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashMap;

use crate::{ApplicationPolicyOverride, SignalDomain, UserConfig};

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    baseline: UserConfig,
    overrides: HashMap<String, ApplicationPolicyOverride>,
}

impl RuntimeConfig {
    pub fn new(baseline: UserConfig) -> Self {
        Self {
            baseline,
            overrides: HashMap::new(),
        }
    }

    pub fn replace_baseline(&mut self, baseline: UserConfig) {
        self.baseline = baseline;
    }

    pub fn set(&mut self, application_id: impl Into<String>, domain: SignalDomain, enabled: bool) {
        let policy = self.overrides.entry(application_id.into()).or_default();
        match domain {
            SignalDomain::Playback => policy.playback = Some(enabled),
            SignalDomain::Capture => policy.capture = Some(enabled),
        }
    }

    pub fn reset(&mut self, application_id: &str) -> bool {
        self.overrides.remove(application_id).is_some()
    }

    pub fn effective(&self) -> UserConfig {
        let mut effective = self.baseline.clone();
        for (application_id, overlay) in &self.overrides {
            let policy = effective
                .applications
                .entry(application_id.clone())
                .or_default();
            if overlay.playback.is_some() {
                policy.playback = overlay.playback;
            }
            if overlay.capture.is_some() {
                policy.capture = overlay.capture;
            }
        }
        effective
    }

    pub fn export_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(&self.effective())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline() -> UserConfig {
        UserConfig::from_toml(
            r#"
            [defaults]
            playback = true
            capture = true

            [applications.player]
            capture = false
            "#,
        )
        .unwrap()
    }

    #[test]
    fn overlays_each_direction_without_mutating_the_other() {
        let mut config = RuntimeConfig::new(baseline());
        config.set("player", SignalDomain::Playback, false);

        let player = config.effective().applications["player"];
        assert_eq!(player.playback, Some(false));
        assert_eq!(player.capture, Some(false));
    }

    #[test]
    fn reset_reveals_the_immutable_baseline() {
        let mut config = RuntimeConfig::new(baseline());
        config.set("player", SignalDomain::Capture, true);
        assert!(config.reset("player"));

        assert_eq!(
            config.effective().applications["player"].capture,
            Some(false)
        );
    }

    #[test]
    fn reload_keeps_ephemeral_overrides() {
        let mut config = RuntimeConfig::new(baseline());
        config.set("player", SignalDomain::Playback, false);
        config.replace_baseline(UserConfig::default());

        assert_eq!(
            config.effective().applications["player"].playback,
            Some(false)
        );
    }

    #[test]
    fn export_round_trips_the_effective_configuration() {
        let mut config = RuntimeConfig::new(baseline());
        config.set("player", SignalDomain::Playback, false);

        let exported = config.export_toml().unwrap();
        assert_eq!(
            UserConfig::from_toml(&exported).unwrap(),
            config.effective()
        );
    }
}
