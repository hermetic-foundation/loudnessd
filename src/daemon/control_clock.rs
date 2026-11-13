// SPDX-License-Identifier: AGPL-3.0-or-later

use std::time::{Duration, Instant};

pub(super) struct ControlClock {
    last_update: Instant,
    interval: Duration,
}

impl ControlClock {
    pub(super) fn new(now: Instant, interval: Duration) -> Self {
        Self {
            last_update: now,
            interval,
        }
    }

    pub(super) fn take_elapsed(&mut self, now: Instant) -> Option<Duration> {
        let elapsed = now.saturating_duration_since(self.last_update);
        if elapsed < self.interval {
            return None;
        }
        self.last_update = now;
        Some(elapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn early_pipewire_wakeups_do_not_accelerate_gain_or_history() {
        let start = Instant::now();
        let mut clock = ControlClock::new(start, Duration::from_millis(100));
        for milliseconds in 1..100 {
            assert_eq!(
                clock.take_elapsed(start + Duration::from_millis(milliseconds)),
                None
            );
        }
        assert_eq!(
            clock.take_elapsed(start + Duration::from_millis(100)),
            Some(Duration::from_millis(100))
        );
        assert_eq!(clock.take_elapsed(start + Duration::from_millis(101)), None);
    }

    #[test]
    fn delayed_updates_account_for_real_time_without_replaying_ticks() {
        let start = Instant::now();
        let mut clock = ControlClock::new(start, Duration::from_millis(100));
        assert_eq!(
            clock.take_elapsed(start + Duration::from_millis(350)),
            Some(Duration::from_millis(350))
        );
        assert_eq!(clock.take_elapsed(start + Duration::from_millis(400)), None);
        assert_eq!(
            clock.take_elapsed(start + Duration::from_millis(450)),
            Some(Duration::from_millis(100))
        );
    }
}
