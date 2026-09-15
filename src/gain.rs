// SPDX-License-Identifier: AGPL-3.0-or-later

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GainStage {
    gain_db: f32,
}

impl Default for GainStage {
    fn default() -> Self {
        Self { gain_db: 0.0 }
    }
}

impl GainStage {
    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    pub fn process_interleaved(
        &mut self,
        samples: &mut [f32],
        channels: usize,
        target_gain_db: f32,
    ) -> Result<(), &'static str> {
        if channels == 0 {
            return Err("channel count must be positive");
        }
        if !samples.len().is_multiple_of(channels) {
            return Err("sample count must contain complete frames");
        }
        if !target_gain_db.is_finite() {
            return Err("target gain must be finite");
        }

        let frame_count = samples.len() / channels;
        if frame_count == 0 {
            return Ok(());
        }

        for (frame_index, frame) in samples.chunks_exact_mut(channels).enumerate() {
            let linear_gain = self.linear_gain(frame_index, frame_count, target_gain_db);
            for sample in frame {
                *sample *= linear_gain;
            }
        }
        self.gain_db = target_gain_db;
        Ok(())
    }

    pub fn process_planar(
        &mut self,
        channels: &mut [&mut [f32]],
        target_gain_db: f32,
    ) -> Result<(), &'static str> {
        let Some(frame_count) = channels.first().map(|channel| channel.len()) else {
            return Err("channel count must be positive");
        };
        if channels.iter().any(|channel| channel.len() != frame_count) {
            return Err("channels must contain the same number of frames");
        }
        if !target_gain_db.is_finite() {
            return Err("target gain must be finite");
        }
        if frame_count == 0 {
            return Ok(());
        }

        for frame_index in 0..frame_count {
            let linear_gain = self.linear_gain(frame_index, frame_count, target_gain_db);
            for channel in channels.iter_mut() {
                channel[frame_index] *= linear_gain;
            }
        }
        self.gain_db = target_gain_db;
        Ok(())
    }

    fn linear_gain(&self, frame_index: usize, frame_count: usize, target_gain_db: f32) -> f32 {
        let gain_db = if frame_count == 1 {
            target_gain_db
        } else {
            let progress = frame_index as f32 / (frame_count - 1) as f32;
            self.gain_db + (target_gain_db - self.gain_db) * progress
        };
        10.0_f32.powf(gain_db / 20.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeakLimiter {
    threshold_linear: f32,
    release_seconds: f32,
    gain: f32,
}

impl PeakLimiter {
    pub fn new(threshold_dbfs: f32, release_seconds: f32) -> Result<Self, &'static str> {
        if !threshold_dbfs.is_finite() || threshold_dbfs > 0.0 {
            return Err("limiter threshold must be finite and no greater than 0 dBFS");
        }
        if !release_seconds.is_finite() || release_seconds <= 0.0 {
            return Err("limiter release must be finite and positive");
        }
        Ok(Self {
            threshold_linear: 10.0_f32.powf(threshold_dbfs / 20.0),
            release_seconds,
            gain: 1.0,
        })
    }

    pub fn gain_for_peak(&mut self, peak: f32, sample_rate: u32) -> f32 {
        if !peak.is_finite() || sample_rate == 0 {
            return self.gain;
        }
        let required = if peak > self.threshold_linear {
            self.threshold_linear / peak
        } else {
            1.0
        };
        if required < self.gain {
            self.gain = required;
        } else {
            let release = 1.0 - (-1.0 / (self.release_seconds * sample_rate as f32)).exp();
            self.gain += (1.0 - self.gain) * release;
        }
        self.gain
    }
}

impl Default for PeakLimiter {
    fn default() -> Self {
        Self::new(-1.0, 0.1).expect("built-in limiter settings are valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_one_gain_to_every_channel_in_a_frame() {
        let mut stage = GainStage::default();
        let mut samples = [1.0, 0.5, 1.0, 0.5];

        stage.process_interleaved(&mut samples, 2, -6.0206).unwrap();

        assert!((samples[0] - 1.0).abs() < 0.0001);
        assert!((samples[1] - 0.5).abs() < 0.0001);
        assert!((samples[2] - 0.5).abs() < 0.0001);
        assert!((samples[3] - 0.25).abs() < 0.0001);
        assert_eq!(stage.gain_db(), -6.0206);
    }

    #[test]
    fn begins_the_next_buffer_without_a_gain_discontinuity() {
        let mut stage = GainStage::default();
        let mut first = [1.0; 4];
        stage.process_interleaved(&mut first, 1, -6.0206).unwrap();

        let mut second = [1.0; 2];
        stage.process_interleaved(&mut second, 1, -6.0206).unwrap();

        assert!((first[3] - second[0]).abs() < 0.0001);
    }

    #[test]
    fn rejects_incomplete_frames_without_modifying_audio() {
        let mut stage = GainStage::default();
        let mut samples = [1.0, 0.5, 0.25];

        assert!(stage.process_interleaved(&mut samples, 2, 0.0).is_err());
        assert_eq!(samples, [1.0, 0.5, 0.25]);
    }

    #[test]
    fn planar_and_interleaved_layouts_apply_the_same_ramp() {
        let mut interleaved_stage = GainStage::default();
        let mut interleaved = [1.0, 0.5, 1.0, 0.5];
        interleaved_stage
            .process_interleaved(&mut interleaved, 2, -6.0206)
            .unwrap();

        let mut planar_stage = GainStage::default();
        let mut left = [1.0, 1.0];
        let mut right = [0.5, 0.5];
        planar_stage
            .process_planar(&mut [&mut left, &mut right], -6.0206)
            .unwrap();

        assert_eq!(left, [interleaved[0], interleaved[2]]);
        assert_eq!(right, [interleaved[1], interleaved[3]]);
        assert_eq!(planar_stage, interleaved_stage);
    }

    #[test]
    fn rejects_mismatched_planar_channels_without_modifying_audio() {
        let mut stage = GainStage::default();
        let mut left = [1.0, 0.5];
        let mut right = [0.25];

        assert!(
            stage
                .process_planar(&mut [&mut left, &mut right], -6.0)
                .is_err()
        );
        assert_eq!(left, [1.0, 0.5]);
        assert_eq!(right, [0.25]);
    }

    #[test]
    fn limiter_catches_peaks_and_releases_smoothly() {
        let mut limiter = PeakLimiter::new(-1.0, 0.1).unwrap();
        let threshold = 10.0_f32.powf(-1.0 / 20.0);

        let attack_gain = limiter.gain_for_peak(2.0, 48_000);
        assert!((2.0 * attack_gain - threshold).abs() < 0.0001);

        let release_gain = limiter.gain_for_peak(0.1, 48_000);
        assert!(release_gain > attack_gain);
        assert!(release_gain < 1.0);
    }

    #[test]
    fn limiter_rejects_unsafe_configuration() {
        assert!(PeakLimiter::new(1.0, 0.1).is_err());
        assert!(PeakLimiter::new(-1.0, 0.0).is_err());
    }
}
