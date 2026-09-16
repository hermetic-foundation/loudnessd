// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    ffi::{CStr, CString},
    fmt,
    marker::PhantomData,
    os::raw::c_void,
    ptr::NonNull,
    rc::Rc,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use ebur128_stream::Channel;
use pipewire::{loop_::Loop, properties::properties, sys};

use crate::{
    gain::{GainStage, PeakLimiter},
    meter::{LoudnessMeter, MeterReading},
};

const MAX_METER_CHANNELS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterState {
    Error,
    Unconnected,
    Connecting,
    Paused,
    Streaming,
    Unknown(i32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterCreateError {
    NameContainsNul,
    CreationFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortDirection {
    Input,
    Output,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortDescriptor {
    pub direction: PortDirection,
    pub name: String,
    pub channel: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeterSnapshot {
    pub sequence: u64,
    pub source_loudness_lufs: f32,
    pub source_true_peak_dbtp: Option<f32>,
    pub output_loudness_lufs: Option<f32>,
    pub output_true_peak_dbtp: Option<f32>,
    pub limiter_reduction_db: f32,
    pub maximum_limiter_reduction_db: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortCreateError {
    FilterNotUnconnected,
    CreationFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilterConnectError(i32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilterActivationError(i32);

impl fmt::Display for FilterCreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameContainsNul => formatter.write_str("filter name contains a NUL byte"),
            Self::CreationFailed => formatter.write_str("PipeWire failed to create the filter"),
        }
    }
}

impl std::error::Error for FilterCreateError {}

impl fmt::Display for PortCreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FilterNotUnconnected => {
                formatter.write_str("ports can only be added before the filter is connected")
            }
            Self::CreationFailed => formatter.write_str("PipeWire failed to create the port"),
        }
    }
}

impl std::error::Error for PortCreateError {}

impl FilterConnectError {
    pub fn raw_code(self) -> i32 {
        self.0
    }
}

impl fmt::Display for FilterConnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "PipeWire failed to connect the filter (error {})",
            self.0
        )
    }
}

impl std::error::Error for FilterConnectError {}

impl FilterActivationError {
    pub fn raw_code(self) -> i32 {
        self.0
    }
}

impl fmt::Display for FilterActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "PipeWire failed to change filter activation (error {})",
            self.0
        )
    }
}

impl std::error::Error for FilterActivationError {}

static FILTER_EVENTS: sys::pw_filter_events = sys::pw_filter_events {
    version: sys::PW_VERSION_FILTER_EVENTS,
    destroy: None,
    state_changed: None,
    io_changed: None,
    param_changed: None,
    add_buffer: None,
    remove_buffer: None,
    process: Some(process),
    drained: None,
    command: None,
};

struct FilterCallbackData {
    ports: Vec<CallbackPort>,
    target_gain_bits: AtomicU32,
    meter: Option<MeterState>,
    latest_metrics: PublishedMetrics,
    limiter: PeakLimiter,
    maximum_limiter_reduction_db: f32,
}

struct MeterState {
    sample_rate: u32,
    channels: Vec<Channel>,
    source: LoudnessMeter,
    output: LoudnessMeter,
}

impl Default for FilterCallbackData {
    fn default() -> Self {
        Self {
            ports: Vec::new(),
            target_gain_bits: AtomicU32::new(0.0_f32.to_bits()),
            meter: None,
            latest_metrics: PublishedMetrics::default(),
            limiter: PeakLimiter::default(),
            maximum_limiter_reduction_db: 0.0,
        }
    }
}

struct CallbackPort {
    raw: NonNull<c_void>,
    direction: PortDirection,
    channel: String,
    gain: GainStage,
}

struct CycleBuffers {
    pointers: [*mut f32; MAX_METER_CHANNELS],
    len: usize,
}

impl CycleBuffers {
    fn acquire(ports: &[CallbackPort], sample_count: u32) -> Option<Self> {
        Self::acquire_with(ports, |port| {
            // SAFETY: PipeWire owns the port and makes its DSP buffer valid for
            // sample_count f32 samples for the duration of this process cycle.
            unsafe { sys::pw_filter_get_dsp_buffer(port.as_ptr(), sample_count) }.cast::<f32>()
        })
    }

    fn acquire_with(
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

    fn get(&self, index: usize) -> Option<NonNull<f32>> {
        debug_assert!(index < self.len);
        NonNull::new(self.pointers[index])
    }
}

#[derive(Default)]
struct PublishedMetrics {
    version: AtomicU64,
    source_loudness_bits: AtomicU32,
    source_true_peak_bits: AtomicU32,
    output_loudness_bits: AtomicU32,
    output_true_peak_bits: AtomicU32,
    limiter_reduction_bits: AtomicU32,
    maximum_limiter_reduction_bits: AtomicU32,
}

impl PublishedMetrics {
    fn publish(&self, metrics: ProcessMetrics) {
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

    fn read(&self) -> Option<MeterSnapshot> {
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
struct ProcessMetrics {
    source: MeterReading,
    output: Option<MeterReading>,
    limiter_reduction_db: f32,
    maximum_limiter_reduction_db: f32,
}

fn option_f32_bits(value: Option<f32>) -> u32 {
    value.unwrap_or(f32::NAN).to_bits()
}

fn option_f32_from_bits(bits: u32) -> Option<f32> {
    let value = f32::from_bits(bits);
    value.is_finite().then_some(value)
}

unsafe extern "C" fn process(
    data: *mut c_void,
    position: *mut pipewire::spa::sys::spa_io_position,
) {
    // SAFETY: PipeWire invokes this callback with the data pointer supplied to
    // pw_filter_new_simple and a position valid for this process cycle.
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

    let sample_rate =
        process_sample_rate(position, data.meter.as_ref().map(|meter| meter.sample_rate));
    prepare_meters(data, sample_rate);
    let source_reading = meter_ports(
        data,
        &buffers,
        sample_count,
        PortDirection::Input,
        MeterKind::Source,
    );

    let target_gain_db = f32::from_bits(data.target_gain_bits.load(Ordering::Relaxed));
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
        process_mono_buffers(
            input.as_ptr(),
            output_buffer.as_ptr(),
            sample_count,
            &mut output.gain,
            target_gain_db,
        );
    }
    let limiter_reduction_db = limit_output_ports(data, &buffers, sample_count, sample_rate);
    data.maximum_limiter_reduction_db = data.maximum_limiter_reduction_db.max(limiter_reduction_db);
    let output_reading = meter_ports(
        data,
        &buffers,
        sample_count,
        PortDirection::Output,
        MeterKind::Output,
    );
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
    data: &mut FilterCallbackData,
    buffers: &CycleBuffers,
    sample_count: u32,
    sample_rate: Option<u32>,
) -> f32 {
    let Some(sample_rate) = sample_rate else {
        return 0.0;
    };
    let mut outputs = [std::ptr::null_mut(); MAX_METER_CHANNELS];
    let mut output_count = 0;
    for (index, port) in data.ports.iter().enumerate() {
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
    let mut minimum_limiter_gain = 1.0_f32;
    for sample_index in 0..sample_count as usize {
        let peak = outputs[..output_count]
            .iter()
            // Every pointer was validated above for a buffer of sample_count samples.
            .map(|output| unsafe { *output.add(sample_index) }.abs())
            .fold(0.0_f32, f32::max);
        let limiter_gain = data.limiter.gain_for_peak(peak, sample_rate);
        minimum_limiter_gain = minimum_limiter_gain.min(limiter_gain);
        for output in &outputs[..output_count] {
            // Every pointer was validated above for a buffer of sample_count samples.
            unsafe { *output.add(sample_index) *= limiter_gain };
        }
    }
    if minimum_limiter_gain > 0.0 {
        -20.0 * minimum_limiter_gain.log10()
    } else {
        f32::INFINITY
    }
}

fn process_sample_rate(
    position: &pipewire::spa::sys::spa_io_position,
    fallback: Option<u32>,
) -> Option<u32> {
    let rate = position.clock.rate;
    if rate.num == 0 || rate.denom == 0 {
        return fallback;
    }
    rate.denom.checked_div(rate.num).filter(|rate| *rate > 0)
}

#[derive(Clone, Copy)]
enum MeterKind {
    Source,
    Output,
}

fn prepare_meters(data: &mut FilterCallbackData, sample_rate: Option<u32>) {
    let Some(sample_rate) = sample_rate else {
        return;
    };
    let Some(meter) = data.meter.as_mut() else {
        return;
    };
    if meter.sample_rate == sample_rate {
        return;
    }
    let (Ok(source), Ok(output)) = (
        LoudnessMeter::new(sample_rate, &meter.channels),
        LoudnessMeter::new(sample_rate, &meter.channels),
    ) else {
        return;
    };
    meter.sample_rate = sample_rate;
    meter.source = source;
    meter.output = output;
}

fn meter_ports(
    data: &mut FilterCallbackData,
    buffers: &CycleBuffers,
    sample_count: u32,
    direction: PortDirection,
    kind: MeterKind,
) -> Option<MeterReading> {
    let meter = data.meter.as_mut()?;
    let mut channels = [&[][..]; MAX_METER_CHANNELS];
    let mut channel_count = 0;
    for (index, port) in data.ports.iter().enumerate() {
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

    match kind {
        MeterKind::Source => meter.source.push_planar(&channels[..channel_count]),
        MeterKind::Output => meter.output.push_planar(&channels[..channel_count]),
    }
    .ok()
    .flatten()
}

fn process_mono_buffers(
    input: *mut f32,
    output: *mut f32,
    sample_count: u32,
    gain: &mut GainStage,
    target_gain_db: f32,
) {
    // SAFETY: Both PipeWire port buffers are valid for sample_count f32
    // samples for the duration of this process cycle.
    let input = unsafe { std::slice::from_raw_parts(input, sample_count as usize) };
    let output = unsafe { std::slice::from_raw_parts_mut(output, sample_count as usize) };
    process_samples(input, output, gain, target_gain_db);
}

fn process_samples(input: &[f32], output: &mut [f32], gain: &mut GainStage, target_gain_db: f32) {
    output.copy_from_slice(input);
    let result = gain.process_interleaved(output, 1, target_gain_db);
    debug_assert!(result.is_ok(), "validated filter gain must process");
}

fn filter_name(name: &str) -> Result<CString, FilterCreateError> {
    CString::new(name).map_err(|_| FilterCreateError::NameContainsNul)
}

fn raw_direction(direction: PortDirection) -> pipewire::spa::sys::spa_direction {
    match direction {
        PortDirection::Input => pipewire::spa::sys::SPA_DIRECTION_INPUT,
        PortDirection::Output => pipewire::spa::sys::SPA_DIRECTION_OUTPUT,
    }
}

fn meter_channel(channel: &str) -> Channel {
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

fn target_gain_bits(target_gain_db: f32) -> Result<u32, &'static str> {
    target_gain_db
        .is_finite()
        .then(|| target_gain_db.to_bits())
        .ok_or("target gain must be finite")
}

pub struct UnconnectedFilter {
    raw: NonNull<sys::pw_filter>,
    ports: Vec<OwnedPort>,
    callback_data: Box<FilterCallbackData>,
    listener: Option<Box<pipewire::spa::sys::spa_hook>>,
    _main_thread_only: PhantomData<Rc<()>>,
}

pub struct ConnectedFilter {
    filter: UnconnectedFilter,
}

struct OwnedPort {
    _raw: NonNull<c_void>,
    descriptor: PortDescriptor,
}

#[repr(C)]
struct PortData {
    _reserved: u8,
}

impl UnconnectedFilter {
    pub fn new(loop_: &Loop, name: &str) -> Result<Self, FilterCreateError> {
        let name = filter_name(name)?;
        let mut callback_data = Box::<FilterCallbackData>::default();
        let properties = properties! {
            "media.type" => "Audio",
            "media.category" => "Filter",
            "media.role" => "DSP",
            "node.autoconnect" => "false",
            "object.linger" => "false",
        };

        // SAFETY: The loop and static event table outlive the returned filter.
        // pw_filter_new_simple takes ownership of properties. No callback data is
        // supplied callback data has a stable heap address and outlives raw.
        // The handle remains confined to the main-loop thread.
        let raw = unsafe {
            sys::pw_filter_new_simple(
                loop_.as_raw_ptr(),
                name.as_ptr(),
                properties.into_raw(),
                &FILTER_EVENTS,
                (&mut *callback_data as *mut FilterCallbackData).cast(),
            )
        };
        let raw = NonNull::new(raw).ok_or(FilterCreateError::CreationFailed)?;
        Ok(Self {
            raw,
            ports: Vec::new(),
            callback_data,
            listener: None,
            _main_thread_only: PhantomData,
        })
    }

    pub fn new_on_core(
        core: &pipewire::core::CoreRc,
        name: &str,
    ) -> Result<Self, FilterCreateError> {
        let name = filter_name(name)?;
        let mut callback_data = Box::<FilterCallbackData>::default();
        let properties = properties! {
            "media.type" => "Audio",
            "media.category" => "Filter",
            "media.role" => "DSP",
            "node.autoconnect" => "false",
            "object.linger" => "false",
        };

        // SAFETY: core remains alive independently through its Rc owner, and
        // pw_filter_new takes ownership of the properties.
        let raw =
            unsafe { sys::pw_filter_new(core.as_raw_ptr(), name.as_ptr(), properties.into_raw()) };
        let raw = NonNull::new(raw).ok_or(FilterCreateError::CreationFailed)?;
        // The heap allocation keeps the hook address stable until Drop removes
        // it before destroying the filter.
        let mut listener: Box<pipewire::spa::sys::spa_hook> =
            Box::new(unsafe { std::mem::zeroed() });
        // SAFETY: raw is valid, listener has a stable heap address, the static
        // event table outlives the filter, and callback_data stays boxed.
        unsafe {
            sys::pw_filter_add_listener(
                raw.as_ptr(),
                (&mut *listener as *mut pipewire::spa::sys::spa_hook).cast(),
                &FILTER_EVENTS,
                (&mut *callback_data as *mut FilterCallbackData).cast(),
            );
        }
        Ok(Self {
            raw,
            ports: Vec::new(),
            callback_data,
            listener: Some(listener),
            _main_thread_only: PhantomData,
        })
    }

    pub fn add_mono_port(
        &mut self,
        direction: PortDirection,
        name: impl Into<String>,
        channel: impl Into<String>,
    ) -> Result<&PortDescriptor, PortCreateError> {
        if self.state().0 != FilterState::Unconnected {
            return Err(PortCreateError::FilterNotUnconnected);
        }
        let descriptor = PortDescriptor {
            direction,
            name: name.into(),
            channel: channel.into(),
        };
        let properties = properties! {
            "format.dsp" => "32 bit float mono audio",
            "port.name" => descriptor.name.as_str(),
            "audio.channel" => descriptor.channel.as_str(),
        };
        let direction = raw_direction(direction);

        // SAFETY: raw is an unconnected filter owned by self. PipeWire takes
        // ownership of properties and keeps the returned port data alive until
        // the owning filter is destroyed.
        let raw = unsafe {
            sys::pw_filter_add_port(
                self.raw.as_ptr(),
                direction,
                sys::pw_filter_port_flags_PW_FILTER_PORT_FLAG_MAP_BUFFERS,
                std::mem::size_of::<PortData>(),
                properties.into_raw(),
                std::ptr::null_mut(),
                0,
            )
        };
        let raw = NonNull::new(raw).ok_or(PortCreateError::CreationFailed)?;
        self.callback_data.ports.push(CallbackPort {
            raw,
            direction: descriptor.direction,
            channel: descriptor.channel.clone(),
            gain: GainStage::default(),
        });
        self.ports.push(OwnedPort {
            _raw: raw,
            descriptor,
        });
        Ok(&self
            .ports
            .last()
            .expect("the new port was just stored")
            .descriptor)
    }

    pub fn ports(&self) -> impl ExactSizeIterator<Item = &PortDescriptor> {
        self.ports.iter().map(|port| &port.descriptor)
    }

    pub fn enable_meter(&mut self, sample_rate: u32) -> Result<(), ebur128_stream::Error> {
        let channels: Vec<_> = self
            .callback_data
            .ports
            .iter()
            .filter(|port| port.direction == PortDirection::Input)
            .map(|port| meter_channel(&port.channel))
            .collect();
        self.callback_data.meter = Some(MeterState {
            sample_rate,
            source: LoudnessMeter::new(sample_rate, &channels)?,
            output: LoudnessMeter::new(sample_rate, &channels)?,
            channels,
        });
        Ok(())
    }

    pub fn node_id(&self) -> Option<u32> {
        // SAFETY: raw is owned by self and stays valid until Drop.
        let node_id = unsafe { sys::pw_filter_get_node_id(self.raw.as_ptr()) };
        (node_id != sys::PW_ID_ANY).then_some(node_id)
    }

    pub fn state(&self) -> (FilterState, Option<String>) {
        let mut error = std::ptr::null();
        // SAFETY: raw is owned by self and stays valid until Drop.
        let state = unsafe { sys::pw_filter_get_state(self.raw.as_ptr(), &mut error) };
        let error = if error.is_null() {
            None
        } else {
            // PipeWire owns this error string for the lifetime of the state.
            unsafe { CStr::from_ptr(error) }
                .to_str()
                .ok()
                .map(str::to_owned)
        };
        (FilterState::from_raw(state), error)
    }

    pub fn connect_inactive(self) -> Result<ConnectedFilter, FilterConnectError> {
        // SAFETY: raw is an unconnected filter owned by self. No parameters
        // are supplied, and INACTIVE prevents processing until explicitly
        // enabled by a later processing implementation.
        let result = unsafe {
            sys::pw_filter_connect(
                self.raw.as_ptr(),
                sys::pw_filter_flags_PW_FILTER_FLAG_INACTIVE
                    | sys::pw_filter_flags_PW_FILTER_FLAG_RT_PROCESS,
                std::ptr::null_mut(),
                0,
            )
        };
        if result < 0 {
            return Err(FilterConnectError(result));
        }
        Ok(ConnectedFilter { filter: self })
    }
}

impl ConnectedFilter {
    pub fn set_target_gain_db(&self, target_gain_db: f32) -> Result<(), &'static str> {
        let target_gain_bits = target_gain_bits(target_gain_db)?;
        self.filter
            .callback_data
            .target_gain_bits
            .store(target_gain_bits, Ordering::Relaxed);
        Ok(())
    }

    pub fn target_gain_db(&self) -> f32 {
        f32::from_bits(
            self.filter
                .callback_data
                .target_gain_bits
                .load(Ordering::Relaxed),
        )
    }

    pub fn latest_meter_snapshot(&self) -> Option<MeterSnapshot> {
        self.filter.callback_data.latest_metrics.read()
    }

    pub fn set_active(&mut self, active: bool) -> Result<(), FilterActivationError> {
        // SAFETY: the connected filter is uniquely owned by self. PipeWire
        // synchronizes the requested state change with its processing loop.
        let result = unsafe { sys::pw_filter_set_active(self.filter.raw.as_ptr(), active) };
        if result < 0 {
            return Err(FilterActivationError(result));
        }
        Ok(())
    }

    pub fn node_id(&self) -> Option<u32> {
        self.filter.node_id()
    }

    pub fn state(&self) -> (FilterState, Option<String>) {
        self.filter.state()
    }

    pub fn ports(&self) -> impl ExactSizeIterator<Item = &PortDescriptor> {
        self.filter.ports()
    }
}

impl FilterState {
    fn from_raw(state: sys::pw_filter_state) -> Self {
        match state {
            sys::pw_filter_state_PW_FILTER_STATE_ERROR => Self::Error,
            sys::pw_filter_state_PW_FILTER_STATE_UNCONNECTED => Self::Unconnected,
            sys::pw_filter_state_PW_FILTER_STATE_CONNECTING => Self::Connecting,
            sys::pw_filter_state_PW_FILTER_STATE_PAUSED => Self::Paused,
            sys::pw_filter_state_PW_FILTER_STATE_STREAMING => Self::Streaming,
            state => Self::Unknown(state),
        }
    }
}

impl Drop for UnconnectedFilter {
    fn drop(&mut self) {
        if let Some(listener) = self.listener.take() {
            pipewire::spa::utils::hook::remove(*listener);
        }
        // SAFETY: self uniquely owns raw. PipeWire stops callbacks before
        // returning, and callback_data remains alive until after this method.
        unsafe { sys::pw_filter_destroy(self.raw.as_ptr()) };
    }
}

#[cfg(test)]
mod tests;
