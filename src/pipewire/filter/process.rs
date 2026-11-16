// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    os::raw::c_void,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use ::pipewire::sys;
use ebur128_stream::Channel;

use super::{MeterSnapshot, PortDirection};
use crate::normalization::ControllerConfig;
use crate::normalization::dsp::{
    fast_cut::{FastCut, FastCutConfig},
    gain::{GainStage, TruePeakLimiter},
    meter::{LoudnessMeter, MeterReading},
};

const MAX_METER_CHANNELS: usize = 64;
const PREPARED_SAMPLE_RATES: [u32; 7] = [22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 192_000];

pub(super) struct FilterCallbackData {
    destroyed: AtomicBool,
    ports: Vec<CallbackPort>,
    target_gain_bits: AtomicU32,
    user_gain_bits: AtomicU32,
    handoff: AtomicU32,
    handoff_ack: AtomicU32,
    held_audio_ns: AtomicU64,
    user_gain_current: f32,
    user_gain_target: f32,
    user_gain_step: f32,
    user_gain_remaining: usize,
    default_sample_rate: Option<u32>,
    processing_states: Vec<ProcessingState>,
    active_processing_state: Option<usize>,
    latest_metrics: PublishedMetrics,
    maximum_limiter_reduction_db: f32,
    dynamics: Option<FastCut>,
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
            destroyed: AtomicBool::new(false),
            ports: Vec::new(),
            target_gain_bits: AtomicU32::new(0.0_f32.to_bits()),
            user_gain_bits: AtomicU32::new(1.0_f32.to_bits()),
            handoff: AtomicU32::new(0),
            handoff_ack: AtomicU32::new(0),
            held_audio_ns: AtomicU64::new(0),
            user_gain_current: 1.0,
            user_gain_target: 1.0,
            user_gain_step: 0.0,
            user_gain_remaining: 0,
            default_sample_rate: None,
            processing_states: Vec::new(),
            active_processing_state: None,
            latest_metrics: PublishedMetrics::default(),
            maximum_limiter_reduction_db: 0.0,
            dynamics: None,
        }
    }
}

impl FilterCallbackData {
    pub(super) fn handoff(&self, mode: u32) {
        self.handoff.store(mode, Ordering::Release);
    }

    pub(super) fn handoff_ack(&self) -> u32 {
        self.handoff_ack.load(Ordering::Acquire)
    }

    pub(super) fn held_audio_ns(&self) -> u64 {
        self.held_audio_ns.load(Ordering::Acquire)
    }

    pub(super) fn set_user_gain(&self, gain: f32) {
        debug_assert!(gain.is_finite() && (0.0..=8.0).contains(&gain));
        self.user_gain_bits.store(gain.to_bits(), Ordering::Relaxed);
    }

    pub(super) fn configure_dynamics(
        &mut self,
        config: ControllerConfig,
    ) -> Result<(), &'static str> {
        let config = config.validate()?;
        self.dynamics = Some(FastCut::new(FastCutConfig {
            target_lufs: config.target_lufs,
            silence_gate_lufs: config.silence_gate_lufs,
            deadband_lu: config.deadband_lu,
            maximum_boost_db: config.maximum_boost_db,
            maximum_cut_db: config.maximum_cut_db,
            boost_rate_db_per_second: config.boost_rate_db_per_second,
            hold_seconds: config.fast_cut_hold_seconds,
        })?);
        Ok(())
    }

    pub(super) fn restore_dynamics(
        &mut self,
        gain_db: f32,
        remaining_hold_seconds: f32,
        sequence: u64,
    ) -> Result<(), &'static str> {
        let dynamics = self
            .dynamics
            .as_mut()
            .ok_or("dynamics must be configured before restoring gain")?;
        dynamics.restore(gain_db, remaining_hold_seconds, sequence)?;
        let restored_gain_db = dynamics.gain_db();
        self.set_target_gain_db(restored_gain_db)
    }

    pub(super) fn mark_destroyed(&self) {
        self.destroyed.store(true, Ordering::Release);
    }

    pub(super) fn is_destroyed(&self) -> bool {
        self.destroyed.load(Ordering::Acquire)
    }

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

pub(super) unsafe extern "C" fn destroy_callback(data: *mut c_void) {
    // SAFETY: PipeWire invokes this callback with the data pointer supplied to
    // the filter while the boxed callback data is still alive.
    if let Some(data) = unsafe { data.cast::<FilterCallbackData>().as_ref() } {
        data.mark_destroyed();
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
    source_momentary_bits: AtomicU32,
    source_true_peak_bits: AtomicU32,
    output_loudness_bits: AtomicU32,
    output_true_peak_bits: AtomicU32,
    limiter_reduction_bits: AtomicU32,
    maximum_limiter_reduction_bits: AtomicU32,
    applied_gain_bits: AtomicU32,
    fast_cut_hold_bits: AtomicU32,
    fast_cut_sequence: AtomicU64,
}

impl PublishedMetrics {
    pub(super) fn publish(&self, metrics: ProcessMetrics) {
        let version = self.version.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(version % 2, 0, "the process callback is a single writer");
        self.source_loudness_bits
            .store(metrics.source.loudness_lufs.to_bits(), Ordering::Relaxed);
        self.source_momentary_bits.store(
            option_f32_bits(metrics.source.momentary_lufs),
            Ordering::Relaxed,
        );
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
        self.applied_gain_bits
            .store(metrics.applied_gain_db.to_bits(), Ordering::Relaxed);
        self.fast_cut_hold_bits
            .store(metrics.fast_cut_hold_seconds.to_bits(), Ordering::Relaxed);
        self.fast_cut_sequence
            .store(metrics.fast_cut_sequence, Ordering::Relaxed);
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
            let source_momentary_lufs =
                option_f32_from_bits(self.source_momentary_bits.load(Ordering::Relaxed));
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
            let applied_gain_db = f32::from_bits(self.applied_gain_bits.load(Ordering::Relaxed));
            let fast_cut_hold_seconds =
                f32::from_bits(self.fast_cut_hold_bits.load(Ordering::Relaxed));
            let fast_cut_sequence = self.fast_cut_sequence.load(Ordering::Relaxed);
            let after = self.version.load(Ordering::Acquire);
            if before == after {
                return Some(MeterSnapshot {
                    sequence: before / 2,
                    source_loudness_lufs,
                    source_momentary_lufs,
                    source_true_peak_dbtp,
                    output_loudness_lufs,
                    output_true_peak_dbtp,
                    limiter_reduction_db,
                    maximum_limiter_reduction_db,
                    applied_gain_db,
                    fast_cut_hold_seconds,
                    fast_cut_sequence,
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
    pub(super) applied_gain_db: f32,
    pub(super) fast_cut_hold_seconds: f32,
    pub(super) fast_cut_sequence: u64,
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
    position: *mut ::pipewire::spa::sys::spa_io_position,
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
    // SAFETY: Acquired buffers remain valid for this native process cycle.
    unsafe { process_cycle(data, &buffers, sample_rate, sample_count) };
}

unsafe fn process_cycle(
    data: &mut FilterCallbackData,
    buffers: &CycleBuffers,
    sample_rate: Option<u32>,
    sample_count: u32,
) {
    let handoff = data.handoff.load(Ordering::Acquire);
    if handoff != 0 {
        if handoff == 2 && data.handoff_ack.load(Ordering::Relaxed) != 2 {
            for state in &mut data.processing_states {
                state.reset();
            }
            if let Some(dynamics) = &mut data.dynamics {
                dynamics.reset();
            }
            for port in &mut data.ports {
                port.gain = GainStage::default();
            }
            data.maximum_limiter_reduction_db = 0.0;
            data.target_gain_bits
                .store(0.0_f32.to_bits(), Ordering::Relaxed);
            data.latest_metrics.version.store(0, Ordering::Release);
            data.user_gain_current = 0.0;
            data.user_gain_target = 0.0;
            data.user_gain_remaining = 0;
        }
        for (index, port) in data.ports.iter().enumerate() {
            if port.direction == PortDirection::Output
                && let Some(buffer) = buffers.get(index)
            {
                // SAFETY: acquired outputs contain sample_count writable samples.
                unsafe { std::slice::from_raw_parts_mut(buffer.as_ptr(), sample_count as usize) }
                    .fill(0.0);
            }
        }
        data.handoff_ack.store(handoff, Ordering::Release);
        if let Some(rate) = sample_rate.filter(|rate| *rate > 0) {
            data.held_audio_ns.fetch_add(
                u64::from(sample_count) * 1_000_000_000 / u64::from(rate),
                Ordering::Release,
            );
        }
        return;
    }
    data.handoff_ack.store(0, Ordering::Release);
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
            buffers,
            sample_count,
            PortDirection::Input,
        )
    });

    let requested_gain_db = processing_state.map_or(0.0, |_| {
        f32::from_bits(data.target_gain_bits.load(Ordering::Relaxed))
    });
    let target_gain_db = if let (Some(_), Some(rate), Some(dynamics)) =
        (processing_state, sample_rate, data.dynamics.as_mut())
    {
        dynamics.process(
            requested_gain_db,
            source_reading.and_then(|reading| reading.momentary_lufs),
            sample_count as f32 / rate as f32,
        )
    } else {
        requested_gain_db
    };
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
    // This gain never enters the source meter or normalization/fastcut state.
    // Keep the final clipping limiter downstream even when the UI boosts >1.
    let user_gain = f32::from_bits(data.user_gain_bits.load(Ordering::Relaxed));
    let ramp_samples = (sample_rate.unwrap_or(48_000) / 200).max(1) as usize;
    if data.user_gain_target != user_gain {
        data.user_gain_target = user_gain;
        data.user_gain_remaining = ramp_samples;
        data.user_gain_step = (user_gain - data.user_gain_current) / ramp_samples as f32;
    }
    let previous_user_gain = data.user_gain_current;
    let remaining = data.user_gain_remaining;
    let step = data.user_gain_step;
    for (index, port) in data.ports.iter().enumerate() {
        if port.direction == PortDirection::Output
            && let Some(buffer) = buffers.get(index)
        {
            // SAFETY: the acquired output contains sample_count writable samples.
            for (index, sample) in
                unsafe { std::slice::from_raw_parts_mut(buffer.as_ptr(), sample_count as usize) }
                    .iter_mut()
                    .enumerate()
            {
                let gain = if index + 1 >= remaining {
                    user_gain
                } else {
                    previous_user_gain + step * (index + 1) as f32
                };
                *sample *= gain;
            }
        }
    }
    data.user_gain_remaining = remaining.saturating_sub(sample_count as usize);
    data.user_gain_current = if data.user_gain_remaining == 0 {
        user_gain
    } else {
        previous_user_gain + step * sample_count as f32
    };
    let limiter_reduction_db = processing_state.map_or(0.0, |index| {
        limit_output_ports(
            &mut data.processing_states[index].limiter,
            &data.ports,
            buffers,
            sample_count,
        )
    });
    data.maximum_limiter_reduction_db = data.maximum_limiter_reduction_db.max(limiter_reduction_db);
    let output_reading = processing_state.and_then(|index| {
        meter_ports(
            &mut data.processing_states[index].output,
            &data.ports,
            buffers,
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
            applied_gain_db: target_gain_db,
            fast_cut_hold_seconds: data
                .dynamics
                .as_ref()
                .map_or(0.0, FastCut::remaining_hold_seconds),
            fast_cut_sequence: data
                .dynamics
                .as_ref()
                .map_or(0, FastCut::reduction_sequence),
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
    position: &::pipewire::spa::sys::spa_io_position,
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
    fn records_native_filter_destruction() {
        let data = FilterCallbackData::default();
        assert!(!data.is_destroyed());

        data.mark_destroyed();

        assert!(data.is_destroyed());
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

    #[test]
    fn native_processing_path_cuts_loud_audio_and_never_snaps_back_to_a_stale_request() {
        use std::f32::consts::TAU;

        const FRAMES: usize = 480;
        let mut data = callback_data_with_stereo_ports();
        data.enable_meter(48_000).unwrap();
        data.configure_dynamics(ControllerConfig::default())
            .unwrap();
        // Deliberately never update the main controller's stale boost request.
        data.set_target_gain_db(6.0).unwrap();
        let mut left = [0.0; FRAMES];
        let mut right = [0.0; FRAMES];
        let mut left_out = [0.0; FRAMES];
        let mut right_out = [0.0; FRAMES];
        let buffers = CycleBuffers {
            pointers: {
                let mut pointers = [std::ptr::null_mut(); MAX_METER_CHANNELS];
                pointers[..4].copy_from_slice(&[
                    left.as_mut_ptr(),
                    left_out.as_mut_ptr(),
                    right.as_mut_ptr(),
                    right_out.as_mut_ptr(),
                ]);
                pointers
            },
            len: 4,
        };
        let mut previous_gain = 0.0;
        let mut first_cut_gain = None;
        for cycle in 0..300 {
            let amplitude = if cycle < 100 { 0.5 } else { 0.005 };
            for frame in 0..FRAMES {
                let sample =
                    amplitude * (TAU * 1_000.0 * (cycle * FRAMES + frame) as f32 / 48_000.0).sin();
                left[frame] = sample;
                right[frame] = sample;
            }
            // SAFETY: All four disjoint arrays remain allocated for this call
            // and contain exactly FRAMES writable samples.
            unsafe { process_cycle(&mut data, &buffers, Some(48_000), FRAMES as u32) };
            assert!(
                left_out
                    .iter()
                    .chain(&right_out)
                    .all(|sample| sample.is_finite())
            );
            assert_eq!(left_out, right_out);
            if let Some(snapshot) = data.latest_meter_snapshot() {
                assert!(snapshot.applied_gain_db <= previous_gain + 0.0021);
                previous_gain = snapshot.applied_gain_db;
                if snapshot.fast_cut_sequence > 0 {
                    let cut = *first_cut_gain.get_or_insert(snapshot.applied_gain_db);
                    assert!(snapshot.applied_gain_db <= cut + 0.2 * 3.0 + 0.001);
                    assert!(snapshot.source_momentary_lufs.is_some());
                }
            }
        }
        assert!(first_cut_gain.is_some_and(|gain| gain < -6.0));
        let final_snapshot = data.latest_meter_snapshot().unwrap();
        assert!(final_snapshot.applied_gain_db < -6.0);
        assert_eq!(final_snapshot.fast_cut_hold_seconds, 0.0);
        assert!(data.dynamics.as_ref().unwrap().reduction_sequence() > 0);
    }

    #[test]
    fn downstream_user_gain_does_not_change_source_meter_or_fastcut() {
        const FRAMES: usize = 480;
        let mut reference = None;
        for gain in [0.0, 0.125, 1.0, 8.0] {
            let mut data = callback_data_with_stereo_ports();
            data.enable_meter(48_000).unwrap();
            data.configure_dynamics(ControllerConfig::default())
                .unwrap();
            data.set_user_gain(gain);
            let mut input = [0.0_f32; FRAMES];
            let mut input_right = [0.0_f32; FRAMES];
            let mut output = [0.0_f32; FRAMES];
            let mut output_right = [0.0_f32; FRAMES];
            let buffers = CycleBuffers {
                pointers: {
                    let mut pointers = [std::ptr::null_mut(); MAX_METER_CHANNELS];
                    pointers[..4].copy_from_slice(&[
                        input.as_mut_ptr(),
                        output.as_mut_ptr(),
                        input_right.as_mut_ptr(),
                        output_right.as_mut_ptr(),
                    ]);
                    pointers
                },
                len: 4,
            };
            for cycle in 0..100 {
                for frame in 0..FRAMES {
                    let amplitude = if cycle < 50 { 0.5 } else { 0.01 };
                    input[frame] = amplitude
                        * (std::f32::consts::TAU * 997.0 * (cycle * FRAMES + frame) as f32
                            / 48_000.0)
                            .sin();
                    input_right[frame] = input[frame];
                }
                // SAFETY: four disjoint arrays provide FRAMES writable samples.
                unsafe { process_cycle(&mut data, &buffers, Some(48_000), FRAMES as u32) };
                assert_eq!(output, output_right);
                assert!(output.iter().all(|sample| sample.abs() <= 0.9));
            }
            let meter = data.latest_meter_snapshot().unwrap();
            let source_and_dynamics = (
                meter.source_loudness_lufs,
                meter.source_momentary_lufs,
                meter.applied_gain_db,
                meter.fast_cut_sequence,
                meter.fast_cut_hold_seconds,
            );
            if let Some(expected) = reference {
                assert_eq!(source_and_dynamics, expected);
            } else {
                reference = Some(source_and_dynamics);
            }
            if gain == 0.0 {
                assert!(output.iter().all(|sample| *sample == 0.0));
            }
            data.handoff(1);
            // SAFETY: the same arrays remain valid for this process cycle.
            unsafe { process_cycle(&mut data, &buffers, Some(48_000), FRAMES as u32) };
            assert_eq!(data.handoff_ack(), 1);
            assert!(output.iter().all(|sample| *sample == 0.0));
            data.handoff(2);
            // SAFETY: the same arrays remain valid for this process cycle.
            unsafe { process_cycle(&mut data, &buffers, Some(48_000), FRAMES as u32) };
            assert_eq!(data.latest_meter_snapshot(), None);
            assert_eq!(data.dynamics.as_ref().unwrap().gain_db(), 0.0);
            assert_eq!(data.dynamics.as_ref().unwrap().reduction_sequence(), 0);
        }
    }

    #[test]
    fn replacement_filter_starts_with_its_inherited_attenuation_before_any_meter_reading() {
        let mut data = callback_data_with_stereo_ports();
        data.enable_meter(48_000).unwrap();
        data.configure_dynamics(ControllerConfig::default())
            .unwrap();
        data.restore_dynamics(-12.0, 0.5, 42).unwrap();
        assert_eq!(data.target_gain_db(), -12.0);
        assert_eq!(data.latest_meter_snapshot(), None);
        let requested_gain_db = data.target_gain_db();
        let dynamics = data.dynamics.as_mut().unwrap();
        assert_eq!(dynamics.process(requested_gain_db, None, 0.01), -12.0);
        assert_eq!(dynamics.reduction_sequence(), 42);
    }
}
