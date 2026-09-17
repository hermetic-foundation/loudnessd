# Release candidate audit: 2026-09-17

## Status

In progress. This audit applies the objective criteria in
[`../release-gates.md`](../release-gates.md) to the current candidate. It does
not declare the project stable while any required evidence below remains open.

- Functional candidate: `a18fdc0a7e2f074d0fc5a23631134ed8ab9fbb59`
- Current `main`: `f3da343dc6b16638d17fac756e1c13b6c8825777`
- Package closure shared by both revisions:
  `/nix/store/yzv8lmfhc7mdph5124g8zhqqcb50fjm6-loudnessd-0.1.0`
- Host: `midi-desktop-1`, x86_64 Linux 6.18.51
- PipeWire: 1.6.8
- Playback target under evaluation: `-16 LUFS`

Commits after the functional candidate change CI policy or documentation only;
the package closure under soak is unchanged. A release still requires checks
to pass against the exact tagged `main` revision.

## Automated checks

| Gate | State | Evidence |
| --- | --- | --- |
| Rust formatting, tests, and Clippy | Pass | Local checks passed with warnings denied. |
| ShellCheck | Pass | The live harness passed the flake check. |
| Ignored live PipeWire tests | Pass | All six passed serially in the user session. |
| Nix package and module checks | Pass | `nix flake check` and all-system evaluation passed locally. |
| Dependency policy | Pass | `cargo audit`, `cargo machete`, and the reproducible `cargo-deny` check passed. |
| Exact-main GitHub CI | Open | The current workflow is queued because the organization runner is quarantined. |
| Native ARM build | Not claimed | ARM evaluates, but no native ARM build evidence exists. |

## Playback and resources

Short gain-path, pause/resume, limiter, eight-stream, rate-matrix, and real
desktop integration probes pass. The evidence is recorded in this directory,
including [`2026-09-17-gain-path-qualification.md`](2026-09-17-gain-path-qualification.md)
and [`2026-09-17-browser-pause.md`](2026-09-17-browser-pause.md).

The final eight-hour synthetic soak for the exact functional package started
at 2026-09-17 15:30:27 CDT and remains in progress. Its artifacts use the
prefix `playback-soak-8h-final-a18fdc0a-target16` under
`$XDG_STATE_HOME/loudnessd/validation`. This gate remains open until the
harness exits successfully and its final summary passes every continuity,
convergence, profiler, process-identity, CPU, and memory assertion.

## Capture isolation

| Gate | State | Evidence |
| --- | --- | --- |
| Synthetic non-silent duplex capture | Pass | A 120-second private-graph run exercised independent playback and capture gain in both directions. |
| Hardware-backed capture routing | Partial | Real Scarlett routes remained healthy, but the source stayed below the silence gate. |
| Additional capture client | Partial | A second real capture client confirmed discovery and routing, but its microphone signal remained silent. |
| Same-identity duplex routing | Pass | One application identity held healthy playback and capture routes with independent callback progress and gain state for 60 observations. |
| Non-silent physical microphone convergence | Open | A sustained signal above the capture silence gate is still required. |

The latest physical probe ran for 120 seconds with 119 healthy observations,
zero skipped streams, and a meter sequence advancing from 6 to 6,098. Its
silence-gate result is recorded in
[`2026-09-17-target16-webrtc.md`](2026-09-17-target16-webrtc.md).

## Lifecycle and recovery

| Scenario | State |
| --- | --- |
| Clean disable and stop | Pass |
| Forced daemon exit, bypass, and restart recovery | Pass |
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
interrupts the active desktop session.

## Codebase audit

- The largest Rust module is 766 lines; no source file approaches the
  2,000-line limit.
- Daemon discovery, commands, recovery, and reporting are nested modules with
  separate ownership boundaries.
- Real-time processing and tests are colocated under `pipewire_filter`.
- No `TODO`, `FIXME`, `HACK`, or `XXX` marker remains in Rust or Nix sources.
- Production unsafe blocks are limited to the PipeWire FFI boundary and the
  libc process-metrics query.

## Release blockers

1. Complete and accept the final eight-hour soak.
2. Demonstrate non-silent physical microphone convergence through a
   hardware-backed capture stream.
3. Pass controlled suspend/resume.
4. Obtain a green reusable GitHub workflow on the exact release revision after
   the organization runner leaves quarantine.
5. Re-run the audit against the final `main`, resolve every high-severity
   defect, and only then create a semantic version tag and GitHub release.
