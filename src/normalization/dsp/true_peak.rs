// SPDX-License-Identifier: AGPL-3.0-or-later

//! Streaming ITU-R BS.1770 four-times-oversampled true-peak detection.
//!
//! The coefficient layout follows BS.1770-4 Annex 2 and the implementation
//! used by `ebur128-stream`. State is allocated during construction; feeding
//! audio performs no allocation.

const PHASES: usize = 4;
const TAPS: usize = 12;

#[rustfmt::skip]
#[allow(clippy::excessive_precision)]
const COEFFICIENTS: [[f32; TAPS]; PHASES] = [
    [
         0.001_708_984_4,  0.010_986_328_0, -0.019_653_320_0,
         0.033_203_125_0, -0.059_448_242_0,  0.137_329_100_0,
         0.972_167_970_0, -0.102_294_920_0,  0.047_607_422_0,
        -0.026_611_328_0,  0.014_892_578_0, -0.008_300_781_3,
    ],
    [
        -0.029_174_805_0,  0.029_296_875_0, -0.051_757_812_0,
         0.089_111_328_0, -0.166_503_910_0,  0.465_087_890_0,
         0.779_785_160_0, -0.200_317_380_0,  0.101_562_500_0,
        -0.058_227_540_0,  0.033_081_055_0, -0.018_920_898_0,
    ],
    [
        -0.018_920_898_0,  0.033_081_055_0, -0.058_227_540_0,
         0.101_562_500_0, -0.200_317_380_0,  0.779_785_160_0,
         0.465_087_890_0, -0.166_503_910_0,  0.089_111_328_0,
        -0.051_757_812_0,  0.029_296_875_0, -0.029_174_805_0,
    ],
    [
        -0.008_300_781_3,  0.014_892_578_0, -0.026_611_328_0,
         0.047_607_422_0, -0.102_294_920_0,  0.972_167_970_0,
         0.137_329_100_0, -0.059_448_242_0,  0.033_203_125_0,
        -0.019_653_320_0,  0.010_986_328_0,  0.001_708_984_4,
    ],
];

#[derive(Debug)]
struct ChannelState {
    delay: [f32; TAPS],
    write_index: usize,
}

impl Default for ChannelState {
    fn default() -> Self {
        Self {
            delay: [0.0; TAPS],
            write_index: 0,
        }
    }
}

impl ChannelState {
    fn reset(&mut self) {
        self.delay.fill(0.0);
        self.write_index = 0;
    }

    #[inline]
    fn push(&mut self, sample: f32) -> f32 {
        self.delay[self.write_index] = sample;
        self.write_index = (self.write_index + 1) % TAPS;

        let mut peak = sample.abs();
        for coefficients in &COEFFICIENTS {
            let mut interpolated = 0.0;
            for (tap, coefficient) in coefficients.iter().enumerate() {
                let index = (self.write_index + TAPS - 1 - tap) % TAPS;
                interpolated += coefficient * self.delay[index];
            }
            peak = peak.max(interpolated.abs());
        }
        peak
    }
}

#[derive(Debug)]
pub(crate) struct TruePeakDetector {
    channels: Vec<ChannelState>,
}

impl TruePeakDetector {
    pub(crate) fn new(channel_count: usize) -> Result<Self, &'static str> {
        if channel_count == 0 {
            return Err("true-peak detector requires at least one channel");
        }
        Ok(Self {
            channels: (0..channel_count)
                .map(|_| ChannelState::default())
                .collect(),
        })
    }

    pub(crate) fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub(crate) fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.reset();
        }
    }

    /// Returns the largest raw or four-times-oversampled value produced by the
    /// new linked-channel frame.
    #[inline]
    pub(crate) fn push_frame(&mut self, frame: &[f32]) -> Result<f32, &'static str> {
        if frame.len() != self.channels.len() {
            return Err("true-peak frame does not match the configured channel count");
        }
        Ok(self
            .channels
            .iter_mut()
            .zip(frame)
            .map(|(state, sample)| state.push(*sample))
            .fold(0.0_f32, f32::max))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_channel_layouts() {
        assert!(TruePeakDetector::new(0).is_err());

        let mut detector = TruePeakDetector::new(2).unwrap();
        assert!(detector.push_frame(&[0.0]).is_err());
        assert_eq!(detector.channel_count(), 2);
    }

    #[test]
    fn tracks_the_loudest_channel() {
        let mut detector = TruePeakDetector::new(2).unwrap();
        let peak = detector.push_frame(&[0.25, -0.75]).unwrap();

        assert!(peak >= 0.75);
    }

    #[test]
    fn detects_an_inter_sample_peak_above_every_raw_sample() {
        let mut detector = TruePeakDetector::new(1).unwrap();
        let waveform = [0.8, 0.8, -0.8, -0.8];
        let mut true_peak = 0.0_f32;
        for index in 0..256 {
            true_peak = true_peak.max(
                detector
                    .push_frame(&[waveform[index % waveform.len()]])
                    .unwrap(),
            );
        }

        assert!(
            true_peak > 0.9,
            "expected an inter-sample over, got {true_peak}"
        );
    }

    #[test]
    fn full_scale_dc_settles_near_full_scale() {
        let mut detector = TruePeakDetector::new(1).unwrap();
        let mut settled_peak = 0.0;
        for _ in 0..256 {
            settled_peak = detector.push_frame(&[1.0]).unwrap();
        }

        assert!((settled_peak - 1.0).abs() < 0.01, "got {settled_peak}");
    }

    #[test]
    fn reset_discards_filter_history() {
        let mut detector = TruePeakDetector::new(1).unwrap();
        for _ in 0..32 {
            detector.push_frame(&[1.0]).unwrap();
        }
        detector.reset();

        assert_eq!(detector.push_frame(&[0.0]).unwrap(), 0.0);
    }
}
