// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamLifecycle {
    Connecting,
    Active,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStatus {
    Connecting,
    Healthy,
    Superseded,
    Broken,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlStatus {
    Waiting,
    Bypass,
    Silence,
    Settled,
    Converging,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StreamStatus {
    pub node_id: u32,
    pub filter_node_id: Option<u32>,
    #[serde(default)]
    pub filter_state: Option<String>,
    #[serde(default)]
    pub filter_error: Option<String>,
    pub domain: String,
    pub application: String,
    pub meter_sequence: Option<u64>,
    pub lifecycle: StreamLifecycle,
    pub route: RouteStatus,
    pub control: ControlStatus,
    pub target_lufs: f32,
    pub source_lufs: Option<f32>,
    pub source_peak_dbtp: Option<f32>,
    pub output_lufs: Option<f32>,
    pub output_peak_dbtp: Option<f32>,
    pub gain_db: f32,
    pub gain_clamped: bool,
    pub limiter_db: f32,
    pub limiter_max_db: f32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SkippedStreamStatus {
    pub node_id: u32,
    pub domain: String,
    pub application: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessStatus {
    pub pid: u32,
    pub start_time_ticks: u64,
    pub rss_bytes: u64,
    pub user_cpu_ticks: u64,
    pub system_cpu_ticks: u64,
    pub clock_ticks_per_second: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DaemonStatus {
    pub enabled: bool,
    pub managed: usize,
    pub active: usize,
    #[serde(default)]
    pub skipped: usize,
    pub process: Option<ProcessStatus>,
    pub streams: Vec<StreamStatus>,
    #[serde(default)]
    pub skipped_streams: Vec<SkippedStreamStatus>,
}

impl DaemonStatus {
    pub fn to_text(&self) -> String {
        let mut output = format!(
            "enabled={} managed={} active={} skipped={}\n",
            self.enabled, self.managed, self.active, self.skipped
        );
        if let Some(process) = &self.process {
            output.push_str(&format!(
                "process_pid={} process_start_time_ticks={} rss_bytes={} user_cpu_ticks={} system_cpu_ticks={} clock_ticks_per_second={}\n",
                process.pid,
                process.start_time_ticks,
                process.rss_bytes,
                process.user_cpu_ticks,
                process.system_cpu_ticks,
                process.clock_ticks_per_second,
            ));
        } else {
            output.push_str("process=unavailable\n");
        }
        for stream in &self.streams {
            output.push_str(&format!(
                "stream={} filter={} filter_state={} filter_error={} domain={} application={} sequence={} state={} route={} control={} target_lufs={:.2} source_lufs={} source_peak_dbtp={} output_lufs={} output_peak_dbtp={} gain_db={:.2} gain_clamped={} limiter_db={:.2} limiter_max_db={:.2}\n",
                stream.node_id,
                stream
                    .filter_node_id
                    .map(|node_id| node_id.to_string())
                    .unwrap_or_else(|| "unavailable".to_owned()),
                stream.filter_state.as_deref().unwrap_or("unavailable"),
                stream.filter_error.as_deref().unwrap_or("none"),
                stream.domain,
                stream.application,
                stream
                    .meter_sequence
                    .map(|sequence| sequence.to_string())
                    .unwrap_or_else(|| "unavailable".to_owned()),
                stream.lifecycle.as_str(),
                stream.route.as_str(),
                stream.control.as_str(),
                stream.target_lufs,
                format_metric(stream.source_lufs),
                format_metric(stream.source_peak_dbtp),
                format_metric(stream.output_lufs),
                format_metric(stream.output_peak_dbtp),
                stream.gain_db,
                stream.gain_clamped,
                stream.limiter_db,
                stream.limiter_max_db,
            ));
        }
        for stream in &self.skipped_streams {
            output.push_str(&format!(
                "skipped_stream={} domain={} application={} reason={}\n",
                stream.node_id, stream.domain, stream.application, stream.reason,
            ));
        }
        output
    }
}

impl StreamLifecycle {
    fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Active => "active",
        }
    }
}

impl RouteStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Healthy => "healthy",
            Self::Superseded => "superseded",
            Self::Broken => "broken",
        }
    }
}

impl ControlStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Bypass => "bypass",
            Self::Silence => "silence",
            Self::Settled => "settled",
            Self::Converging => "converging",
        }
    }
}

fn format_metric(value: Option<f32>) -> String {
    value
        .map(|value| format!("{value:.2}"))
        .unwrap_or_else(|| "unavailable".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_json_preserve_observability_fields() {
        let status = DaemonStatus {
            enabled: true,
            managed: 1,
            active: 1,
            skipped: 1,
            process: Some(ProcessStatus {
                pid: 123,
                start_time_ticks: 4_567,
                rss_bytes: 1_572_864,
                user_cpu_ticks: 10,
                system_cpu_ticks: 5,
                clock_ticks_per_second: 100,
            }),
            streams: vec![StreamStatus {
                node_id: 42,
                filter_node_id: Some(84),
                filter_state: Some("streaming".to_owned()),
                filter_error: None,
                domain: "playback".to_owned(),
                application: "browser".to_owned(),
                meter_sequence: Some(12),
                lifecycle: StreamLifecycle::Active,
                route: RouteStatus::Healthy,
                control: ControlStatus::Settled,
                target_lufs: -13.0,
                source_lufs: Some(-20.0),
                source_peak_dbtp: Some(-4.0),
                output_lufs: Some(-13.1),
                output_peak_dbtp: Some(-1.2),
                gain_db: 7.0,
                gain_clamped: false,
                limiter_db: 0.2,
                limiter_max_db: 1.5,
            }],
            skipped_streams: vec![SkippedStreamStatus {
                node_id: 99,
                domain: "capture".to_owned(),
                application: "recorder".to_owned(),
                reason: "disabled by policy".to_owned(),
            }],
        };

        let text = status.to_text();
        assert!(text.contains("process_pid=123 process_start_time_ticks=4567"));
        assert!(text.contains("route=healthy control=settled"));
        assert!(text.contains("stream=42 filter=84"));
        assert!(text.contains("filter_state=streaming filter_error=none"));
        assert!(text.contains("output_lufs=-13.10"));
        assert!(text.contains(
            "skipped_stream=99 domain=capture application=recorder reason=disabled by policy"
        ));

        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["streams"][0]["route"], "healthy");
        assert_eq!(json["streams"][0]["filter_node_id"], 84);
        assert_eq!(json["streams"][0]["filter_state"], "streaming");
        assert_eq!(json["streams"][0]["filter_error"], serde_json::Value::Null);
        assert_eq!(json["process"]["pid"], 123);
        assert_eq!(json["process"]["start_time_ticks"], 4_567);
        assert_eq!(json["process"]["rss_bytes"], 1_572_864);
        assert_eq!(json["streams"][0]["meter_sequence"], 12);
        assert_eq!(json["streams"][0]["target_lufs"], -13.0);
        let output_lufs = json["streams"][0]["output_lufs"].as_f64().unwrap();
        assert!((output_lufs - -13.1).abs() < 0.0001);
        assert_eq!(json["streams"][0]["limiter_max_db"], 1.5);
        assert_eq!(json["skipped"], 1);
        assert_eq!(json["skipped_streams"][0]["reason"], "disabled by policy");
    }
}
