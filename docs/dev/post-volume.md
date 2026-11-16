# Downstream Playback Controls

## IPC Contract

```console
loudnessd msg volume NODE LEVEL
loudnessd msg mute NODE on
loudnessd msg mute NODE off
```

`NODE` is the original application's numeric PipeWire node ID, not its filter
ID. `LEVEL` is finite cubic UI volume in the inclusive range 0..2; the linear
audio multiplier is `LEVEL^3`. Mute preserves the stored volume. Commands
return `ok` or `error: ...`; callers must inspect the response and must not
silently retry failed commands against the upstream mixer.

Managed playback entries in `status-json` advertise
`post_volume_supported: true` before and after migration. Capture entries
advertise false. `user_volume` (UI units) and `user_muted` are null before
successful migration and populated afterward. Old status JSON without these
fields remains deserializable. Connecting playback entries advertise the
implementation but reject commands until their route is healthy.

## Audio And Handoff

The signal path is source meter/P90/fastcut -> normalization gain -> user gain
-> final true-peak limiter -> output meter. Source measurements and requested
normalization gain never divide by metadata-derived effective volume.
Output measurements intentionally include user controls. Ordinary volume/mute
changes use a five-millisecond linear-amplitude ramp, including to/from zero.
Monitor convergence uses the source percentile plus normalization gain, excludes
user mute/zero volume, and avoids final-output fallback when manually trimmed.

Only the first explicit command migrates a stream. The daemon reads fresh
original Props, preserves logical channelVolumes/mute (scalar volume when
channelVolumes is absent), acknowledges a silent filter hold, journals recovery
state, writes unity/unmuted Props, and verifies fresh enumeration readback.
Both channel and soft controls are neutralized: softVolumes/softMute can
override channel controls in PipeWire's audioconvert implementation.
Only volume-related properties are serialized in Node.set_param; channel maps
and unrelated Props are not rewritten.

After readback, the held callback consumes/discards 100 ms of actual graph
audio before resetting its meters, limiter delay, fastcut gain/hold/sequence,
and normalization gain. This excludes ordinary queued DSP buffers and SPA
volume ramps, rather than assuming metadata is sample-synchronous. The main
controller drops only that playback stream's P90/history and retained dynamics.
Only then is downstream gain released and migrated status published.
A paused graph that cannot acknowledge/drain the handoff returns an error.

## Lifecycle And Limits

- Bypass, policy disable, and graceful shutdown restore the desired logical
  volume/mute to the original node before removing its managed route. They do
  not restore the historical volume from before the most recent slider action.
- Filter replacement on reload or route repair temporarily restores upstream
  controls, then automatically re-migrates the retained user settings after the
  replacement route becomes healthy. Another slider action is not required.
- A private `loudnessd-volumes.json` beside the IPC socket is atomically replaced
  and fsynced before upstream neutralization or subsequent user-setting changes.
  Startup restores matching live nodes before route recovery/discovery. A
  PipeWire server cookie and object serial prevent replay onto another server
  or a reused node ID. Failed restoration preserves the journal and fails
  startup rather than silently discarding the desired controls.
- SIGKILL cannot execute restoration: originals remain unity until a daemon
  restart replays the journal. Audio continuity and mute are not guaranteed
  during that crash/restart interval. This is recovery on restart, not an
  independent crash watchdog. Do not treat this as a safety mute.
- An unresponsive original client (for example SIGSTOP) cannot acknowledge
  Props restoration. The daemon reports failure and preserves its recovery
  journal; recovery requires that client to resume and the daemon to restart.
- Node-only IPC cannot distinguish a stale UI command issued after an ID has
  already been reused. Identity is checked throughout each handoff and journal
  replay; an optional serial in the public command remains future work.
- Asymmetric channelVolumes, missing readable controls, and original logical
  gains outside 0..8 are rejected. Channel balance is not silently flattened.
- Concurrent upstream mixer writers are not coordinated with downstream
  ownership. Once migrated, clients must use this IPC for these controls.
  Arbitrary upstream plugin delays or custom long SPA ramps beyond the 100 ms
  drain are not qualified by the ordinary audioconvert handoff.
- No hardware listening, suspend/resume, long soak, or deployment is implied by
  the private-graph test. Those remain release qualification work.

## Qualification

`tests/live/post-volume.py` launches a disposable PipeWire server, policy-only
WirePlumber, a null sink, and generated stereo playback in a private runtime.
It checks first-command unity handoff, soft override versus logical controls,
mute preservation, removal of contaminated gain history, independent source and
normalization gain, downstream output attenuation, reload retention, bypass,
SIGKILL/restart journal recovery, and graceful-shutdown restoration. It never
connects to the desktop server. Run from the repository with PipeWire tools,
WirePlumber, and Python available:

```console
nix develop --max-jobs 1 --cores 2 --command env CARGO_BUILD_JOBS=2 taskset -c 0,1 cargo build --offline
nix develop --max-jobs 1 --cores 2 --command nix shell --max-jobs 1 --cores 2 nixpkgs#python3 --command env CARGO_BUILD_JOBS=2 taskset -c 0,1 python3 tests/live/post-volume.py target/debug/loudnessd
```
