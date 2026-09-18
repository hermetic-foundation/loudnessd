// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{ControllerBank, Decision, Observation, SignalDomain};
use crate::pipewire::filter::{ConnectedFilter, MeterSnapshot};

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
                lufs: snapshot.source_loudness_lufs,
                elapsed_seconds,
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
            source_true_peak_dbtp: Some(-3.0),
            output_loudness_lufs: Some(-13.0),
            output_true_peak_dbtp: Some(-1.0),
            limiter_reduction_db: 0.0,
            maximum_limiter_reduction_db: 0.0,
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
        assert_eq!(endpoint.target_gain_db.get(), 1.0);
        assert_eq!(control.domain(), SignalDomain::Playback);
        assert_eq!(control.application_id(), "player");
        assert_eq!(control.stream_id(), "stream-1");
        assert_eq!(control.last_update(), Some(update));
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
}
