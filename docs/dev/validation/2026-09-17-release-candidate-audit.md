# Release candidate audit: 2026-09-17

## Status

In progress. This audit applies the objective criteria in
[`../release-gates.md`](../release-gates.md) to the current candidate. It does
not declare the project stable while any required evidence below remains open.

- Functional package revision: `6680c9fba550`
- Evidence-base `main`: `f048b8e67225`
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

The final strict varied-playback soak started at 2026-09-17 20:51:29 CDT and
remains in progress. Its artifacts use the prefix
`playback-soak-8h-server-10c8aa56-1789696289` under
`$XDG_STATE_HOME/loudnessd/validation`. It runs the pre-disconnect-fix package;
the later functional changes affect only native teardown after fatal server
loss, not the metering, controller, limiter, or process callback exercised by
this soak.

The first eight-stream resource run used ordinary-priority file feeders. After
2.8 hours every loudnessd filter had accumulated 3--10 profiler errors, so the
run was rejected rather than reclassifying those counters. Its artifacts use
the prefix `memory-soak-8h-final-f2865340`, with the observed deltas retained in
`memory-soak-8h-final-f2865340.early-profiler-errors.tsv`.

Memory mode now uses eight PipeWire server-side generators and persistent
loopback application nodes. A 60-second optimized-package qualification held
all source, transport, application, filter, and sink counters at zero with
stable 13.65 MiB RSS. The replacement eight-hour run started at
2026-09-17 22:30:30 CDT from the functional package closure. Its artifacts use
the prefix `memory-soak-8h-server-f888695a-1789702230`.

Both long-run gates remain open until the harnesses exit successfully and their
summaries pass every continuity, profiler, process-identity, CPU, control-state,
and memory assertion.

The active long runs predate test-harness commit `565ec448bc4a`, which makes
nonzero monitor continuity counters, insufficient playback convergence, and
resource-memory limit violations fail automatically. Restarting the runs would
discard their uninterrupted evidence, so this audit will apply those same
assertions directly to the retained summaries. Every subsequent run enforces
them in `audio-soak.sh` before returning success.

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
| System suspend and resume | Open |

Passing recovery evidence is recorded in
[`2026-09-17-bypass-exit.md`](2026-09-17-bypass-exit.md),
[`2026-09-17-pipewire-restart.md`](2026-09-17-pipewire-restart.md), and the
playback qualification records. The controlled Scarlett profile-cycle result
is recorded in
[`2026-09-17-device-reconnect.md`](2026-09-17-device-reconnect.md), together
with kernel USB removal and re-enumeration. Suspend remains open because it
would invalidate the two active uninterrupted soaks. It will run after both
services reach their natural terminal state.

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

1. Complete and accept both final eight-hour soaks.
2. Pass controlled suspend/resume.
3. Activate the built personal NixOS closure and verify the packaged service,
   immutable config, route health, and desktop playback/capture behavior.
4. Re-run this audit against final local `main` and resolve every high-severity
   local defect.

## Deferred external release gates

These remain required before publishing a stable release, but are intentionally
outside the current local-preparation goal:

1. Obtain a green reusable GitHub workflow on the exact release revision after
   the organization runner work is complete.
2. Create the semantic version tag and GitHub release from that verified
   revision.
