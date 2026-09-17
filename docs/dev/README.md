# Developer guide

The measurable criteria for promoting a build beyond experimental status are
defined in the [release gates](release-gates.md).
Live test records are kept as dated reports under [`validation/`](validation/).

## Architecture

Every PipeWire media stream gets independent controller state. Application
identity is retained for policy and display, but does not collapse multiple
streams into one gain control. Playback and capture are separate domains, so a
browser's output and microphone capture cannot modify each other's state.

Playback targets `-16 LUFS`. Capture independently targets `-18 LUFS` and uses
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
with a linked-channel `-1 dBTP` guard. The guard uses the BS.1770 four-times
oversampled detector, a fixed 10 ms lookahead, immediate attenuation, and a
smooth release. Its peak window and sample delay are allocated before the
filter becomes active, so the real-time callback does not allocate. One linked
gain preserves stereo balance.

Loudness meters and limiter state for 22.05, 32, 44.1, 48, 88.2, 96, and
192 kHz are constructed before a filter becomes active. These are the rates
supported by the current meter backend. A graph-rate switch resets and selects
one of those retained states; the process callback never constructs or drops
DSP state. Another graph rate, including 176.4 kHz, fails open by slewing
normalization to unity and bypassing metering and limiting rather than
allocating on the real-time thread.

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
keys. Policy matching therefore uses the application identity reported by
`loudnessd --list-streams`, not the inherited Wine client process name.

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

Registry objects for one stream can arrive across several main-loop iterations.
Discovery therefore requires the same complete channel layout to remain stable
for 500 ms before creating a filter. A missing port or link resets that window;
a changed channel set starts it again. This prevents a partially announced
stereo or multichannel stream from being routed as if it were complete mono.

`loudnessd msg disable` performs the same ordered bypass and teardown while
leaving the daemon available for inspection and later re-enablement. Processing
must fail open: an internal error restores the original route rather than
interrupting application audio.

If direct-link restoration or filter deactivation fails, the daemon retains
the active filter, replacement links, controller state, and recovery-journal
entry instead of dropping the only working route. A failed runtime disable or
configuration change reports an error and keeps normalization enabled. A failed
shutdown returns an error so the service manager can restart the daemon and
replay the journal; it never reports a clean stop after losing route ownership.
Restored lingering direct links retain a client proxy only while at least one of
their server-side link IDs exists. The control loop prunes sets after PipeWire
removes their globals, preventing repeated reloads or route moves from retaining
stale proxies indefinitely. Failed or unconfirmed multi-channel restoration
explicitly destroys every partially created lingering global before returning.

Before removing any original link, the daemon atomically records all direct
endpoints in a mode-`0600` runtime journal. A restarted daemon validates that
the recorded ports still exist, restores missing direct links, and only then
resumes discovery and normalization. PipeWire service restarts propagate to
the NixOS user unit, preventing object IDs from being reused across a server
restart. A successfully recovered or already-direct route clears its journal;
journals whose endpoints disappeared are discarded without linking.

## Validation

Pure tests cover loudness windows, controller convergence, channel-independent
policy, gain continuity, peak limiting, route planning, transaction rollback,
runtime overlays, deterministic export, and socket ownership. Ignored live
tests exercise registry discovery, filter registration, transient link cleanup,
and install/bypass transactions against a running PipeWire session using only
disposable nodes. A live two-channel sample-flow test through a disposable null
sink also verified runtime disable, clean bypass, forced process termination,
journal restoration on restart, and resumed normalization. A separate
two-channel capture test linked only the null sink's monitor ports to a
discarding client and verified capture filter insertion and clean bypass.

The process callback acquires every PipeWire DSP buffer exactly once per port
and cycle, then reuses that pointer for metering, gain, and peak limiting.
Calling `pw_filter_get_dsp_buffer` more than once for the same port dequeues
different buffers and previously caused one processed quantum followed by
silence. Unit coverage enforces the one-acquisition invariant and verifies
that one temporarily unavailable channel does not discard other available
buffers.

The Nix flake checks Rust formatting, Clippy, the package test suite, the live
harness with ShellCheck, and the NixOS module, including its generated
immutable TOML and graphical-session user unit. CI does not execute live
PipeWire tests or the soak harness against runner hardware.

The current live-suite evidence is recorded in
[`validation/2026-09-17-live-pipewire-suite.md`](validation/2026-09-17-live-pipewire-suite.md).
PipeWire disconnect detection, stale-journal rejection, and systemd recovery
are recorded in
[`validation/2026-09-17-pipewire-restart.md`](validation/2026-09-17-pipewire-restart.md).
Private playback and duplex-capture graph-rate results are recorded in
[`validation/2026-09-17-sample-rate-matrix.md`](validation/2026-09-17-sample-rate-matrix.md).
A real Chromium Web Audio pause/resume result is recorded in
[`validation/2026-09-17-browser-pause.md`](validation/2026-09-17-browser-pause.md).
Concurrent application exit during ordered bypass is recorded in
[`validation/2026-09-17-bypass-exit.md`](validation/2026-09-17-bypass-exit.md).

`tests/live/audio-soak.sh` starts a private PipeWire daemon and a policy-only
WirePlumber instance in a temporary runtime directory. Playback mode drives two
varied 48 kHz stereo playback streams into a disposable null sink; capture mode
uses one application identity with simultaneous playback and capture streams;
lifecycle mode exercises process and configuration recovery; limiter mode
drives deterministic low-average, high-crest material; memory mode drives eight
independent stereo playback applications.
Hardware monitors are not loaded. Private clients disable realtime scheduling
so an unpaced synthetic graph cannot trip the kernel realtime watchdog;
real-graph integration covers production scheduling separately. The candidate
daemon, IPC socket, graph, fixtures, and recovery journal therefore cannot
observe or modify desktop applications or hardware.

Before monitoring, the harness assigns distinct non-default fixture volumes,
snapshots each stream's complete PipeWire `Props` control state, and destroys
and replaces the sink while both application streams remain alive. Replacement
is identified by PipeWire object serial rather than its recyclable global ID,
and both managed routes must return healthy. The harness also snapshots the
post-recovery target. After pause/resume and monitoring, the complete volume,
mute, channel-volume, soft-volume, monitor-control, and target state must match
exactly. A mismatch retains before-and-after diagnostics and fails the run.

Monitoring requires exactly the active-stream count for its selected mode. The harness retains NDJSON
status, private server and session-manager logs, and one continuous `pw-top`
timeline spanning the monitoring interval. That timeline must contain both
fixture nodes and both loudnessd filter nodes, and every observed error counter
must remain zero. Profiling starts after deliberate endpoint replacement and
pause/resume fault injection, so the steady-state result cannot hide a transient
by sampling only the final graph. Recovery behavior is recorded separately.
Private service logs must contain no error entries, and no audio is retained.
Its exit trap removes the complete private runtime and never starts, stops, or
reloads the user's ordinary PipeWire or loudnessd services.

Native desktop validation on NixOS additionally covered:

- two simultaneous Chromium playback streams with independent `-10.99 LUFS`
  and `-31.14 LUFS` source levels, which settled at `-1.50 dB` and `+17.40 dB`
  gain without changing either stream's persisted 100% volume;
- a real PipeWire capture client connected to the default microphone and
  writing to `/dev/null`, which stayed near the capture silence gate and did
  not receive an inappropriate boost;
- runtime disable and re-enable while a native playback stream remained
  active, including direct-route restoration and filter reinsertion;
- a playback stream created while a muted HDMI sink was the default, whose
  filter outputs followed HDMI before the USB default was restored; and
- compositor focus changes between monitors, which did not change source
  loudness, reset controller state, or interrupt processing.

Post-fix release validation also covered continuous sample flow through both
directions at 100% stream volume:

- a 15-second synthetic playback stream measured `-27.09 LUFS`, reached
  `+13.40 dB` normalization gain, and remained continuous apart from the
  expected initial and route-transition quanta;
- a 15-second synthetic capture stream measured `-27.55 LUFS`, reached
  `+7.35 dB` gain, and remained continuous through filter insertion; and
- live Skyrim playback under Wine matched the `TESV: Skyrim` application
  identity, reached `+14.80 dB` gain from a roughly `-38.40 LUFS` source, and
  kept the application stream at exactly 100% volume.

Changing the physical sink from 25% to 20% and back left source LUFS and
normalization gain unchanged, confirming that master volume remains downstream.
The deployed systemd user service also restarted cleanly during NixOS activation
without restarting PipeWire or the compositor.

Before a stable release, validation still needs subjective sustained listening
across varied content and active Wine/Proton capture coverage. Wine playback
has been validated with Skyrim, but no Wine capture stream was available during
this test pass.

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

The chorus samples average `-13.1 LUFS`, which originally became the playback
target. Live use showed that this treated the loudness of an already loud
master as the desired level for every source: ordinary Helium content around
`-21 LUFS` received about 7 dB of boost and repeatedly approached the true-peak
limiter. The generic target was therefore lowered to `-16 LUFS`, preserving
roughly 3 dB more headroom while still lifting quiet sources. A near-silent
Chromium auxiliary stream measured `-56.2 LUFS`, so the playback silence gate
remains `-50 LUFS` to avoid boosting utility streams.

PipeWire's monitor tap is before sink volume. For a linear sink gain `g`, the
expected digital level after the sink is:

```text
effective_lufs = target_lufs + 20 * log10(g)
```

| Sink volume | Attenuation | Expected level at -16 LUFS |
| --- | ---: | ---: |
| 10% | -20.00 dB | -36.00 LUFS |
| 15% | -16.48 dB | -32.48 LUFS |
| 20% | -13.98 dB | -29.98 LUFS |
