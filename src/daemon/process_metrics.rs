// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::status::ProcessStatus;

pub fn read() -> Option<ProcessStatus> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rss_bytes = parse_rss_bytes(&status)?;
    let (user_cpu_ticks, system_cpu_ticks, start_time_ticks) = parse_process_stat(&stat)?;
    // SAFETY: sysconf only queries process-global kernel configuration.
    let clock_ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let clock_ticks_per_second = u64::try_from(clock_ticks_per_second).ok()?;
    Some(ProcessStatus {
        pid: std::process::id(),
        start_time_ticks,
        rss_bytes,
        user_cpu_ticks,
        system_cpu_ticks,
        clock_ticks_per_second,
    })
}

fn parse_rss_bytes(status: &str) -> Option<u64> {
    let kibibytes = status.lines().find_map(|line| {
        let value = line.strip_prefix("VmRSS:")?.trim();
        value.strip_suffix("kB")?.trim().parse::<u64>().ok()
    })?;
    kibibytes.checked_mul(1024)
}

fn parse_process_stat(stat: &str) -> Option<(u64, u64, u64)> {
    let fields: Vec<_> = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .collect();
    Some((
        fields.get(11)?.parse().ok()?,
        fields.get(12)?.parse().ok()?,
        fields.get(19)?.parse().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_linux_resident_memory() {
        assert_eq!(
            parse_rss_bytes("Name:\tloudnessd\nVmRSS:\t   1536 kB\n"),
            Some(1_572_864)
        );
    }

    #[test]
    fn parses_cpu_and_start_ticks_after_a_parenthesized_name() {
        let stat = "42 (loudness daemon) R 1 2 3 4 5 6 7 8 9 10 123 45 0 0 0 0 0 0 654321";
        assert_eq!(parse_process_stat(stat), Some((123, 45, 654_321)));
    }
}
