// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io::{self, BufRead};

use loudnessd::{
    ControllerBank, Decision, Observation, SignalDomain, UserConfig,
    pipewire_backend::snapshot_streams,
};

fn usage() {
    eprintln!(
        "loudnessd [--config PATH] [--list-streams]\n\nDry-run protocol on stdin: DOMAIN APPLICATION_ID STREAM_ID LUFS ELAPSED_MILLISECONDS\nDOMAIN is playback or capture"
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config_path = None;
    let mut list_streams = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--config" => config_path = Some(arguments.next().ok_or("--config needs a path")?),
            "--list-streams" => list_streams = true,
            "--help" | "-h" => {
                usage();
                return Ok(());
            }
            unknown => return Err(format!("unknown argument: {unknown}").into()),
        }
    }

    if list_streams {
        for stream in snapshot_streams()? {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                match stream.domain {
                    SignalDomain::Playback => "playback",
                    SignalDomain::Capture => "capture",
                },
                stream.node_id,
                stream.application_id.as_deref().unwrap_or("-"),
                stream.application_name.as_deref().unwrap_or("-"),
                stream.process_binary.as_deref().unwrap_or("-"),
                stream.media_name.as_deref().unwrap_or("-"),
            );
        }
        return Ok(());
    }

    let mut controllers = ControllerBank::defaults();
    if let Some(path) = config_path {
        let source = std::fs::read_to_string(path)?;
        controllers.apply_user_config(UserConfig::from_toml(&source)?);
    }
    eprintln!("loudnessd: dry-run controller; no audio graph changes will be made");

    for line in io::stdin().lock().lines() {
        let line = line?;
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.is_empty() || fields[0].starts_with('#') {
            continue;
        }
        if fields.len() != 5 {
            eprintln!("ignored malformed observation: {line}");
            continue;
        }
        let domain = match fields[0] {
            "playback" => SignalDomain::Playback,
            "capture" => SignalDomain::Capture,
            other => {
                eprintln!("ignored unknown signal domain: {other}");
                continue;
            }
        };
        let application_id = fields[1];
        let stream_id = fields[2];
        let lufs: f32 = fields[3].parse()?;
        let elapsed_ms: f32 = fields[4].parse()?;
        let decision = controllers.observe(
            domain,
            application_id,
            stream_id,
            Observation {
                lufs,
                elapsed_seconds: elapsed_ms / 1000.0,
            },
        );
        let (action, gain_db) = match decision {
            Decision::Bypass => ("bypass", 0.0),
            Decision::Silence { gain_db } => ("silence", gain_db),
            Decision::Hold { gain_db } => ("hold", gain_db),
            Decision::Adjust { gain_db, .. } => ("adjust", gain_db),
        };
        println!(
            "{}\t{application_id}\t{stream_id}\t{action}\t{gain_db:.3}",
            fields[0]
        );
    }
    Ok(())
}
