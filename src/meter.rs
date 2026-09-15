// SPDX-License-Identifier: AGPL-3.0-or-later

use ebur128_stream::{Analyzer, AnalyzerBuilder, Channel, Error, Mode};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeterReading {
    pub loudness_lufs: f32,
    pub true_peak_dbtp: Option<f32>,
    pub using_short_term: bool,
}

#[derive(Debug)]
pub struct LoudnessMeter {
    analyzer: Analyzer,
}

impl LoudnessMeter {
    pub fn new(sample_rate: u32, channels: &[Channel]) -> Result<Self, Error> {
        let analyzer = AnalyzerBuilder::new()
            .sample_rate(sample_rate)
            .channels(channels)
            .modes(Mode::Momentary | Mode::ShortTerm | Mode::TruePeak)
            .build()?;
        Ok(Self { analyzer })
    }

    pub fn push_interleaved(&mut self, samples: &[f32]) -> Result<Option<MeterReading>, Error> {
        self.analyzer.push_interleaved(samples)?;
        let snapshot = self.analyzer.snapshot();
        let short_term = snapshot.short_term_lufs();
        let loudness = short_term.or_else(|| snapshot.momentary_lufs());
        Ok(loudness.map(|loudness_lufs| MeterReading {
            loudness_lufs: loudness_lufs as f32,
            true_peak_dbtp: snapshot.true_peak_dbtp().map(|peak| peak as f32),
            using_short_term: short_term.is_some(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;

    use super::*;

    fn stereo_sine(sample_rate: u32, seconds: u32, amplitude: f32) -> Vec<f32> {
        let frames = sample_rate as usize * seconds as usize;
        let mut samples = Vec::with_capacity(frames * 2);
        for frame in 0..frames {
            let sample = (TAU * 1_000.0 * frame as f32 / sample_rate as f32).sin() * amplitude;
            samples.extend_from_slice(&[sample, sample]);
        }
        samples
    }

    #[test]
    fn silence_does_not_produce_a_loudness_value() {
        let mut meter = LoudnessMeter::new(48_000, &[Channel::Left, Channel::Right]).unwrap();
        assert_eq!(meter.push_interleaved(&vec![0.0; 96_000]).unwrap(), None);
    }

    #[test]
    fn arbitrary_chunks_produce_stable_short_term_loudness() {
        let samples = stereo_sine(48_000, 4, 0.1);
        let mut whole = LoudnessMeter::new(48_000, &[Channel::Left, Channel::Right]).unwrap();
        let expected = whole.push_interleaved(&samples).unwrap().unwrap();

        let mut chunked = LoudnessMeter::new(48_000, &[Channel::Left, Channel::Right]).unwrap();
        let mut actual = None;
        for chunk in samples.chunks(514) {
            actual = chunked.push_interleaved(chunk).unwrap().or(actual);
        }
        let actual = actual.unwrap();

        assert!(expected.using_short_term);
        assert!(actual.using_short_term);
        assert!((expected.loudness_lufs - actual.loudness_lufs).abs() < 0.001);
        assert!(expected.true_peak_dbtp.is_some());
    }
}
