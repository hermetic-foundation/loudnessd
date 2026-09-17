// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::HashMap,
    error::Error,
    fs::File,
    io::{self, BufWriter, Write},
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;

use crate::{
    ipc,
    status::{ControlStatus, DaemonStatus, RouteStatus, StreamLifecycle},
};

const CONVERGENCE_TOLERANCE_LU: f32 = 1.5;

#[derive(Clone, Debug, PartialEq)]
pub struct MonitorOptions {
    pub duration: Duration,
    pub interval: Duration,
    pub output: Option<PathBuf>,
    pub expected_active_streams: Option<usize>,
}

impl Default for MonitorOptions {
    fn default() -> Self {
        Self {
            duration: Duration::from_secs(300),
            interval: Duration::from_secs(1),
            output: None,
            expected_active_streams: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SoakReport {
    pub elapsed_milliseconds: u64,
    pub samples: u64,
    pub ipc_failures: u64,
    pub daemon_restarts: u64,
    pub expected_active_streams: Option<usize>,
    pub minimum_active_streams: Option<usize>,
    pub maximum_active_streams: Option<usize>,
    pub active_stream_shortfall_observations: u64,
    pub skipped_stream_observations: u64,
    pub maximum_skipped_streams: usize,
    pub unhealthy_route_observations: u64,
    pub stalled_callback_observations: u64,
    pub converging_observations: u64,
    pub convergence_eligible_observations: u64,
    pub convergence_passing_observations: u64,
    pub convergence_ratio: Option<f64>,
    pub minimum_gain_db: Option<f32>,
    pub maximum_gain_db: Option<f32>,
    pub boost_observations: u64,
    pub cut_observations: u64,
    pub maximum_limiter_reduction_db: f32,
    pub maximum_output_true_peak_dbtp: Option<f32>,
    pub initial_rss_bytes: Option<u64>,
    pub final_rss_bytes: Option<u64>,
    pub peak_rss_bytes: Option<u64>,
    pub rss_growth_bytes: Option<i64>,
    pub average_cpu_percent: Option<f64>,
}

#[derive(Serialize)]
struct TimedStatus<'a> {
    elapsed_milliseconds: u64,
    status: &'a DaemonStatus,
}

#[derive(Default)]
struct SoakAccumulator {
    report: SoakReport,
    sequences: HashMap<u32, u64>,
    previous_process: Option<ProcessSample>,
    accumulated_cpu_ticks: u64,
    clock_ticks_per_second: Option<u64>,
}

#[derive(Clone, Copy)]
struct ProcessSample {
    pid: u32,
    start_time_ticks: u64,
    cpu_ticks: u64,
}

impl SoakAccumulator {
    fn new(expected_active_streams: Option<usize>) -> Self {
        Self {
            report: SoakReport {
                expected_active_streams,
                ..SoakReport::default()
            },
            ..Self::default()
        }
    }

    fn observe(&mut self, status: &DaemonStatus) {
        self.report.samples += 1;
        self.report.minimum_active_streams = Some(
            self.report
                .minimum_active_streams
                .unwrap_or(status.active)
                .min(status.active),
        );
        self.report.maximum_active_streams = Some(
            self.report
                .maximum_active_streams
                .unwrap_or(status.active)
                .max(status.active),
        );
        if self
            .report
            .expected_active_streams
            .is_some_and(|expected| status.active < expected)
        {
            self.report.active_stream_shortfall_observations += 1;
        }
        if status.skipped > 0 {
            self.report.skipped_stream_observations += 1;
            self.report.maximum_skipped_streams =
                self.report.maximum_skipped_streams.max(status.skipped);
        }
        if let Some(process) = &status.process {
            self.report
                .initial_rss_bytes
                .get_or_insert(process.rss_bytes);
            self.report.final_rss_bytes = Some(process.rss_bytes);
            self.report.peak_rss_bytes = Some(
                self.report
                    .peak_rss_bytes
                    .unwrap_or_default()
                    .max(process.rss_bytes),
            );
            let cpu_ticks = process.user_cpu_ticks + process.system_cpu_ticks;
            let sample = ProcessSample {
                pid: process.pid,
                start_time_ticks: process.start_time_ticks,
                cpu_ticks,
            };
            if let Some(previous) = self.previous_process {
                if (sample.pid, sample.start_time_ticks)
                    == (previous.pid, previous.start_time_ticks)
                {
                    self.accumulated_cpu_ticks = self
                        .accumulated_cpu_ticks
                        .saturating_add(sample.cpu_ticks.saturating_sub(previous.cpu_ticks));
                } else {
                    self.report.daemon_restarts += 1;
                }
            }
            self.previous_process = Some(sample);
            self.clock_ticks_per_second = Some(process.clock_ticks_per_second);
        }
        for stream in &status.streams {
            if stream.route != RouteStatus::Healthy {
                self.report.unhealthy_route_observations += 1;
            }
            if let Some(sequence) = stream.meter_sequence
                && self.sequences.insert(stream.node_id, sequence) == Some(sequence)
                && stream.lifecycle == StreamLifecycle::Active
                && stream.route == RouteStatus::Healthy
                && !matches!(
                    stream.control,
                    ControlStatus::Waiting | ControlStatus::Silence | ControlStatus::Bypass
                )
            {
                self.report.stalled_callback_observations += 1;
            }
            self.report.maximum_limiter_reduction_db = self
                .report
                .maximum_limiter_reduction_db
                .max(stream.limiter_max_db);
            if let Some(output_peak_dbtp) = stream.output_peak_dbtp {
                self.report.maximum_output_true_peak_dbtp = Some(
                    self.report
                        .maximum_output_true_peak_dbtp
                        .map_or(output_peak_dbtp, |maximum| maximum.max(output_peak_dbtp)),
                );
            }

            let carrying_signal = stream.lifecycle == StreamLifecycle::Active
                && stream.route == RouteStatus::Healthy
                && !matches!(
                    stream.control,
                    ControlStatus::Waiting | ControlStatus::Silence | ControlStatus::Bypass
                );
            if carrying_signal {
                self.report.minimum_gain_db = Some(
                    self.report
                        .minimum_gain_db
                        .map_or(stream.gain_db, |minimum| minimum.min(stream.gain_db)),
                );
                self.report.maximum_gain_db = Some(
                    self.report
                        .maximum_gain_db
                        .map_or(stream.gain_db, |maximum| maximum.max(stream.gain_db)),
                );
                if stream.gain_db > 0.01 {
                    self.report.boost_observations += 1;
                } else if stream.gain_db < -0.01 {
                    self.report.cut_observations += 1;
                }
            }

            let eligible = stream.lifecycle == StreamLifecycle::Active
                && stream.route == RouteStatus::Healthy
                && !stream.gain_clamped
                && stream.limiter_db <= 0.01
                && stream.control == ControlStatus::Settled;
            if stream.control == ControlStatus::Converging {
                self.report.converging_observations += 1;
            }
            if eligible && let Some(output_lufs) = stream.output_lufs {
                self.report.convergence_eligible_observations += 1;
                if (output_lufs - stream.target_lufs).abs() <= CONVERGENCE_TOLERANCE_LU {
                    self.report.convergence_passing_observations += 1;
                }
            }
        }
    }

    fn finish(mut self, elapsed: Duration) -> SoakReport {
        self.report.elapsed_milliseconds = duration_milliseconds(elapsed);
        self.report.convergence_ratio =
            (self.report.convergence_eligible_observations != 0).then(|| {
                self.report.convergence_passing_observations as f64
                    / self.report.convergence_eligible_observations as f64
            });
        self.report.rss_growth_bytes = self
            .report
            .initial_rss_bytes
            .zip(self.report.final_rss_bytes)
            .map(|(initial, last)| last as i64 - initial as i64);
        self.report.average_cpu_percent =
            self.clock_ticks_per_second.and_then(|ticks_per_second| {
                let seconds = elapsed.as_secs_f64();
                (seconds > 0.0 && ticks_per_second > 0).then(|| {
                    self.accumulated_cpu_ticks as f64 / ticks_per_second as f64 / seconds * 100.0
                })
            });
        self.report
    }
}

pub fn run(options: &MonitorOptions) -> Result<SoakReport, Box<dyn Error>> {
    if options.duration.is_zero() {
        return Err("monitor duration must be greater than zero".into());
    }
    if options.interval.is_zero() {
        return Err("monitor interval must be greater than zero".into());
    }

    let socket = ipc::default_socket_path()?;
    let mut output: Box<dyn Write> = match &options.output {
        Some(path) => Box::new(BufWriter::new(File::create(path)?)),
        None => Box::new(io::sink()),
    };
    let started = Instant::now();
    let mut accumulator = SoakAccumulator::new(options.expected_active_streams);

    while started.elapsed() < options.duration {
        let elapsed = started.elapsed();
        match ipc::send(&socket, &["status-json".to_owned()])
            .and_then(|response| serde_json::from_str(&response).map_err(io::Error::other))
        {
            Ok(status) => {
                serde_json::to_writer(
                    &mut output,
                    &TimedStatus {
                        elapsed_milliseconds: duration_milliseconds(elapsed),
                        status: &status,
                    },
                )?;
                writeln!(output)?;
                accumulator.observe(&status);
            }
            Err(error) => {
                accumulator.report.ipc_failures += 1;
                eprintln!("loudnessd: monitor sample failed: {error}");
            }
        }
        let remaining = options.duration.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(options.interval.min(remaining));
    }
    output.flush()?;
    Ok(accumulator.finish(started.elapsed()))
}

fn duration_milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{ProcessStatus, StreamStatus};

    fn status(sequence: u64, output_lufs: f32) -> DaemonStatus {
        DaemonStatus {
            enabled: true,
            managed: 1,
            active: 1,
            skipped: 0,
            process: Some(ProcessStatus {
                pid: 100,
                start_time_ticks: 1_000,
                rss_bytes: 2_000_000 + sequence * 1000,
                user_cpu_ticks: sequence * 10,
                system_cpu_ticks: sequence * 5,
                clock_ticks_per_second: 100,
            }),
            streams: vec![StreamStatus {
                node_id: 9,
                filter_node_id: Some(19),
                filter_state: Some("streaming".to_owned()),
                filter_error: None,
                domain: "playback".to_owned(),
                application: "player".to_owned(),
                meter_sequence: Some(sequence),
                lifecycle: StreamLifecycle::Active,
                route: RouteStatus::Healthy,
                control: ControlStatus::Settled,
                target_lufs: -13.0,
                source_lufs: Some(-20.0),
                source_peak_dbtp: Some(-4.0),
                output_lufs: Some(output_lufs),
                output_peak_dbtp: Some(-1.0),
                gain_db: 7.0,
                gain_clamped: false,
                limiter_db: 0.0,
                limiter_max_db: 0.4,
            }],
            skipped_streams: Vec::new(),
        }
    }

    #[test]
    fn scores_convergence_and_callback_progress() {
        let mut accumulator = SoakAccumulator::default();
        accumulator.observe(&status(1, -13.2));
        accumulator.observe(&status(2, -10.0));
        accumulator.observe(&status(2, -13.0));

        let report = accumulator.finish(Duration::from_secs(3));
        assert_eq!(report.samples, 3);
        assert_eq!(report.daemon_restarts, 0);
        assert_eq!(report.minimum_active_streams, Some(1));
        assert_eq!(report.maximum_active_streams, Some(1));
        assert_eq!(report.stalled_callback_observations, 1);
        assert_eq!(report.convergence_eligible_observations, 3);
        assert_eq!(report.convergence_passing_observations, 2);
        assert_eq!(report.convergence_ratio, Some(2.0 / 3.0));
        assert_eq!(report.minimum_gain_db, Some(7.0));
        assert_eq!(report.maximum_gain_db, Some(7.0));
        assert_eq!(report.boost_observations, 3);
        assert_eq!(report.cut_observations, 0);
        assert_eq!(report.maximum_limiter_reduction_db, 0.4);
        assert_eq!(report.maximum_output_true_peak_dbtp, Some(-1.0));
        assert_eq!(report.initial_rss_bytes, Some(2_001_000));
        assert_eq!(report.final_rss_bytes, Some(2_002_000));
        assert_eq!(report.rss_growth_bytes, Some(1000));
        assert_eq!(report.average_cpu_percent, Some(5.0));
    }

    #[test]
    fn reports_active_stream_shortfalls() {
        let mut accumulator = SoakAccumulator::new(Some(2));
        accumulator.observe(&status(1, -13.0));
        let mut complete = status(2, -13.0);
        complete.managed = 2;
        complete.active = 2;
        complete.streams.push(StreamStatus {
            node_id: 10,
            ..complete.streams[0].clone()
        });
        accumulator.observe(&complete);

        let report = accumulator.finish(Duration::from_secs(2));
        assert_eq!(report.expected_active_streams, Some(2));
        assert_eq!(report.minimum_active_streams, Some(1));
        assert_eq!(report.maximum_active_streams, Some(2));
        assert_eq!(report.active_stream_shortfall_observations, 1);
    }

    #[test]
    fn reports_skipped_streams() {
        let mut accumulator = SoakAccumulator::default();
        let mut skipped = status(1, -13.0);
        skipped.skipped = 2;
        accumulator.observe(&skipped);
        accumulator.observe(&status(2, -13.0));

        let report = accumulator.finish(Duration::from_secs(2));
        assert_eq!(report.skipped_stream_observations, 1);
        assert_eq!(report.maximum_skipped_streams, 2);
    }

    #[test]
    fn detects_restarts_without_treating_reset_cpu_ticks_as_wraparound() {
        let mut accumulator = SoakAccumulator::default();
        let mut before = status(10, -13.0);
        let before_process = before.process.as_mut().unwrap();
        before_process.user_cpu_ticks = 80;
        before_process.system_cpu_ticks = 20;
        accumulator.observe(&before);

        let mut restarted = status(1, -13.0);
        let restarted_process = restarted.process.as_mut().unwrap();
        restarted_process.pid = 101;
        restarted_process.start_time_ticks = 2_000;
        restarted_process.user_cpu_ticks = 4;
        restarted_process.system_cpu_ticks = 1;
        accumulator.observe(&restarted);

        let mut after = restarted;
        let after_process = after.process.as_mut().unwrap();
        after_process.user_cpu_ticks = 12;
        after_process.system_cpu_ticks = 3;
        accumulator.observe(&after);

        let report = accumulator.finish(Duration::from_secs(2));
        assert_eq!(report.daemon_restarts, 1);
        assert_eq!(report.average_cpu_percent, Some(5.0));
    }

    #[test]
    fn reports_but_does_not_score_in_progress_slew() {
        let mut accumulator = SoakAccumulator::default();
        let mut converging = status(1, -20.0);
        converging.streams[0].control = ControlStatus::Converging;
        accumulator.observe(&converging);

        let report = accumulator.finish(Duration::from_secs(1));
        assert_eq!(report.converging_observations, 1);
        assert_eq!(report.convergence_eligible_observations, 0);
    }

    #[test]
    fn excludes_clamped_limited_and_silent_observations() {
        let mut accumulator = SoakAccumulator::default();
        let mut clamped = status(1, -20.0);
        clamped.streams[0].gain_clamped = true;
        accumulator.observe(&clamped);
        let mut limited = status(2, -20.0);
        limited.streams[0].limiter_db = 0.5;
        accumulator.observe(&limited);
        let mut silent = status(2, -20.0);
        silent.streams[0].control = ControlStatus::Silence;
        accumulator.observe(&silent);

        let report = accumulator.finish(Duration::from_secs(3));
        assert_eq!(report.convergence_eligible_observations, 0);
        assert_eq!(report.convergence_ratio, None);
        assert_eq!(report.stalled_callback_observations, 0);
    }

    #[test]
    fn reports_gain_path_coverage_only_while_carrying_signal() {
        let mut accumulator = SoakAccumulator::default();
        let mut boosted = status(1, -13.0);
        boosted.streams[0].gain_db = 4.0;
        accumulator.observe(&boosted);

        let mut cut = status(2, -13.0);
        cut.streams[0].gain_db = -3.0;
        accumulator.observe(&cut);

        let mut silent = status(3, -13.0);
        silent.streams[0].control = ControlStatus::Silence;
        silent.streams[0].gain_db = -9.0;
        accumulator.observe(&silent);

        let report = accumulator.finish(Duration::from_secs(3));
        assert_eq!(report.minimum_gain_db, Some(-3.0));
        assert_eq!(report.maximum_gain_db, Some(4.0));
        assert_eq!(report.boost_observations, 1);
        assert_eq!(report.cut_observations, 1);
    }
}
