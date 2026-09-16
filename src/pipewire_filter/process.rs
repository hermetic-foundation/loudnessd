// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    os::raw::c_void,
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use ebur128_stream::Channel;
use pipewire::sys;

use super::{MeterSnapshot, PortDirection};
use crate::{
    gain::{GainStage, TruePeakLimiter},
    meter::{LoudnessMeter, MeterReading},
};

const MAX_METER_CHANNELS: usize = 64;
const PREPARED_SAMPLE_RATES: [u32; 7] = [22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 192_000];

pub(super) struct FilterCallbackData {
    ports: Vec<CallbackPort>,
    target_gain_bits: AtomicU32,
    default_sample_rate: Option<u32>,
    processing_states: Vec<ProcessingState>,
    active_processing_state: Option<usize>,
    latest_metrics: PublishedMetrics,
    maximum_limiter_reduction_db: f32,
}

struct ProcessingState {
    sample_rate: u32,
    source: LoudnessMeter,
    output: LoudnessMeter,
    limiter: TruePeakLimiter,
}

impl ProcessingState {
    fn new(sample_rate: u32, channels: &[Channel]) -> Result<Self, ebur128_stream::Error> {
        Ok(Self {
            sample_rate,
            source: LoudnessMeter::new(sample_rate, channels)?,
            output: LoudnessMeter::new(sample_rate, channels)?,
            limiter: TruePeakLimiter::new(-1.0, 0.1, channels.len(), sample_rate)
                .expect("the built-in true-peak limiter settings are valid"),
        })
    }

    fn reset(&mut self) {
        self.source.reset();
        self.output.reset();
        self.limiter.reset();
    }
}

impl Default for FilterCallbackData {
    fn default() -> Self {
        Self {
            ports: Vec::new(),
            target_gain_bits: AtomicU32::new(0.0_f32.to_bits()),
            default_sample_rate: None,
            processing_states: Vec::new(),
            active_processing_state: None,
            latest_metrics: PublishedMetrics::default(),
            maximum_limiter_reduction_db: 0.0,
        }
    }
}

impl FilterCallbackData {
    pub(super) fn add_port(
        &mut self,
        raw: NonNull<c_void>,
        direction: PortDirection,
        channel: String,
    ) {
        self.ports.push(CallbackPort {
            raw,
            direction,
            channel,
            gain: GainStage::default(),
        });
    }

    pub(super) fn enable_meter(&mut self, sample_rate: u32) -> Result<(), ebur128_stream::Error> {
        let channels: Vec<_> = self
            .ports
            .iter()
            .filter(|port| port.direction == PortDirection::Input)
            .map(|port| meter_channel(&port.channel))
            .collect();
        let mut sample_rates = Vec::with_capacity(PREPARED_SAMPLE_RATES.len() + 1);
        sample_rates.push(sample_rate);
        sample_rates.extend(
            PREPARED_SAMPLE_RATES
                .into_iter()
                .filter(|prepared| *prepared != sample_rate),
        );
        self.processing_states = sample_rates
            .into_iter()
            .map(|rate| ProcessingState::new(rate, &channels))
            .collect::<Result<_, _>>()?;
        self.default_sample_rate = Some(sample_rate);
        self.active_processing_state = None;
        Ok(())
    }

    pub(super) fn set_target_gain_db(&self, target_gain_db: f32) -> Result<(), &'static str> {
        self.target_gain_bits
            .store(target_gain_bits(target_gain_db)?, Ordering::Relaxed);
        Ok(())
    }

    pub(super) fn target_gain_db(&self) -> f32 {
        f32::from_bits(self.target_gain_bits.load(Ordering::Relaxed))
    }

    pub(super) fn latest_meter_snapshot(&self) -> Option<MeterSnapshot> {
        self.latest_metrics.read()
    }
}

pub(super) struct CallbackPort {
    pub(super) raw: NonNull<c_void>,
    pub(super) direction: PortDirection,
    pub(super) channel: String,
    pub(super) gain: GainStage,
}

pub(super) struct CycleBuffers {
    pointers: [*mut f32; MAX_METER_CHANNELS],
    pub(super) len: usize,
}

impl CycleBuffers {
    fn acquire(ports: &[CallbackPort], sample_count: u32) -> Option<Self> {
        Self::acquire_with(ports, |port| {
            // SAFETY: PipeWire owns the port and makes its DSP buffer valid for
            // sample_count f32 samples for the duration of this process cycle.
            unsafe { sys::pw_filter_get_dsp_buffer(port.as_ptr(), sample_count) }.cast::<f32>()
        })
    }

    pub(super) fn acquire_with(
        ports: &[CallbackPort],
        mut acquire: impl FnMut(NonNull<c_void>) -> *mut f32,
    ) -> Option<Self> {
        if ports.len() > MAX_METER_CHANNELS {
            return None;
        }
        let mut pointers = [std::ptr::null_mut(); MAX_METER_CHANNELS];
        for (index, port) in ports.iter().enumerate() {
            pointers[index] = acquire(port.raw);
        }
        Some(Self {
            pointers,
            len: ports.len(),
        })
    }

    pub(super) fn get(&self, index: usize) -> Option<NonNull<f32>> {
        debug_assert!(index < self.len);
        NonNull::new(self.pointers[index])
    }
}

#[derive(Default)]
pub(super) struct PublishedMetrics {
    version: AtomicU64,
    source_loudness_bits: AtomicU32,
    source_true_peak_bits: AtomicU32,
    output_loudness_bits: AtomicU32,
    output_true_peak_bits: AtomicU32,
    limiter_reduction_bits: AtomicU32,
    maximum_limiter_reduction_bits: AtomicU32,
}

impl PublishedMetrics {
    pub(super) fn publish(&self, metrics: ProcessMetrics) {
        let version = self.version.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(version % 2, 0, "the process callback is a single writer");
        self.source_loudness_bits
            .store(metrics.source.loudness_lufs.to_bits(), Ordering::Relaxed);
        self.source_true_peak_bits.store(
            option_f32_bits(metrics.source.true_peak_dbtp),
            Ordering::Relaxed,
        );
        self.output_loudness_bits.store(
            option_f32_bits(metrics.output.map(|reading| reading.loudness_lufs)),
            Ordering::Relaxed,
        );
        self.output_true_peak_bits.store(
            option_f32_bits(metrics.output.and_then(|reading| reading.true_peak_dbtp)),
            Ordering::Relaxed,
        );
        self.limiter_reduction_bits
            .store(metrics.limiter_reduction_db.to_bits(), Ordering::Relaxed);
        self.maximum_limiter_reduction_bits.store(
            metrics.maximum_limiter_reduction_db.to_bits(),
            Ordering::Relaxed,
        );
        self.version.store(version + 2, Ordering::Release);
    }

    pub(super) fn read(&self) -> Option<MeterSnapshot> {
        loop {
            let before = self.version.load(Ordering::Acquire);
            if before == 0 {
                return None;
            }
            if !before.is_multiple_of(2) {
                std::hint::spin_loop();
                continue;
            }
            let source_loudness_lufs =
                f32::from_bits(self.source_loudness_bits.load(Ordering::Relaxed));
            let source_true_peak_dbtp =
                option_f32_from_bits(self.source_true_peak_bits.load(Ordering::Relaxed));
            let output_loudness_lufs =
                option_f32_from_bits(self.output_loudness_bits.load(Ordering::Relaxed));
            let output_true_peak_dbtp =
                option_f32_from_bits(self.output_true_peak_bits.load(Ordering::Relaxed));
            let limiter_reduction_db =
                f32::from_bits(self.limiter_reduction_bits.load(Ordering::Relaxed));
            let maximum_limiter_reduction_db =
                f32::from_bits(self.maximum_limiter_reduction_bits.load(Ordering::Relaxed));
            let after = self.version.load(Ordering::Acquire);
            if before == after {
                return Some(MeterSnapshot {
                    sequence: before / 2,
                    source_loudness_lufs,
                    source_true_peak_dbtp,
                    output_loudness_lufs,
                    output_true_peak_dbtp,
                    limiter_reduction_db,
                    maximum_limiter_reduction_db,
                });
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ProcessMetrics {
    pub(super) source: MeterReading,
    pub(super) output: Option<MeterReading>,
    pub(super) limiter_reduction_db: f32,
    pub(super) maximum_limiter_reduction_db: f32,
}

fn option_f32_bits(value: Option<f32>) -> u32 {
    value.unwrap_or(f32::NAN).to_bits()
}

fn option_f32_from_bits(bits: u32) -> Option<f32> {
    let value = f32::from_bits(bits);
    value.is_finite().then_some(value)
}

pub(super) unsafe extern "C" fn callback(
    data: *mut c_void,
    position: *mut pipewire::spa::sys::spa_io_position,
) {
    // SAFETY: PipeWire invokes this callback with the data pointer supplied to
    // the filter and a position valid for this process cycle.
    let (data, position) = unsafe {
        (
            data.cast::<FilterCallbackData>().as_mut(),
            position.as_ref(),
        )
    };
    let (Some(data), Some(position)) = (data, position) else {
        return;
    };
    let Ok(sample_count) = u32::try_from(position.clock.duration) else {
        return;
    };
    let Some(buffers) = CycleBuffers::acquire(&data.ports, sample_count) else {
        return;
    };

    let sample_rate = process_sample_rate(position, data.default_sample_rate);
    let processing_state = processing_state_index(&data.processing_states, sample_rate);
    if data.active_processing_state != processing_state {
        if let Some(index) = processing_state {
            data.processing_states[index].reset();
        }
        data.active_processing_state = processing_state;
    }
    let source_reading = processing_state.and_then(|index| {
        meter_ports(
            &mut data.processing_states[index].source,
            &data.ports,
            &buffers,
            sample_count,
            PortDirection::Input,
        )
    });

    let target_gain_db = processing_state.map_or(0.0, |_| {
        f32::from_bits(data.target_gain_bits.load(Ordering::Relaxed))
    });
    for output_index in 0..data.ports.len() {
        if data.ports[output_index].direction != PortDirection::Output {
            continue;
        }
        let input_index = data.ports.iter().enumerate().find_map(|(index, port)| {
            (port.direction == PortDirection::Input
                && port.channel == data.ports[output_index].channel)
                .then_some(index)
        });
        let Some(input_index) = input_index else {
            continue;
        };
        let output = &mut data.ports[output_index];
        let (Some(input), Some(output_buffer)) =
            (buffers.get(input_index), buffers.get(output_index))
        else {
            continue;
        };
        // SAFETY: Both pointers came from buffers acquired for this process
        // cycle and are valid for sample_count f32 samples.
        unsafe {
            process_mono_buffers(
                input.as_ptr(),
                output_buffer.as_ptr(),
                sample_count,
                &mut output.gain,
                target_gain_db,
            );
        }
    }
    let limiter_reduction_db = processing_state.map_or(0.0, |index| {
        limit_output_ports(
            &mut data.processing_states[index].limiter,
            &data.ports,
            &buffers,
            sample_count,
        )
    });
    data.maximum_limiter_reduction_db = data.maximum_limiter_reduction_db.max(limiter_reduction_db);
    let output_reading = processing_state.and_then(|index| {
        meter_ports(
            &mut data.processing_states[index].output,
            &data.ports,
            &buffers,
            sample_count,
            PortDirection::Output,
        )
    });
    if let Some(source) = source_reading {
        data.latest_metrics.publish(ProcessMetrics {
            source,
            output: output_reading,
            limiter_reduction_db,
            maximum_limiter_reduction_db: data.maximum_limiter_reduction_db,
        });
    }
}

fn limit_output_ports(
    limiter: &mut TruePeakLimiter,
    ports: &[CallbackPort],
    buffers: &CycleBuffers,
    sample_count: u32,
) -> f32 {
    let mut outputs = [std::ptr::null_mut(); MAX_METER_CHANNELS];
    let mut output_count = 0;
    for (index, port) in ports.iter().enumerate() {
        if port.direction != PortDirection::Output {
            continue;
        }
        if output_count == MAX_METER_CHANNELS {
            return 0.0;
        }
        let Some(output) = buffers.get(index) else {
            continue;
        };
        outputs[output_count] = output.as_ptr();
        output_count += 1;
    }
    if output_count != limiter.channel_count() {
        return 0.0;
    }
    let mut minimum_limiter_gain = 1.0_f32;
    let mut input_frame = [0.0; MAX_METER_CHANNELS];
    let mut output_frame = [0.0; MAX_METER_CHANNELS];
    for sample_index in 0..sample_count as usize {
        for (channel, output) in outputs[..output_count].iter().enumerate() {
            // Every pointer was validated above for a buffer of sample_count samples.
            input_frame[channel] = unsafe { *output.add(sample_index) };
        }
        let limiter_gain = limiter
            .process_frame(
                &input_frame[..output_count],
                &mut output_frame[..output_count],
            )
            .expect("the output layout was validated before processing");
        minimum_limiter_gain = minimum_limiter_gain.min(limiter_gain);
        for (channel, output) in outputs[..output_count].iter().enumerate() {
            // Every pointer was validated above for a buffer of sample_count samples.
            unsafe { *output.add(sample_index) = output_frame[channel] };
        }
    }
    if minimum_limiter_gain > 0.0 {
        -20.0 * minimum_limiter_gain.log10()
    } else {
        f32::INFINITY
    }
}

pub(super) fn process_sample_rate(
    position: &pipewire::spa::sys::spa_io_position,
    fallback: Option<u32>,
) -> Option<u32> {
    let rate = position.clock.rate;
    if rate.num == 0 || rate.denom == 0 {
        return fallback;
    }
    rate.denom.checked_div(rate.num).filter(|rate| *rate > 0)
}

fn processing_state_index(states: &[ProcessingState], sample_rate: Option<u32>) -> Option<usize> {
    let sample_rate = sample_rate?;
    states
        .iter()
        .position(|state| state.sample_rate == sample_rate)
}

fn meter_ports(
    meter: &mut LoudnessMeter,
    ports: &[CallbackPort],
    buffers: &CycleBuffers,
    sample_count: u32,
    direction: PortDirection,
) -> Option<MeterReading> {
    let mut channels = [&[][..]; MAX_METER_CHANNELS];
    let mut channel_count = 0;
    for (index, port) in ports.iter().enumerate() {
        if port.direction != direction {
            continue;
        }
        if channel_count == MAX_METER_CHANNELS {
            return None;
        }
        // SAFETY: The pointer and length follow from the PipeWire DSP buffer
        // contract above, and the slice does not escape this callback.
        let input = buffers.get(index)?;
        channels[channel_count] =
            unsafe { std::slice::from_raw_parts(input.as_ptr(), sample_count as usize) };
        channel_count += 1;
    }
    if channel_count == 0 {
        return None;
    }

    meter.push_planar(&channels[..channel_count]).ok().flatten()
}

pub(super) unsafe fn process_mono_buffers(
    input: *mut f32,
    output: *mut f32,
    sample_count: u32,
    gain: &mut GainStage,
    target_gain_db: f32,
) {
    // SAFETY: The caller guarantees both pointers are valid for sample_count
    // f32 samples. ptr::copy permits PipeWire to provide identical or
    // overlapping input and output buffers without creating aliased slices.
    unsafe { std::ptr::copy(input, output, sample_count as usize) };
    let output = unsafe { std::slice::from_raw_parts_mut(output, sample_count as usize) };
    let result = gain.process_interleaved(output, 1, target_gain_db);
    debug_assert!(result.is_ok(), "validated filter gain must process");
}

#[cfg(test)]
pub(super) fn process_samples(
    input: &[f32],
    output: &mut [f32],
    gain: &mut GainStage,
    target_gain_db: f32,
) {
    output.copy_from_slice(input);
    let result = gain.process_interleaved(output, 1, target_gain_db);
    debug_assert!(result.is_ok(), "validated filter gain must process");
}

pub(super) fn meter_channel(channel: &str) -> Channel {
    match channel {
        "FL" => Channel::Left,
        "FR" => Channel::Right,
        "FC" => Channel::Center,
        "LFE" => Channel::Lfe,
        "SL" | "RL" => Channel::LeftSurround,
        "SR" | "RR" => Channel::RightSurround,
        _ => Channel::Other,
    }
}

pub(super) fn target_gain_bits(target_gain_db: f32) -> Result<u32, &'static str> {
    target_gain_db
        .is_finite()
        .then(|| target_gain_db.to_bits())
        .ok_or("target gain must be finite")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn callback_data_with_stereo_ports() -> FilterCallbackData {
        let mut data = FilterCallbackData::default();
        let pointer = NonNull::<u8>::dangling().cast();
        for channel in ["FL", "FR"] {
            data.add_port(pointer, PortDirection::Input, channel.to_owned());
            data.add_port(pointer, PortDirection::Output, channel.to_owned());
        }
        data
    }

    #[test]
    fn meter_setup_prepares_standard_pipewire_rates_before_processing() {
        let mut data = callback_data_with_stereo_ports();
        data.enable_meter(48_000).unwrap();

        let rates: Vec<_> = data
            .processing_states
            .iter()
            .map(|state| state.sample_rate)
            .collect();
        assert_eq!(
            rates,
            [48_000, 22_050, 32_000, 44_100, 88_200, 96_000, 192_000]
        );
        assert_eq!(data.default_sample_rate, Some(48_000));
        assert_eq!(data.active_processing_state, None);
    }

    #[test]
    fn unsupported_graph_rates_select_no_processing_state() {
        let mut data = callback_data_with_stereo_ports();
        data.enable_meter(48_000).unwrap();

        assert_eq!(
            processing_state_index(&data.processing_states, Some(44_100))
                .map(|index| data.processing_states[index].sample_rate),
            Some(44_100)
        );
        assert_eq!(
            processing_state_index(&data.processing_states, Some(176_400)),
            None
        );
        assert_eq!(processing_state_index(&data.processing_states, None), None);
    }
}
