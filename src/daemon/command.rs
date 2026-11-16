// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::normalization::SignalDomain;

pub(super) const USAGE: &str = "usage: loudnessd msg status|status-json|reload|enable|disable|set APP playback|capture on|off|reset APP|volume NODE LEVEL|mute NODE on|off|export";

#[derive(Debug, PartialEq)]
pub(super) enum Command {
    Status,
    StatusJson,
    Reload,
    Enable,
    Disable,
    Set {
        application_id: String,
        domain: SignalDomain,
        enabled: bool,
    },
    Reset {
        application_id: String,
    },
    Export,
    Volume {
        node_id: u32,
        level: f32,
    },
    Mute {
        node_id: u32,
        muted: bool,
    },
}

pub(super) fn parse(request: &str) -> Result<Command, String> {
    let fields: Vec<_> = request.split('\t').collect();
    match fields.as_slice() {
        ["status"] => Ok(Command::Status),
        ["status-json"] => Ok(Command::StatusJson),
        ["reload"] => Ok(Command::Reload),
        ["enable"] => Ok(Command::Enable),
        ["disable"] => Ok(Command::Disable),
        ["set", application_id, domain, enabled] => Ok(Command::Set {
            application_id: (*application_id).to_owned(),
            domain: parse_domain(domain)?,
            enabled: parse_enabled(enabled)?,
        }),
        ["reset", application_id] => Ok(Command::Reset {
            application_id: (*application_id).to_owned(),
        }),
        ["export"] => Ok(Command::Export),
        ["volume", node, level] => {
            let level: f32 = level.parse().map_err(|_| "invalid volume level")?;
            if !level.is_finite() || !(0.0..=2.0).contains(&level) {
                return Err("volume level must be finite and in 0..2".to_owned());
            }
            Ok(Command::Volume {
                node_id: node.parse().map_err(|_| "invalid node id")?,
                level,
            })
        }
        ["mute", node, muted] => Ok(Command::Mute {
            node_id: node.parse().map_err(|_| "invalid node id")?,
            muted: match *muted {
                "on" => true,
                "off" => false,
                _ => return Err("mute expects on or off".to_owned()),
            },
        }),
        _ => Err(USAGE.to_owned()),
    }
}

fn parse_domain(value: &str) -> Result<SignalDomain, String> {
    match value {
        "playback" => Ok(SignalDomain::Playback),
        "capture" => Ok(SignalDomain::Capture),
        _ => Err(format!("unknown direction: {value}")),
    }
}

fn parse_enabled(value: &str) -> Result<bool, String> {
    match value {
        "on" | "true" => Ok(true),
        "off" | "false" => Ok(false),
        _ => Err(format!("expected on or off, got: {value}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_command_shape() {
        assert_eq!(parse("status").unwrap(), Command::Status);
        assert_eq!(parse("status-json").unwrap(), Command::StatusJson);
        assert_eq!(parse("reload").unwrap(), Command::Reload);
        assert_eq!(parse("enable").unwrap(), Command::Enable);
        assert_eq!(parse("disable").unwrap(), Command::Disable);
        assert_eq!(
            parse("set\torg.example.Player\tplayback\ton").unwrap(),
            Command::Set {
                application_id: "org.example.Player".to_owned(),
                domain: SignalDomain::Playback,
                enabled: true,
            }
        );
        assert_eq!(
            parse("set\torg.example.Recorder\tcapture\tfalse").unwrap(),
            Command::Set {
                application_id: "org.example.Recorder".to_owned(),
                domain: SignalDomain::Capture,
                enabled: false,
            }
        );
        assert_eq!(
            parse("reset\torg.example.Player").unwrap(),
            Command::Reset {
                application_id: "org.example.Player".to_owned(),
            }
        );
        assert_eq!(parse("export").unwrap(), Command::Export);
    }

    #[test]
    fn validates_playback_volume_and_mute_commands() {
        assert_eq!(
            parse("volume\t42\t0.5").unwrap(),
            Command::Volume {
                node_id: 42,
                level: 0.5
            }
        );
        assert_eq!(
            parse("mute\t42\ton").unwrap(),
            Command::Mute {
                node_id: 42,
                muted: true
            }
        );
        for level in ["NaN", "inf", "-0.1", "2.1", "bogus"] {
            assert!(parse(&format!("volume\t42\t{level}")).is_err());
        }
        assert!(parse("volume\t-1\t0.5").is_err());
        assert!(parse("mute\t42\ttrue").is_err());
    }

    #[test]
    fn reports_specific_invalid_set_values() {
        assert_eq!(
            parse("set\tplayer\tmonitor\ton").unwrap_err(),
            "unknown direction: monitor"
        );
        assert_eq!(
            parse("set\tplayer\tplayback\tmaybe").unwrap_err(),
            "expected on or off, got: maybe"
        );
    }

    #[test]
    fn rejects_unknown_or_incomplete_commands_with_usage() {
        assert_eq!(parse("unknown").unwrap_err(), USAGE);
        assert_eq!(parse("set\tplayer\tplayback").unwrap_err(), USAGE);
        assert_eq!(parse("status\textra").unwrap_err(), USAGE);
    }
}
