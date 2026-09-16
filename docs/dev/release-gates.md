# Release gates

`loudnessd` remains experimental until every gate below has current evidence.
Passing unit tests alone is not sufficient because routing, timing, and device
lifecycle behavior depend on a live PipeWire graph.

## Automated checks

- `cargo fmt --check`, Clippy with warnings denied, and all non-live Rust tests
  pass.
- Every ignored live PipeWire test passes serially in a real user session.
- `nix flake check` passes and builds both the package and NixOS module checks.
- The reusable `Nix CI` workflow passes on the exact commit proposed for a
  release.
- The release commit has no unresolved critical or high-severity defect.
  Medium-severity limitations must be documented in the user guide.

## Playback continuity and control

- An eight-hour soak with at least two simultaneous stereo streams completes
  without an unexpected silence interval longer than two graph quanta.
- The soak includes continuous music, speech, intermittent browser audio,
  silence, and content that reaches both boost and cut paths.
- For non-silent windows that are not gain-clamped or limiter-bound, at least
  95% of post-filter short-term readings settle within `1.5 LU` of the target
  after the configured slew time.
- No application stream volume, mute state, channel volume, or persistent
  WirePlumber target changes during normalization.
- Peak output does not exceed the configured limiter threshold, stereo channels
  receive linked limiter reduction, and no non-finite sample reaches an output.

## Capture isolation

- Native microphone capture is validated with a recorder, a browser WebRTC
  client, and a bidirectional voice application.
- Playback and capture belonging to the same application maintain independent
  meters, controllers, gain, and policy.
- Capture normalization never links a monitor source unless the user selected
  that monitor as the application's actual source.
- Available Wine or Proton capture clients complete the same continuity and
  isolation checks. If no such client is available, stable release remains
  blocked and the missing evidence is recorded.

## Lifecycle and recovery

Each scenario restores a working direct route without changing application
volume or requiring a PipeWire restart:

- clean daemon disable and stop;
- forced daemon termination followed by restart and journal recovery;
- PipeWire and WirePlumber restart;
- default sink or source change while streams are active;
- active device removal and reconnection;
- application exit during route installation and bypass;
- suspend and resume; and
- configuration reload with both valid and invalid input.

No stale loudnessd node, link, recovery journal, or temporary test artifact may
remain after cleanup.

## Compatibility and fail-open behavior

- Mono and stereo planar floating-point playback and capture pass live tests at
  every sample rate supported by the test devices.
- Ambiguous, unsupported, or multichannel topology is left on its original
  direct route and reports a specific reason.
- Native, Wine, and Proton application identity is documented from observed
  metadata; policy matching does not rely on an inherited process name when a
  more specific application identity exists.
- Any internal routing, filter, metering, or control failure bypasses or leaves
  the stream direct rather than interrupting audio.

## Resource stability

During the eight-hour soak:

- resident memory remains below `32 MiB` with eight active stereo streams and
  grows by less than `2 MiB` between hour one and hour eight;
- average CPU use remains below `5%` of one core on the Ryzen 5 3400G baseline
  for two active 48 kHz stereo streams, measured from daemon process CPU ticks;
  comparable hardware must record its processor and measurement method; and
- there are no daemon restarts, real-time callback allocation regressions,
  unbounded log growth, or control-socket stalls.

Hardware, PipeWire version, sample rate, stream count, and measurement commands
must accompany the recorded results so regressions can be compared fairly.

## Release evidence

Store the release candidate commit, test date, environment, commands, summary
statistics, and pass/fail result in the developer documentation. A semantic
version tag may be created only from the current `main` commit after all gates
pass. The tag's GitHub release must reference the successful `Nix CI` run.
