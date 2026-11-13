// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::VecDeque;

pub(crate) const MAX_HISTORY_WINDOW_SECONDS: f32 = 300.0;

// At the 100 ms control cadence even the largest window needs only 3,001
// records. This additional ceiling bounds memory if a caller floods updates.
const MAX_HISTORY_RECORDS: usize = 4096;

#[derive(Clone, Copy, Debug)]
struct Interval {
    lufs: Option<f32>,
    seconds: f64,
}

/// Duration-weighted, nearest-rank loudness history, containing no audio data.
/// Gated/invalid observations advance time but contribute no percentile weight.
/// Excess updates beyond the record ceiling discard the oldest intervals.
#[derive(Debug)]
pub(crate) struct LoudnessHistory {
    intervals: VecDeque<Interval>,
    seconds: f64,
    window_seconds: f64,
}

impl LoudnessHistory {
    pub(crate) fn new(window_seconds: f32) -> Self {
        Self {
            intervals: VecDeque::new(),
            seconds: 0.0,
            window_seconds: f64::from(window_seconds),
        }
    }

    pub(crate) fn set_window_seconds(&mut self, window_seconds: f32) {
        self.window_seconds = f64::from(window_seconds);
        self.trim();
    }

    pub(crate) fn observe(&mut self, lufs: Option<f32>, elapsed_seconds: f32) {
        if !elapsed_seconds.is_finite() || elapsed_seconds <= 0.0 {
            return;
        }

        let seconds = f64::from(elapsed_seconds).min(self.window_seconds);
        if f64::from(elapsed_seconds) >= self.window_seconds {
            self.intervals.clear();
            self.seconds = 0.0;
        }
        let lufs = lufs.filter(|value| value.is_finite());
        if let Some(last) = self.intervals.back_mut().filter(|last| last.lufs == lufs) {
            last.seconds += seconds;
        } else {
            if self.intervals.len() == MAX_HISTORY_RECORDS {
                self.seconds -= self.intervals.pop_front().unwrap().seconds;
            }
            self.intervals.push_back(Interval { lufs, seconds });
        }
        self.seconds += seconds;
        self.trim();
    }

    fn trim(&mut self) {
        while self.seconds > self.window_seconds {
            let excess = self.seconds - self.window_seconds;
            let Some(first) = self.intervals.front_mut() else {
                self.seconds = 0.0;
                break;
            };
            if first.seconds <= excess {
                self.seconds -= self.intervals.pop_front().unwrap().seconds;
            } else {
                first.seconds -= excess;
                self.seconds = self.window_seconds;
            }
        }
    }

    pub(crate) fn percentile(&self, percentile: f32) -> Option<f32> {
        if !percentile.is_finite() || percentile <= 0.0 || percentile > 100.0 {
            return None;
        }
        let mut weighted: Vec<_> = self
            .intervals
            .iter()
            .filter_map(|interval| interval.lufs.map(|lufs| (lufs, interval.seconds)))
            .collect();
        weighted.sort_unstable_by(|left, right| left.0.total_cmp(&right.0));
        let maximum = weighted.last()?.0;
        if percentile == 100.0 {
            return Some(maximum);
        }

        let total: f64 = weighted.iter().map(|(_, seconds)| seconds).sum();
        let rank = total * f64::from(percentile) / 100.0;
        // Durations arrive as f32. Keep exact rank boundaries stable across
        // equivalent chunkings such as 9 seconds versus ninety 100 ms updates.
        let tolerance = total * f64::from(f32::EPSILON);
        let mut cumulative = 0.0;
        for (lufs, seconds) in weighted {
            cumulative += seconds;
            if cumulative + tolerance >= rank {
                return Some(lufs);
            }
        }
        Some(maximum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p90_is_a_rank_not_the_loudest_block() {
        let mut history = LoudnessHistory::new(30.0);
        history.observe(Some(-30.0), 9.0);
        history.observe(Some(-4.0), 1.0);
        assert_eq!(history.percentile(90.0), Some(-30.0));
        assert_eq!(history.percentile(91.0), Some(-4.0));
        assert_eq!(history.percentile(100.0), Some(-4.0));

        history.observe(Some(-4.0), 0.1);
        assert_eq!(history.percentile(90.0), Some(-4.0));
    }

    #[test]
    fn percentile_uses_elapsed_time_instead_of_update_count() {
        let mut history = LoudnessHistory::new(30.0);
        history.observe(Some(-30.0), 9.9);
        for _ in 0..10 {
            history.observe(Some(-4.0), 0.01);
        }
        assert_eq!(history.percentile(90.0), Some(-30.0));
    }

    #[test]
    fn equivalent_chunking_preserves_rank_and_expiration() {
        let mut whole = LoudnessHistory::new(30.0);
        let mut chunks = LoudnessHistory::new(30.0);
        for (lufs, seconds) in [(Some(-5.0), 10), (Some(-30.0), 20), (None, 8)] {
            whole.observe(lufs, seconds as f32);
            for _ in 0..seconds * 10 {
                chunks.observe(lufs, 0.1);
            }
            for percentile in [1.0, 50.0, 90.0, 100.0] {
                assert_eq!(whole.percentile(percentile), chunks.percentile(percentile));
            }
            assert!((whole.seconds - chunks.seconds).abs() < 0.00001);
        }
        whole.observe(None, 22.0);
        for _ in 0..220 {
            chunks.observe(None, 0.1);
        }
        assert_eq!(whole.percentile(90.0), None);
        assert_eq!(chunks.percentile(90.0), None);
    }

    #[test]
    fn partially_expired_intervals_lose_only_the_expired_weight() {
        let mut history = LoudnessHistory::new(10.0);
        history.observe(Some(-5.0), 2.0);
        history.observe(Some(-30.0), 8.0);
        assert_eq!(history.percentile(90.0), Some(-5.0));
        history.observe(Some(-30.0), 1.0);
        assert_eq!(history.percentile(90.0), Some(-30.0));
        assert_eq!(history.percentile(100.0), Some(-5.0));
        history.observe(Some(-30.0), 1.0);
        assert_eq!(history.percentile(100.0), Some(-30.0));
    }

    #[test]
    fn gaps_expire_values_without_adding_silence_to_the_distribution() {
        let mut history = LoudnessHistory::new(30.0);
        history.observe(Some(-20.0), 1.0);
        history.observe(None, 29.0);
        assert_eq!(history.percentile(90.0), Some(-20.0));
        history.observe(Some(f32::NEG_INFINITY), 1.0);
        assert_eq!(history.percentile(90.0), None);
        history.observe(Some(f32::NAN), 1.0);
        assert_eq!(history.percentile(90.0), None);
    }

    #[test]
    fn invalid_elapsed_does_not_advance_or_insert() {
        let mut history = LoudnessHistory::new(30.0);
        history.observe(Some(-20.0), 1.0);
        for seconds in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            history.observe(Some(-4.0), seconds);
        }
        assert_eq!(history.seconds, 1.0);
        assert_eq!(history.percentile(100.0), Some(-20.0));
    }

    #[test]
    fn long_intervals_and_window_changes_are_bounded() {
        let mut history = LoudnessHistory::new(30.0);
        history.observe(Some(-4.0), 10.0);
        history.observe(Some(-30.0), f32::MAX);
        assert_eq!(history.seconds, 30.0);
        assert_eq!(history.percentile(100.0), Some(-30.0));
        history.set_window_seconds(5.0);
        assert_eq!(history.seconds, 5.0);
        history.observe(None, 5.0);
        assert_eq!(history.percentile(90.0), None);
    }

    #[test]
    fn update_flood_has_a_hard_metadata_bound() {
        let mut history = LoudnessHistory::new(MAX_HISTORY_WINDOW_SECONDS);
        for index in 0..MAX_HISTORY_RECORDS * 2 {
            history.observe(Some(if index % 2 == 0 { -30.0 } else { -10.0 }), 0.001);
        }
        assert_eq!(history.intervals.len(), MAX_HISTORY_RECORDS);
        assert!(history.seconds <= f64::from(MAX_HISTORY_WINDOW_SECONDS));
        assert_eq!(history.percentile(90.0), Some(-10.0));
    }

    #[test]
    fn invalid_percentiles_and_empty_history_have_no_reference() {
        let mut history = LoudnessHistory::new(30.0);
        assert_eq!(history.percentile(90.0), None);
        history.observe(Some(-20.0), 1.0);
        for percentile in [0.0, -1.0, 101.0, f32::NAN, f32::INFINITY] {
            assert_eq!(history.percentile(percentile), None);
        }
    }
}
