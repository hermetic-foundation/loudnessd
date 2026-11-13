# Three-layer loudness dynamics implementation checks

## Scope

Implementation verification on 2026-11-12 for the
[three-layer dynamics design](../loudness-dynamics.md): 30-second P90 history,
immediate audio-path cuts with a one-second hold, and recovery limited by
default to 0.2 dB/s and +6 dB. Playback and capture remain independent.

The implementation was an uncommitted working copy based on `b59acecf`.
The pre-existing `flake.lock` edit was preserved and used by these local checks.
No new semantic version, release tag, or immutable release candidate was
qualified by this run.

## Results

| Check | Result |
| --- | --- |
| Rust library non-live tests | 196 passed |
| CLI tests | 4 passed |
| Public module-path tests | 3 passed |
| Ignored live PipeWire tests on a private server | 8 passed |
| Total Rust tests across those runs | 211 passed |
| Rust formatting | Passed |
| Clippy, all targets, warnings denied | Passed |
| Nix formatting | Passed |
| NixOS module and RTKit-policy derivation checks | Built and passed |
| `nix flake check --no-build` | Passed for x86_64-linux; aarch64-linux omitted |

Build/test commands used the Nix development environment, two Cargo jobs, and
CPU affinity limited to CPUs 0 and 1. Nix realization used one job and two
cores. Representative commands:

```console
nix develop --max-jobs 1 --cores 2 --command taskset -c 0,1 env CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 cargo test --locked --all-targets
nix develop --max-jobs 1 --cores 2 --command taskset -c 0,1 env CARGO_BUILD_JOBS=2 cargo clippy --locked --all-targets -- -D warnings
nix build .#checks.x86_64-linux.module .#checks.x86_64-linux.realtime-policy --no-link --max-jobs 1 --cores 2
nix flake check --no-build --max-jobs 1 --cores 2
```

The eight live tests ran serially against a temporary PipeWire 1.6.8 server.
Its native socket and XDG runtime/config/state/cache directories were private.
Only a dummy driver, two virtual null sinks, and a disposable link were
provisioned; no hardware devices or desktop application streams were loaded.
The null-sink link supplied the graph snapshot test's required ports and link.
The temporary server and all fixture artifacts were removed afterward.

## Regression coverage

- P90 rank and duration weighting, including brief bursts that do not move P90.
- Quiet/loud/quiet histories, partial expiration, silent gaps, and bounded storage.
- Instant downward changes and configurable legacy main-controller slew.
- One-second fast-cut holds, overlapping stronger cuts, and no upward snapback.
- Handover after the main loop resumes later than the hold expiry.
- Simulated multi-minute recovery, without waiting minutes in wall-clock time.
- Generated stereo samples through the actual processing routine, including
  linked gain, finite output, fast attenuation, and a stale higher gain request.
- Independent stream/direction state and cleanup of ended-stream history.
- Actual elapsed control time, early wakeups, and no fabricated observation
  weight after a stalled main loop.
- History aging while bypassed or connecting.
- Selective reload decisions and restoration of gain, hold, and event sequence
  before a replacement filter obtains its first meter reading.
- Status JSON compatibility and percentile-based convergence scoring.
- Nix/TOML defaults, directional overrides, range validation, omission of null
  cut rates, and preservation of authoritative external config files.

## Limits of this evidence

These checks did not deploy the new implementation, restart the user's audio
service, change application or master mixer settings, or suspend the machine.
They did not run a new hardware-backed listening session or a 30-minute soak.
The complete Nix Rust-package derivation and CI were not built/run as part of
this verification; the Rust code was compiled and tested in the Nix development
environment, while the focused module/policy derivations were built separately.

Historical validation reports describe earlier implementations, not qualification
of these new dynamics. Fresh deployment/listening evidence and the remaining
[release gates](../release-gates.md) are still required before a release is
declared qualified.
