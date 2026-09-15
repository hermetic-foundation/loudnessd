# Developer guide

## Architecture

Every PipeWire media stream gets independent controller state. Application
identity is retained for policy and display, but does not collapse multiple
streams into one gain control. Playback and capture are separate domains, so a
browser's output and microphone capture cannot modify each other's state.

Playback targets `-13 LUFS`. Capture independently targets `-18 LUFS` and uses
a slower boost rate. Both domains use silence gates, deadbands, gain limits,
and asymmetric adjustment rates.

Measurement is incremental and does not retain raw audio. The real-time meter
keeps K-weighting state and fixed momentary and short-term windows. It measures
the source before gain, then the controller slews toward the required absolute
gain rather than accumulating the same loudness error repeatedly.

Playback normalization belongs upstream of the system mixer. The controller
must never compensate for master output gain: changing master volume should
preserve stream balance while applying the system volume curve to the complete
mix.

## PipeWire backend

The daemon uses native PipeWire filters rather than recording monitor streams
through PulseAudio compatibility APIs. One shared PipeWire core owns registry
tracking, transient links, and per-stream filters. Each filter measures its
pre-gain signal, applies a smoothly changing normalization gain, and finishes
with a linked-channel `-1 dBFS` peak guard. The guard has immediate attack and
a smooth release, preserving stereo balance.

Route installation is transactional: create and confirm replacement links,
remove the original direct links, then activate the filter. Failure restores
the direct route and releases replacement links. Shutdown performs the inverse
order. Active routes are reconciled against registry changes so device moves
and relinks do not leave stale filters in the graph.

The implementation and tests enforce that it:

- never links playback control to capture or monitor sources;
- keeps every stream control loop independent;
- bypasses cleanly without interrupting playback;
- never writes controller gain into WirePlumber's persistent stream volume.

Wine and Proton expose ordinary PipeWire application streams, but their
identity metadata is not consistent enough to treat process names as stable
keys. Live Wine/Proton behavior remains a release-validation requirement.

The last requirement follows from the retired prototype: it wrote a 200%
Chromium stream volume through `pactl`, and WirePlumber restored that broad
application-level value onto later Chromium streams.

## Configuration and runtime control

The daemon receives its baseline configuration through `--config PATH` and
never rewrites that file. The NixOS module normally generates the file in the
Nix store, but can pass an externally managed file through
`services.loudnessd.configFile`.

The generated TOML is a separate host-configuration derivation, not part of
the application package. This keeps the `loudnessd` binary identical and
cacheable across machines while allowing each NixOS system closure to carry
its own policy.

Runtime control uses a mode-`0600` per-user Unix socket and a `loudnessd msg`
client, following the command pattern used by compositors such as Niri.
Runtime changes are overlays held in daemon memory. They disappear on restart
unless the user exports the merged effective configuration with
`loudnessd msg export` and deliberately persists it. Export writes TOML to
standard output; it does not mutate the baseline configuration.

The command contract includes status, reload, enable, disable, setting or
resetting one application's directional policy, and exporting effective
configuration. `reload` rereads the original `--config` path. Requests have a
bounded read time so an incomplete client cannot stall the control loop.

## Graph ownership and cleanup

Every processing node and replacement link is owned by the daemon's PipeWire
connection and is non-lingering. Disabling or stopping processing first
creates lingering direct links, deactivates each filter, then removes the
daemon-owned replacement links and nodes. Lingering is intentional for restored
direct routes: otherwise a clean daemon disconnect would silence the stream.

`loudnessd msg disable` performs the same ordered bypass and teardown while
leaving the daemon available for inspection and later re-enablement. Processing
must fail open: an internal error restores the original route rather than
interrupting application audio.

Before removing any original link, the daemon atomically records all direct
endpoints in a mode-`0600` runtime journal. A restarted daemon validates that
the recorded ports still exist, restores missing direct links, and only then
resumes discovery and normalization. PipeWire service restarts propagate to
the NixOS user unit, preventing object IDs from being reused across a server
restart. Journals whose endpoints disappeared are discarded without linking.

## Validation

Pure tests cover loudness windows, controller convergence, channel-independent
policy, gain continuity, peak limiting, route planning, transaction rollback,
runtime overlays, deterministic export, and socket ownership. Ignored live
tests exercise registry discovery, filter registration, transient link cleanup,
and install/bypass transactions against a running PipeWire session using only
disposable nodes. A live two-channel sample-flow test through a disposable null
sink also verified runtime disable, clean bypass, forced process termination,
journal restoration on restart, and resumed normalization.

The Nix flake checks the Rust package and evaluates the NixOS module, including
its generated immutable TOML and graphical-session user unit. Before a stable
release, validation still needs sustained listening tests and active Wine/Proton
playback and capture coverage.

## Playback calibration

The initial playback target was calibrated from the official YouTube upload of
Rick Astley's "Never Gonna Give You Up" in an isolated Chromium profile. The
test bypassed EasyEffects and routed Chromium directly to PipeWire at 100%
stream gain. Every accepted sample reported the expected video ID, no active
advertisement, 100% YouTube UI volume, and 100% PipeWire stream volume.

| Segment | Integrated | Loudness range | True peak |
| --- | ---: | ---: | ---: |
| Post-intro, 28 seconds | -14.0 LUFS | 2.0 LU | -1.3 dBFS |
| First chorus, 48 seconds | -12.5 LUFS | 2.6 LU | -2.3 dBFS |
| Second chorus, 108 seconds | -12.9 LUFS | 4.7 LU | -2.6 dBFS |
| Final chorus, 163 seconds | -13.9 LUFS | 1.1 LU | -1.9 dBFS |

The chorus samples average `-13.1 LUFS`, supporting the generic `-13 LUFS`
target. A near-silent Chromium auxiliary stream measured `-56.2 LUFS`, so the
playback silence gate is `-50 LUFS` to avoid boosting utility streams.

PipeWire's monitor tap is before sink volume. For a linear sink gain `g`, the
expected digital level after the sink is:

```text
effective_lufs = target_lufs + 20 * log10(g)
```

| Sink volume | Attenuation | Expected level at -13 LUFS |
| --- | ---: | ---: |
| 10% | -20.00 dB | -33.00 LUFS |
| 15% | -16.48 dB | -29.48 LUFS |
| 20% | -13.98 dB | -26.98 LUFS |
