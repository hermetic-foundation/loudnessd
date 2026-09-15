// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    ControllerBank, Decision, Observation, SignalDomain,
    pipewire_filter::{ConnectedFilter, MeterSnapshot},
};

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
    pub sequence: u64,
    pub decision: Decision,
}

#[derive(Debug)]
pub struct StreamControl {
    domain: SignalDomain,
    application_id: String,
    stream_id: String,
    last_sequence: Option<u64>,
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
        }
    }

    pub fn update<E: NormalizationEndpoint>(
        &mut self,
        endpoint: &E,
        controllers: &mut ControllerBank,
        elapsed_seconds: f32,
    ) -> Result<Option<ControlUpdate>, &'static str> {
        let Some(snapshot) = endpoint.latest_meter_snapshot() else {
            return Ok(None);
        };
        if self.last_sequence == Some(snapshot.sequence) {
            return Ok(None);
        }

        let decision = controllers.observe(
            self.domain,
            &self.application_id,
            &self.stream_id,
            Observation {
                lufs: snapshot.loudness_lufs,
                elapsed_seconds,
            },
        );
        endpoint.set_target_gain_db(decision.target_gain_db())?;
        self.last_sequence = Some(snapshot.sequence);
        Ok(Some(ControlUpdate {
            sequence: snapshot.sequence,
            decision,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::ApplicationPolicy;

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

    #[test]
    fn applies_each_meter_sequence_once() {
        let endpoint = FakeEndpoint::default();
        endpoint.snapshot.set(Some(MeterSnapshot {
            sequence: 1,
            loudness_lufs: -23.0,
        }));
        let mut control = StreamControl::new(SignalDomain::Playback, "player", "stream-1");
        let mut controllers = ControllerBank::defaults();

        let update = control
            .update(&endpoint, &mut controllers, 1.0)
            .unwrap()
            .unwrap();

        assert_eq!(update.sequence, 1);
        assert_eq!(endpoint.target_gain_db.get(), 1.0);
        assert_eq!(
            control.update(&endpoint, &mut controllers, 1.0).unwrap(),
            None
        );
        assert_eq!(
            controllers
                .stream(SignalDomain::Playback, "stream-1")
                .unwrap()
                .gain_db,
            1.0
        );
    }

    #[test]
    fn bypassed_direction_restores_unity_gain() {
        let endpoint = FakeEndpoint {
            snapshot: Cell::new(Some(MeterSnapshot {
                sequence: 1,
                loudness_lufs: -23.0,
            })),
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
}
