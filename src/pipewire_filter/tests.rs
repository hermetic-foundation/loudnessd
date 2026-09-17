use std::time::{Duration, Instant};

use ebur128_stream::Channel;
use pipewire::loop_::Timeout;

use super::{
    process::{
        CallbackPort, CycleBuffers, ProcessMetrics, PublishedMetrics, meter_channel,
        process_mono_buffers, process_sample_rate, process_samples, target_gain_bits,
    },
    *,
};
use crate::{gain::GainStage, meter::MeterReading};

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
fn process_callback_supports_an_in_place_pipewire_buffer() {
    let mut samples = [0.25, -0.5, 0.75, -1.0];
    let mut gain = GainStage::default();

    // SAFETY: The same live array is valid as both buffers, and the process
    // helper explicitly supports overlapping PipeWire buffers.
    unsafe {
        process_mono_buffers(
            samples.as_mut_ptr(),
            samples.as_mut_ptr(),
            samples.len() as u32,
            &mut gain,
            0.0,
        );
    }

    assert_eq!(samples, [0.25, -0.5, 0.75, -1.0]);
}

#[test]
fn acquires_each_pipewire_port_once_per_cycle() {
    let ports = [
        CallbackPort {
            raw: NonNull::dangling(),
            direction: PortDirection::Input,
            channel: "FL".to_owned(),
            gain: GainStage::default(),
        },
        CallbackPort {
            raw: NonNull::dangling(),
            direction: PortDirection::Output,
            channel: "FL".to_owned(),
            gain: GainStage::default(),
        },
    ];
    let mut acquisitions = 0;

    let buffers = CycleBuffers::acquire_with(&ports, |_| {
        acquisitions += 1;
        NonNull::<f32>::dangling().as_ptr()
    })
    .unwrap();

    assert_eq!(acquisitions, ports.len());
    assert_eq!(buffers.len, ports.len());
}

#[test]
fn keeps_available_buffers_when_one_port_has_no_buffer() {
    let ports = [
        CallbackPort {
            raw: NonNull::dangling(),
            direction: PortDirection::Input,
            channel: "FL".to_owned(),
            gain: GainStage::default(),
        },
        CallbackPort {
            raw: NonNull::dangling(),
            direction: PortDirection::Output,
            channel: "FL".to_owned(),
            gain: GainStage::default(),
        },
    ];
    let mut acquisitions = 0;

    let buffers = CycleBuffers::acquire_with(&ports, |_| {
        acquisitions += 1;
        if acquisitions == 1 {
            NonNull::<f32>::dangling().as_ptr()
        } else {
            std::ptr::null_mut()
        }
    })
    .unwrap();

    assert_eq!(acquisitions, ports.len());
    assert!(buffers.get(0).is_some());
    assert!(buffers.get(1).is_none());
}

#[test]
fn rejects_non_finite_target_gain() {
    assert!(target_gain_bits(f32::NAN).is_err());
    assert!(target_gain_bits(f32::INFINITY).is_err());
    assert_eq!(target_gain_bits(-6.0), Ok((-6.0_f32).to_bits()));
}

#[test]
fn meter_snapshots_are_sequenced() {
    let published = PublishedMetrics::default();
    assert_eq!(published.read(), None);

    published.publish(ProcessMetrics {
        source: MeterReading {
            loudness_lufs: -18.5,
            true_peak_dbtp: Some(-4.0),
            using_short_term: true,
        },
        output: Some(MeterReading {
            loudness_lufs: -13.2,
            true_peak_dbtp: Some(-1.1),
            using_short_term: true,
        }),
        limiter_reduction_db: 0.5,
        maximum_limiter_reduction_db: 1.0,
    });
    assert_eq!(
        published.read(),
        Some(MeterSnapshot {
            sequence: 1,
            source_loudness_lufs: -18.5,
            source_true_peak_dbtp: Some(-4.0),
            output_loudness_lufs: Some(-13.2),
            output_true_peak_dbtp: Some(-1.1),
            limiter_reduction_db: 0.5,
            maximum_limiter_reduction_db: 1.0,
        })
    );

    published.publish(ProcessMetrics {
        source: MeterReading {
            loudness_lufs: -13.0,
            true_peak_dbtp: None,
            using_short_term: false,
        },
        output: None,
        limiter_reduction_db: 0.0,
        maximum_limiter_reduction_db: 1.0,
    });
    assert_eq!(
        published.read(),
        Some(MeterSnapshot {
            sequence: 2,
            source_loudness_lufs: -13.0,
            source_true_peak_dbtp: None,
            output_loudness_lufs: None,
            output_true_peak_dbtp: None,
            limiter_reduction_db: 0.0,
            maximum_limiter_reduction_db: 1.0,
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

#[test]
#[ignore = "requires a live PipeWire user session"]
fn live_core_created_filter_retains_its_core_until_destroyed() {
    let main_loop = pipewire::main_loop::MainLoopRc::new(None).unwrap();
    let context = pipewire::context::ContextRc::new(&main_loop, None).unwrap();
    let core = context.connect_rc(None).unwrap();
    let filter = UnconnectedFilter::new_on_core(&core, "loudnessd-core-lifetime-test").unwrap();

    drop(core);
    drop(context);
    drop(filter);
}
