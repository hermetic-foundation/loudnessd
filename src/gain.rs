// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::VecDeque;

use crate::true_peak::TruePeakDetector;

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

const DEFAULT_LOOKAHEAD_SECONDS: f32 = 0.010;
const TRUE_PEAK_GUARD_DB: f32 = 0.1;

#[derive(Debug)]
pub struct TruePeakLimiter {
    threshold_linear: f32,
    release_coefficient: f32,
    detector: TruePeakDetector,
    delay: Vec<f32>,
    peak_window: VecDeque<(u64, f32)>,
    lookahead_frames: usize,
    write_frame: usize,
    sequence: u64,
    gain: f32,
}

impl TruePeakLimiter {
    pub fn new(
        threshold_dbtp: f32,
        release_seconds: f32,
        channel_count: usize,
        sample_rate: u32,
    ) -> Result<Self, &'static str> {
        if !threshold_dbtp.is_finite() || threshold_dbtp > 0.0 {
            return Err("limiter threshold must be finite and no greater than 0 dBTP");
        }
        if !release_seconds.is_finite() || release_seconds <= 0.0 {
            return Err("limiter release must be finite and positive");
        }
        if sample_rate == 0 {
            return Err("limiter sample rate must be positive");
        }
        let detector = TruePeakDetector::new(channel_count)?;
        let lookahead_frames =
            ((sample_rate as f32 * DEFAULT_LOOKAHEAD_SECONDS).round() as usize).max(1);
        Ok(Self {
            // A small internal guard absorbs envelope movement and f32
            // accumulation error while preserving the advertised ceiling.
            threshold_linear: 10.0_f32.powf((threshold_dbtp - TRUE_PEAK_GUARD_DB) / 20.0),
            release_coefficient: 1.0 - (-1.0 / (release_seconds * sample_rate as f32)).exp(),
            detector,
            delay: vec![0.0; lookahead_frames * channel_count],
            peak_window: VecDeque::with_capacity(lookahead_frames + 1),
            lookahead_frames,
            write_frame: 0,
            sequence: 0,
            gain: 1.0,
        })
    }

    pub fn channel_count(&self) -> usize {
        self.detector.channel_count()
    }

    pub fn latency_frames(&self) -> usize {
        self.lookahead_frames
    }

    pub fn reset(&mut self) {
        self.detector.reset();
        self.delay.fill(0.0);
        self.peak_window.clear();
        self.write_frame = 0;
        self.sequence = 0;
        self.gain = 1.0;
    }

    pub fn process_frame(
        &mut self,
        input: &[f32],
        output: &mut [f32],
    ) -> Result<f32, &'static str> {
        if input.len() != self.channel_count() || output.len() != self.channel_count() {
            return Err("limiter frame does not match the configured channel count");
        }

        let peak = self.detector.push_frame(input)?;
        while self
            .peak_window
            .back()
            .is_some_and(|(_, queued)| *queued <= peak)
        {
            self.peak_window.pop_back();
        }
        self.peak_window.push_back((self.sequence, peak));
        let earliest = self
            .sequence
            .saturating_sub(self.lookahead_frames.saturating_sub(1) as u64);
        while self
            .peak_window
            .front()
            .is_some_and(|(sequence, _)| *sequence < earliest)
        {
            self.peak_window.pop_front();
        }
        self.sequence = self.sequence.wrapping_add(1);

        let lookahead_peak = self.peak_window.front().map_or(0.0, |(_, peak)| *peak);
        let required_gain = if lookahead_peak > self.threshold_linear {
            self.threshold_linear / lookahead_peak
        } else {
            1.0
        };
        if required_gain < self.gain {
            self.gain = required_gain;
        } else {
            self.gain += (1.0 - self.gain) * self.release_coefficient;
        }

        let offset = self.write_frame * self.channel_count();
        for (channel, (input, output)) in input.iter().zip(output).enumerate() {
            let delayed = &mut self.delay[offset + channel];
            *output = *delayed * self.gain;
            *delayed = *input;
        }
        self.write_frame = (self.write_frame + 1) % self.lookahead_frames;
        Ok(self.gain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_and_measure_peak(limiter: &mut TruePeakLimiter, input: &[[f32; 2]]) -> (f32, f32) {
        let latency = limiter.latency_frames();
        let mut output = Vec::with_capacity(input.len() + latency);
        let mut maximum_reduction = 0.0_f32;
        for frame in input
            .iter()
            .copied()
            .chain(std::iter::repeat_n([0.0; 2], latency))
        {
            let mut limited = [0.0; 2];
            let gain = limiter.process_frame(&frame, &mut limited).unwrap();
            maximum_reduction = maximum_reduction.max(-20.0 * gain.log10());
            output.push(limited);
        }

        let mut detector = TruePeakDetector::new(2).unwrap();
        let output_peak = output
            .iter()
            .map(|frame| detector.push_frame(frame).unwrap())
            .fold(0.0_f32, f32::max);
        (output_peak, maximum_reduction)
    }

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
    fn true_peak_limiter_delays_low_level_audio_without_changing_it() {
        let mut limiter = TruePeakLimiter::new(-1.0, 0.1, 1, 1_000).unwrap();
        let latency = limiter.latency_frames();
        let input: Vec<_> = (0..32).map(|index| index as f32 / 100.0).collect();
        let mut output = Vec::new();

        for sample in input
            .iter()
            .copied()
            .chain(std::iter::repeat_n(0.0, latency))
        {
            let mut frame = [0.0];
            limiter.process_frame(&[sample], &mut frame).unwrap();
            output.push(frame[0]);
        }

        assert_eq!(&output[latency..latency + input.len()], &input);
    }

    #[test]
    fn true_peak_limiter_catches_inter_sample_overs() {
        let sample_rate = 48_000;
        let mut limiter = TruePeakLimiter::new(-1.0, 0.1, 1, sample_rate).unwrap();
        let latency = limiter.latency_frames();
        let waveform = [0.8, 0.8, -0.8, -0.8];
        let input: Vec<_> = (0..sample_rate)
            .map(|index| waveform[index as usize % waveform.len()])
            .collect();
        let mut output = Vec::with_capacity(input.len() + latency);
        let mut maximum_reduction = 0.0_f32;
        for sample in input
            .iter()
            .copied()
            .chain(std::iter::repeat_n(0.0, latency))
        {
            let mut frame = [0.0];
            let gain = limiter.process_frame(&[sample], &mut frame).unwrap();
            maximum_reduction = maximum_reduction.max(-20.0 * gain.log10());
            output.push(frame[0]);
        }

        let mut input_detector = TruePeakDetector::new(1).unwrap();
        let input_peak = input
            .iter()
            .map(|sample| input_detector.push_frame(&[*sample]).unwrap())
            .fold(0.0_f32, f32::max);
        let mut output_detector = TruePeakDetector::new(1).unwrap();
        let output_peak = output
            .iter()
            .map(|sample| output_detector.push_frame(&[*sample]).unwrap())
            .fold(0.0_f32, f32::max);
        let threshold = 10.0_f32.powf(-1.0 / 20.0);

        assert!(input_peak > threshold);
        assert!(
            output_peak <= threshold + 0.0001,
            "output peak was {output_peak}"
        );
        assert!(maximum_reduction > 0.0);
    }

    #[test]
    fn true_peak_limiter_links_stereo_channels() {
        let mut limiter = TruePeakLimiter::new(-1.0, 0.1, 2, 48_000).unwrap();
        let waveform = [0.8, 0.8, -0.8, -0.8];
        let input: Vec<_> = (0..48_000)
            .map(|index| [0.1, waveform[index % waveform.len()]])
            .collect();
        let (output_peak, reduction) = process_and_measure_peak(&mut limiter, &input);
        let threshold = 10.0_f32.powf(-1.0 / 20.0);

        assert!(
            output_peak <= threshold + 0.0001,
            "output peak was {output_peak}"
        );
        assert!(reduction > 0.0);
    }

    #[test]
    fn true_peak_limiter_contains_deterministic_transients_without_growing() {
        let mut limiter = TruePeakLimiter::new(-1.0, 0.1, 2, 48_000).unwrap();
        let queue_capacity = limiter.peak_window.capacity();
        let delay_capacity = limiter.delay.capacity();
        let mut state = 0x1234_5678_u32;
        let input: Vec<_> = (0..96_000)
            .map(|index| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let random = (state as f32 / u32::MAX as f32) * 2.0 - 1.0;
                let transient = if index % 997 == 0 { 1.5 } else { random * 0.9 };
                [transient, -transient * 0.73]
            })
            .collect();
        let (output_peak, reduction) = process_and_measure_peak(&mut limiter, &input);
        let threshold = 10.0_f32.powf(-1.0 / 20.0);

        assert!(
            output_peak <= threshold + 0.0001,
            "output peak was {output_peak}"
        );
        assert!(reduction > 0.0);
        assert_eq!(limiter.peak_window.capacity(), queue_capacity);
        assert_eq!(limiter.delay.capacity(), delay_capacity);
    }

    #[test]
    fn limiter_rejects_unsafe_configuration() {
        assert!(TruePeakLimiter::new(1.0, 0.1, 2, 48_000).is_err());
        assert!(TruePeakLimiter::new(-1.0, 0.0, 2, 48_000).is_err());
        assert!(TruePeakLimiter::new(-1.0, 0.1, 0, 48_000).is_err());
        assert!(TruePeakLimiter::new(-1.0, 0.1, 2, 0).is_err());
    }

    #[test]
    fn limiter_reset_discards_delay_and_gain_history() {
        let mut limiter = TruePeakLimiter::new(-1.0, 0.1, 1, 1_000).unwrap();
        let mut output = [0.0];
        for _ in 0..32 {
            limiter.process_frame(&[2.0], &mut output).unwrap();
        }
        limiter.reset();

        let gain = limiter.process_frame(&[0.0], &mut output).unwrap();
        assert_eq!(gain, 1.0);
        assert_eq!(output, [0.0]);
    }
}
