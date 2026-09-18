// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::normalization::SignalDomain;

pub(super) const USAGE: &str = "usage: loudnessd msg status|status-json|reload|enable|disable|set APP playback|capture on|off|reset APP|export";

#[derive(Debug, Eq, PartialEq)]
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
