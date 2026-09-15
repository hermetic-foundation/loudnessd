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

        if frame_count == 1 {
            let linear_gain = 10.0_f32.powf(target_gain_db / 20.0);
            for sample in samples {
                *sample *= linear_gain;
            }
            self.gain_db = target_gain_db;
            return Ok(());
        }

        let denominator = (frame_count - 1) as f32;
        for (frame_index, frame) in samples.chunks_exact_mut(channels).enumerate() {
            let progress = frame_index as f32 / denominator;
            let gain_db = self.gain_db + (target_gain_db - self.gain_db) * progress;
            let linear_gain = 10.0_f32.powf(gain_db / 20.0);
            for sample in frame {
                *sample *= linear_gain;
            }
        }
        self.gain_db = target_gain_db;
        Ok(())
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
}
