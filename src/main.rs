// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    fs::OpenOptions,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
};

use loudnessd::{
    ControllerBank, Decision, Observation, SignalDomain, UserConfig, daemon, ipc,
    monitor::{self, MonitorOptions},
    pipewire_backend::snapshot_streams,
};

fn usage() {
    eprintln!(
        "loudnessd [--config PATH] [--daemon | --list-streams]\n\
         loudnessd monitor [--duration SECONDS] [--interval MILLISECONDS] [--output PATH] [--expect-active COUNT]\n\n\
         loudnessd msg status|status-json|reload|enable|disable|export\n\
         loudnessd msg set APP playback|capture on|off\n\
         loudnessd msg reset APP\n\n\
         Daemon mode without --config uses $XDG_CONFIG_HOME/loudnessd/config.toml.\n\n\
         Dry-run protocol on stdin: DOMAIN APPLICATION_ID STREAM_ID LUFS ELAPSED_MILLISECONDS\n\
         DOMAIN is playback or capture"
    );
}

const STARTER_CONFIG: &str = "\
# Directional defaults apply to applications without an explicit entry.
[defaults]
playback = true
capture = true
";

fn xdg_config_path() -> Result<PathBuf, &'static str> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
        && path.is_absolute()
    {
        return Ok(path.join("loudnessd/config.toml"));
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|path| path.join(".config/loudnessd/config.toml"))
        .ok_or("cannot locate the default config: HOME is unset")
}

fn create_starter_config(path: &Path) -> io::Result<bool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(STARTER_CONFIG.as_bytes())?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let all_arguments: Vec<_> = std::env::args().skip(1).collect();
    if all_arguments.first().map(String::as_str) == Some("msg") {
        let response = ipc::send(&ipc::default_socket_path()?, &all_arguments[1..])?;
        print!("{response}");
        return Ok(());
    }
    if all_arguments.first().map(String::as_str) == Some("monitor") {
        let options = parse_monitor_options(&all_arguments[1..])?;
        println!(
            "{}",
            serde_json::to_string_pretty(&monitor::run(&options)?)?
        );
        return Ok(());
    }
    let mut config_path = None;
    let mut daemon = false;
    let mut list_streams = false;
    let mut arguments = all_arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--config" => config_path = Some(arguments.next().ok_or("--config needs a path")?),
            "--daemon" => daemon = true,
            "--list-streams" => list_streams = true,
            "--help" | "-h" => {
                usage();
                return Ok(());
            }
            unknown => return Err(format!("unknown argument: {unknown}").into()),
        }
    }

    if daemon && list_streams {
        return Err("--daemon and --list-streams are mutually exclusive".into());
    }

    let config_path = match (config_path, daemon) {
        (Some(path), _) => Some(PathBuf::from(path)),
        (None, true) => {
            let path = xdg_config_path()?;
            if create_starter_config(&path)? {
                eprintln!("loudnessd: created starter config at {}", path.display());
            }
            Some(path)
        }
        (None, false) => None,
    };

    let baseline = if let Some(path) = config_path.as_ref() {
        let source = std::fs::read_to_string(path)?;
        UserConfig::from_toml(&source)?
    } else {
        UserConfig::default()
    };
    let mut controllers = ControllerBank::defaults();
    controllers
        .apply_user_config(baseline.clone())
        .map_err(|error| format!("invalid controller configuration: {error}"))?;

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

    if daemon {
        daemon::run(
            controllers,
            config_path.expect("daemon mode always resolves a config path"),
            baseline,
            ipc::default_socket_path()?,
        )?;
        return Ok(());
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

fn parse_monitor_options(
    arguments: &[String],
) -> Result<MonitorOptions, Box<dyn std::error::Error>> {
    let mut options = MonitorOptions::default();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--duration" => {
                let seconds: u64 = arguments
                    .next()
                    .ok_or("--duration needs seconds")?
                    .parse()?;
                options.duration = std::time::Duration::from_secs(seconds);
            }
            "--interval" => {
                let milliseconds: u64 = arguments
                    .next()
                    .ok_or("--interval needs milliseconds")?
                    .parse()?;
                options.interval = std::time::Duration::from_millis(milliseconds);
            }
            "--output" => {
                options.output = Some(PathBuf::from(
                    arguments.next().ok_or("--output needs a path")?,
                ));
            }
            "--expect-active" => {
                let count: usize = arguments
                    .next()
                    .ok_or("--expect-active needs a count")?
                    .parse()?;
                if count == 0 {
                    return Err("--expect-active must be greater than zero".into());
                }
                options.expected_active_streams = Some(count);
            }
            unknown => return Err(format!("unknown monitor argument: {unknown}").into()),
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    fn test_config_path() -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "loudnessd-config-test-{}-{}",
                std::process::id(),
                NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ))
            .join("loudnessd/config.toml")
    }

    #[test]
    fn creates_a_generic_starter_config() {
        let path = test_config_path();

        assert!(create_starter_config(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), STARTER_CONFIG);

        std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn never_overwrites_an_existing_config() {
        let path = test_config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "existing = true\n").unwrap();

        assert!(!create_starter_config(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "existing = true\n");

        std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn parses_expected_active_stream_count() {
        let options = parse_monitor_options(&[
            "--duration".to_owned(),
            "60".to_owned(),
            "--expect-active".to_owned(),
            "8".to_owned(),
        ])
        .unwrap();

        assert_eq!(options.duration, std::time::Duration::from_secs(60));
        assert_eq!(options.expected_active_streams, Some(8));
    }

    #[test]
    fn rejects_zero_expected_active_streams() {
        let error =
            parse_monitor_options(&["--expect-active".to_owned(), "0".to_owned()]).unwrap_err();

        assert_eq!(
            error.to_string(),
            "--expect-active must be greater than zero"
        );
    }
}
