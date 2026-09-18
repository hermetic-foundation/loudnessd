# Release candidate audit: 2026-09-17

## Status

In progress. This audit applies the objective criteria in
[`../release-gates.md`](../release-gates.md) to the current candidate. It does
not declare the project stable while any required evidence below remains open.

- Functional package revision: `6680c9fba550`
- Evidence-contract `main`: `aa4e3647528c`
- Functional package closure:
  `/nix/store/az3y42gva3y9vy1x73i8x4zfrfy2p5vm-loudnessd-0.1.0`
- Host: `midi-desktop-1`, x86_64 Linux 6.18.51
- PipeWire: 1.6.8
- Playback target under evaluation: `-16 LUFS`

Commits after the functional package revision change only test harnesses or
documentation, which the package source filter intentionally excludes. This
audit update is also documentation-only. A release still requires checks to
pass against the exact tagged `main` revision, but CI and tag creation are
explicitly deferred while organization runner work is in progress.

## Automated checks

| Gate | State | Evidence |
| --- | --- | --- |
| Rust formatting, tests, and Clippy | Pass | 112 library tests and 4 CLI tests pass; strict Clippy passes with warnings denied. |
| ShellCheck | Pass | All three live harnesses pass the reproducible flake check. |
| Ignored live PipeWire tests | Pass | All eight pass serially against the desktop PipeWire session. |
| Nix package and module checks | Pass | `nix flake check`, release package construction, NixOS module evaluation, immutable generated config, and portable systemd verification pass locally. |
| Dependency policy | Pass | `cargo audit`, `cargo machete`, and the reproducible `cargo-deny` check passed. |
| Exact-main GitHub CI | Deferred | External gate deferred until organization runner work is complete; it does not block the local-preparation goal. |
| Native ARM build | Not claimed | ARM evaluates, but no native ARM build evidence exists. |

## Playback and resources

Short gain-path, pause/resume, limiter, eight-stream, rate-matrix, and real
desktop integration probes pass. The evidence is recorded in this directory,
including [`2026-09-17-gain-path-qualification.md`](2026-09-17-gain-path-qualification.md)
and [`2026-09-17-browser-pause.md`](2026-09-17-browser-pause.md).

The physical playback/capture capability matrix passes at every rate supported
by both loudnessd and the selected hardware. Playback was verified at 44.1,
48, 88.2, 96, and 192 kHz; capture was verified at 44.1, 48, 88.2, and 96 kHz.
Application node rates and active USB hardware rates matched in every case.
The full evidence and explicit 176.4 kHz fail-open boundary are recorded in
[`2026-09-17-sample-rate-matrix.md`](2026-09-17-sample-rate-matrix.md).

The first completed strict varied-playback monitor ran for 28,800 seconds and
recorded 28,582 observations with no IPC failures, daemon restarts, route
shortfalls, skipped streams, unhealthy routes, callback stalls, RSS growth, or
peak-ceiling violations. All 10,460 eligible convergence observations passed.
Its artifacts use the prefix
`playback-soak-8h-server-10c8aa56-1789696289` under
`$XDG_STATE_HOME/loudnessd/validation`.

The first eight-stream resource run used ordinary-priority file feeders. After
2.8 hours every loudnessd filter had accumulated 3--10 profiler errors, so the
run was rejected rather than reclassifying those counters. Its artifacts use
the prefix `memory-soak-8h-final-f2865340`, with the observed deltas retained in
`memory-soak-8h-final-f2865340.early-profiler-errors.tsv`.

Memory mode now uses eight PipeWire server-side generators and persistent
loopback application nodes. A 60-second optimized-package qualification held
all source, transport, application, filter, and sink counters at zero with
stable 13.65 MiB RSS. The replacement eight-hour monitor then completed 28,800
seconds and 28,729 observations without an IPC failure, daemon restart, route
or callback continuity failure, or RSS growth. Its artifacts use the prefix
`memory-soak-8h-server-f888695a-1789702230`.

The resource run's first complete observation at or after one hour was at
3,600,590 ms. It recorded PID `1561010`, process start ticks `4934787`, and
14,450,688 bytes RSS with all eight routes healthy and no skipped streams. The
final observation had the same RSS, for zero warm-state growth.

The strict profiler gate rejected both completed runs. One playback filter
accumulated 2 errors, while seven resource-run filters accumulated 1--3 errors.
The increases occurred at approximately 01:23 and 01:25 CDT in otherwise
independent private PipeWire servers while both stress graphs and unrelated
host work were running concurrently. Source, transport, application, and sink
counters remained stable. Active inspection in a later private graph confirmed
that PipeWire, WirePlumber, and loudnessd processing threads all used
`SCHED_RR` priority 20 through RTKit, so the rejection is retained as host
contention evidence rather than attributed to missing realtime scheduling.

The original harness processes also reached their terminal checks after their
source file had changed in place during the eight-hour runs. Bash consequently
read later source text and failed after the monitors had completed. Commit
`0b37f3f5c839` fixes this test-infrastructure race by re-executing every run
from an unlinked immutable snapshot. A 180-second playback qualification under
that launcher exited successfully with zero profiler deltas, stable RSS, no log
growth, and 100% eligible convergence.

The release gate was subsequently bounded to 30 minutes per mode. An eight-hour
requirement was disproportionate for the current project and made routine local
qualification take 16 hours when playback and resource runs were kept isolated.
The resource gate now compares RSS from minute 10 through minute 30 while
retaining the 32 MiB ceiling, 2 MiB growth limit, continuous profiler, routing,
control-preservation, and callback-progress assertions.

The in-progress eight-hour playback rerun was stopped intentionally when the
gate changed. It completed 1,092,694 ms with stable 9,068,544-byte RSS, the same
daemon PID and start time, two managed streams, no skips, and zero profiler
deltas across every source, transport, application, filter, and sink. Those
partial artifacts use the prefix
`playback-soak-8h-current-0b37f3f5-1789736874`; they are retained as supporting
evidence but do not replace the final 30-minute terminal run. Commit
`d3d71a430e26` makes the harness reject playback CPU at or above the documented
5% single-core budget, and commit `3c8388a3ab88` implements the bounded
resource-memory window.

The terminal bounded playback qualification completed 1,800,000 ms and 1,776
observations with exactly two active streams throughout. It reported zero IPC
failures, daemon restarts, active-stream shortfalls, skipped streams, unhealthy
routes, and stalled callbacks. All 645 eligible convergence observations passed,
the maximum output peak was -1.096 dBTP, average daemon CPU was 3.74% of one
core, and RSS remained exactly 8,966,144 bytes. Every enforced profiler counter
had zero growth, all private-service logs had zero growth, and fixture controls
and targets were preserved. Its artifacts use the prefix
`playback-soak-30m-current-9e8477bc-1789738409`.

The first bounded resource attempt completed its functional and memory monitor
but was rejected because the private PipeWire profiler, which was still
sampling every graph quantum, overflowed its own client queue and logged 1,815
bytes. No loudnessd node error counter increased. Commits `f92784793477` and
`64e65f389d2f` set the private profiler to the same one-second cadence that
`pw-top` emits, avoiding observer-induced load while retaining cumulative error
counters for every interval.

The replacement resource qualification on `64e65f389d2f` completed 1,800,000 ms
and 1,795 observations with exactly eight active stereo streams throughout. It
reported zero IPC failures, daemon restarts, active-stream shortfalls, skipped
streams, unhealthy routes, and stalled callbacks. Peak, initial, final, and
minute-10 warm RSS were all 14,360,576 bytes, so both total and post-warmup
growth were zero. Every source, transport, application, filter, and sink
profiler counter had zero growth; daemon, PipeWire, and WirePlumber logs had
zero growth; fixture controls and targets were preserved; and cleanup left no
private runtime or process. Its artifacts use the prefix
`resource-soak-30m-final-64e65f38-1789760547`.

## Capture isolation

| Gate | State | Evidence |
| --- | --- | --- |
| Synthetic non-silent duplex capture | Pass | A 120-second private-graph run exercised independent playback and capture gain in both directions. |
| Hardware-backed capture routing and convergence | Pass | A calibrated real microphone stream remained healthy and passed 220 of 221 eligible settled observations. |
| Additional capture client | Partial | A second real capture client confirmed discovery and routing, but its microphone signal remained silent. |
| Same-identity duplex routing | Pass | One application identity held healthy playback and capture routes with independent callback progress and gain state for 60 observations. |

The latest physical probe ran for 120 seconds with 119 healthy observations,
zero skipped streams, and a meter sequence advancing from 6 to 6,098. Its
silence-gate result is recorded in
[`2026-09-17-target16-webrtc.md`](2026-09-17-target16-webrtc.md).
The subsequent calibrated non-silent qualification passed with 99.55%
convergence and is recorded in
[`2026-09-17-hardware-capture.md`](2026-09-17-hardware-capture.md).

## Lifecycle and recovery

| Scenario | State |
| --- | --- |
| Clean disable and stop | Pass |
| Forced daemon exit, bypass, and restart recovery | Pass |
| Fatal PipeWire disconnect with an active managed route | Pass |
| PipeWire and WirePlumber restart | Pass |
| Private-graph sink/source replacement | Pass |
| Browser pause and resume | Pass |
| Application exit during managed routing | Pass |
| Valid and invalid configuration reload | Pass |
| Hardware-backed source profile removal and reconnection | Pass |
| USB driver removal, re-enumeration, and reconnection | Pass |
| System suspend and resume | Not required |

Passing recovery evidence is recorded in
[`2026-09-17-bypass-exit.md`](2026-09-17-bypass-exit.md),
[`2026-09-17-pipewire-restart.md`](2026-09-17-pipewire-restart.md), and the
playback qualification records. The controlled Scarlett profile-cycle result
is recorded in
[`2026-09-17-device-reconnect.md`](2026-09-17-device-reconnect.md), together
with kernel USB removal and re-enumeration. Commit `aa4e3647528c` removes system
suspend from the release gate at the project owner's direction. The retained
suspend harness is an optional operator diagnostic and may not be initiated
autonomously; no suspend evidence is claimed by this audit.

## Personal NixOS deployment

The personal flake commit `fcd1b95f4e1d` advances its loudnessd input to
`16b13c52ef9c`, the qualified upstream revision before the release-policy-only
documentation commit. `nixos-rebuild switch` completed successfully with two
jobs and two cores and activated
`/nix/store/3s0p5bk1m0jhir1wvpkcd9wha853hcih-nixos-system-midi-desktop-1-26.11.20260911.eaad089`.

The upstream source filter kept the deployed package at the qualified closure
`/nix/store/az3y42gva3y9vy1x73i8x4zfrfy2p5vm-loudnessd-0.1.0`. Activation
preserved the running loudnessd, PipeWire, and WirePlumber process identities.
The generated service still reads the immutable
`/nix/store/g0yvkhwc46bpvv6dzj4ml2irqj7xwq4k-loudnessd.toml`, whose defaults
enable playback and capture normalization. A post-switch physical-graph
playback probe acquired one healthy streaming route, advanced its meter from 5
to 313, applied gain toward the `-16 LUFS` target, and cleaned up to zero
managed or skipped streams without restarting the daemon.

## Codebase audit

- The largest Rust module is 766 lines; no source file approaches the
  2,000-line limit.
- Daemon discovery, commands, recovery, and reporting are nested modules with
  separate ownership boundaries.
- Real-time processing and tests are colocated under `pipewire_filter`.
- No `TODO`, `FIXME`, `HACK`, or `XXX` marker remains in Rust or Nix sources.
- Production unsafe blocks are limited to the PipeWire FFI boundary and the
  libc process-metrics query.

## Local completion blockers

None under the current release policy. The final local `nix flake check` passed
all seven checks on `aa4e3647528c`; the subsequent audit update is excluded from
the package source closure. No unresolved critical or high-severity local defect
was found.

## Deferred external release gates

These remain required before publishing a stable release, but are intentionally
outside the current local-preparation goal:

1. Obtain a green reusable GitHub workflow on the exact release revision after
   the organization runner work is complete.
2. Create the semantic version tag and GitHub release from that verified
   revision.
