# Real-time playback fixture qualification: 2026-09-17

## Scope

This qualification validates the private playback fixture used by the extended
strict soak. It does not replace the required eight-hour run.

- Harness commit: `1c3d08834542`
- Documentation-only successor: `8d0c01438879`
- Package: `/nix/store/qn1ffhymvaqhwy73wpfkxkxfrqd7wk05-loudnessd-0.1.0`
- Host: `midi-desktop-1`, x86_64 Linux 6.18.51
- PipeWire: 1.6.8
- Format: 48 kHz, stereo, planar floating point
- Duration: 600 seconds
- Limits: `CPUQuota=200%`, `MemoryMax=512M`
- Artifact prefix:
  `$XDG_STATE_HOME/loudnessd/validation/playback-probe-10m-1c3d0883-1789695608`

The fixture used two pull-driven `audiotestsrc` generators and two loopback
modules loaded by the private PipeWire daemon. Both persistent application
nodes remained alive while the sink was replaced. The generators were detached
before the deliberate fault and reattached to the recovered application routes
while the graph was suspended.

## Result

The harness exited successfully after 596 observations:

- active streams remained exactly 2;
- skipped streams, unhealthy routes, stalled callbacks, IPC failures, and
  daemon restarts remained 0;
- all 218 convergence-eligible observations passed, for a ratio of `1.0`;
- gain ranged from `-5.899991 dB` to `+18.0 dB`, exercising 389 cut and 202
  boost observations;
- maximum limiter reduction was `10.188274 dB`;
- maximum output true peak was `-1.0961416 dBTP`;
- daemon RSS remained exactly `8,998,912` bytes; and
- average daemon CPU use was `3.5233%` of one core.

The continuous profiler covered both application nodes, both source nodes,
both server-owned loopback transports, the replacement sink, and both
loudnessd filters. Every node had a zero error-counter delta. The two
application nodes retained their bounded recovery baseline of 2; every other
node remained at 0. Private PipeWire and WirePlumber logs contained no error
entry, and the system coredump count remained unchanged.

The exact fixture topology is therefore accepted for the full strict playback
soak. Release acceptance still requires that eight-hour run to pass unchanged.
