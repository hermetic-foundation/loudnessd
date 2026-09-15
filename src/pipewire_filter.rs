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

use crate::{gain::GainStage, meter::LoudnessMeter};

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
    pub loudness_lufs: f32,
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
    latest_loudness: PublishedLoudness,
}

struct MeterState {
    sample_rate: u32,
    channels: Vec<Channel>,
    meter: LoudnessMeter,
}

impl Default for FilterCallbackData {
    fn default() -> Self {
        Self {
            ports: Vec::new(),
            target_gain_bits: AtomicU32::new(0.0_f32.to_bits()),
            meter: None,
            latest_loudness: PublishedLoudness::default(),
        }
    }
}

struct CallbackPort {
    raw: NonNull<c_void>,
    direction: PortDirection,
    channel: String,
    gain: GainStage,
}

#[derive(Default)]
struct PublishedLoudness {
    version: AtomicU64,
    loudness_bits: AtomicU32,
}

impl PublishedLoudness {
    fn publish(&self, loudness_lufs: f32) {
        let version = self.version.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(version % 2, 0, "the process callback is a single writer");
        self.loudness_bits
            .store(loudness_lufs.to_bits(), Ordering::Relaxed);
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
            let loudness_lufs = f32::from_bits(self.loudness_bits.load(Ordering::Relaxed));
            let after = self.version.load(Ordering::Acquire);
            if before == after {
                return Some(MeterSnapshot {
                    sequence: before / 2,
                    loudness_lufs,
                });
            }
        }
    }
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

    let sample_rate =
        process_sample_rate(position, data.meter.as_ref().map(|meter| meter.sample_rate));
    meter_source(data, sample_count, sample_rate);

    let target_gain_db = f32::from_bits(data.target_gain_bits.load(Ordering::Relaxed));
    for output_index in 0..data.ports.len() {
        if data.ports[output_index].direction != PortDirection::Output {
            continue;
        }
        let input = data.ports.iter().find_map(|port| {
            (port.direction == PortDirection::Input
                && port.channel == data.ports[output_index].channel)
                .then_some(port.raw)
        });
        let Some(input) = input else { continue };
        let output = &mut data.ports[output_index];
        process_mono_port(
            input,
            output.raw,
            sample_count,
            &mut output.gain,
            target_gain_db,
        );
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

fn meter_source(data: &mut FilterCallbackData, sample_count: u32, sample_rate: Option<u32>) {
    let Some(sample_rate) = sample_rate else {
        return;
    };
    let Some(meter) = data.meter.as_mut() else {
        return;
    };
    if meter.sample_rate != sample_rate {
        let Ok(rebuilt) = LoudnessMeter::new(sample_rate, &meter.channels) else {
            return;
        };
        meter.sample_rate = sample_rate;
        meter.meter = rebuilt;
    }
    let mut channels = [&[][..]; MAX_METER_CHANNELS];
    let mut channel_count = 0;
    for port in data
        .ports
        .iter()
        .filter(|port| port.direction == PortDirection::Input)
    {
        if channel_count == MAX_METER_CHANNELS {
            return;
        }
        // SAFETY: PipeWire keeps the mapped input valid for sample_count f32
        // samples for the duration of this process callback.
        let samples =
            unsafe { sys::pw_filter_get_dsp_buffer(port.raw.as_ptr(), sample_count) }.cast::<f32>();
        if samples.is_null() {
            return;
        }
        // SAFETY: The pointer and length follow from the PipeWire DSP buffer
        // contract above, and the slice does not escape this callback.
        channels[channel_count] =
            unsafe { std::slice::from_raw_parts(samples, sample_count as usize) };
        channel_count += 1;
    }
    if channel_count == 0 {
        return;
    }

    if let Ok(Some(reading)) = meter.meter.push_planar(&channels[..channel_count]) {
        data.latest_loudness.publish(reading.loudness_lufs);
    }
}

fn process_mono_port(
    input: NonNull<c_void>,
    output: NonNull<c_void>,
    sample_count: u32,
    gain: &mut GainStage,
    target_gain_db: f32,
) {
    // SAFETY: PipeWire owns both port data pointers and makes their mapped DSP
    // buffers valid for sample_count f32 samples during the process callback.
    let input =
        unsafe { sys::pw_filter_get_dsp_buffer(input.as_ptr(), sample_count) }.cast::<f32>();
    let output =
        unsafe { sys::pw_filter_get_dsp_buffer(output.as_ptr(), sample_count) }.cast::<f32>();
    if input.is_null() || output.is_null() {
        return;
    }

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
            meter: LoudnessMeter::new(sample_rate, &channels)?,
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
        self.filter.callback_data.latest_loudness.read()
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
mod tests {
    use std::time::{Duration, Instant};

    use pipewire::loop_::Timeout;

    use super::*;

    #[test]
    fn maps_every_known_filter_state() {
        assert_eq!(
            FilterState::from_raw(sys::pw_filter_state_PW_FILTER_STATE_ERROR),
            FilterState::Error
        );
        assert_eq!(FilterState::from_raw(42), FilterState::Unknown(42));
    }

    #[test]
    fn rejects_invalid_names_before_touching_pipewire() {
        assert_eq!(
            filter_name("invalid\0name"),
            Err(FilterCreateError::NameContainsNul)
        );
    }

    #[test]
    fn maps_port_directions_to_the_pipewire_abi() {
        assert_eq!(
            raw_direction(PortDirection::Input),
            pipewire::spa::sys::SPA_DIRECTION_INPUT
        );
        assert_eq!(
            raw_direction(PortDirection::Output),
            pipewire::spa::sys::SPA_DIRECTION_OUTPUT
        );
    }

    #[test]
    fn maps_pipewire_channels_to_ebu_weights() {
        assert_eq!(meter_channel("FL"), Channel::Left);
        assert_eq!(meter_channel("FR"), Channel::Right);
        assert_eq!(meter_channel("LFE"), Channel::Lfe);
        assert_eq!(meter_channel("RL"), Channel::LeftSurround);
        assert_eq!(meter_channel("unknown"), Channel::Other);
    }

    #[test]
    fn zero_gain_preserves_every_sample() {
        let input = [0.25, -0.5, 0.75, -1.0];
        let mut output = [0.0; 4];
        let mut gain = GainStage::default();

        process_samples(&input, &mut output, &mut gain, 0.0);

        assert_eq!(output, input);
    }

    #[test]
    fn process_callback_applies_a_smooth_target_gain() {
        let input = [1.0; 4];
        let mut output = [0.0; 4];
        let mut gain = GainStage::default();

        process_samples(&input, &mut output, &mut gain, -6.0206);

        assert!((output[0] - 1.0).abs() < 0.0001);
        assert!((output[3] - 0.5).abs() < 0.0001);
    }

    #[test]
    fn rejects_non_finite_target_gain() {
        assert!(target_gain_bits(f32::NAN).is_err());
        assert!(target_gain_bits(f32::INFINITY).is_err());
        assert_eq!(target_gain_bits(-6.0), Ok((-6.0_f32).to_bits()));
    }

    #[test]
    fn meter_snapshots_are_sequenced() {
        let published = PublishedLoudness::default();
        assert_eq!(published.read(), None);

        published.publish(-18.5);
        assert_eq!(
            published.read(),
            Some(MeterSnapshot {
                sequence: 1,
                loudness_lufs: -18.5,
            })
        );

        published.publish(-13.0);
        assert_eq!(
            published.read(),
            Some(MeterSnapshot {
                sequence: 2,
                loudness_lufs: -13.0,
            })
        );
    }

    #[test]
    fn process_rate_uses_the_graph_clock_and_validates_the_fraction() {
        let mut position: pipewire::spa::sys::spa_io_position = unsafe { std::mem::zeroed() };
        position.clock.rate.num = 1;
        position.clock.rate.denom = 44_100;
        assert_eq!(process_sample_rate(&position, Some(48_000)), Some(44_100));

        position.clock.rate.num = 0;
        assert_eq!(process_sample_rate(&position, Some(48_000)), Some(48_000));
    }

    #[test]
    #[ignore = "requires a live PipeWire user session"]
    fn live_unconnected_filter_owns_ports_without_registering_a_node() {
        let main_loop = pipewire::main_loop::MainLoopRc::new(None).unwrap();
        let mut filter = UnconnectedFilter::new(main_loop.loop_(), "loudnessd-test").unwrap();
        filter
            .add_mono_port(PortDirection::Input, "input_FL", "FL")
            .unwrap();
        filter
            .add_mono_port(PortDirection::Output, "output_FL", "FL")
            .unwrap();

        assert_eq!(filter.state().0, FilterState::Unconnected);
        assert_eq!(filter.ports().len(), 2);
        assert_eq!(filter.node_id(), None);
    }

    #[test]
    #[ignore = "requires a live PipeWire user session"]
    fn live_filter_registers_an_inactive_node() {
        let main_loop = pipewire::main_loop::MainLoopRc::new(None).unwrap();
        let mut filter = UnconnectedFilter::new(main_loop.loop_(), "loudnessd-test").unwrap();
        filter
            .add_mono_port(PortDirection::Input, "input_FL", "FL")
            .unwrap();
        filter
            .add_mono_port(PortDirection::Output, "output_FL", "FL")
            .unwrap();
        filter.enable_meter(48_000).unwrap();
        let mut filter = filter.connect_inactive().unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while filter.node_id().is_none() && Instant::now() < deadline {
            main_loop
                .loop_()
                .iterate(Timeout::Finite(Duration::from_millis(20)));
        }

        assert!(filter.node_id().is_some());
        assert_eq!(filter.ports().len(), 2);
        assert!(matches!(
            filter.state().0,
            FilterState::Connecting | FilterState::Paused
        ));
        filter.set_target_gain_db(-6.0).unwrap();
        assert_eq!(filter.target_gain_db(), -6.0);
        assert_eq!(filter.latest_meter_snapshot(), None);
        filter.set_active(true).unwrap();
        filter.set_active(false).unwrap();
    }
}
