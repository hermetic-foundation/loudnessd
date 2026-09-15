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

## Planned PipeWire backend

The production backend will use native PipeWire nodes rather than recording
streams through PulseAudio compatibility APIs. Each application stream will be
routed through an inline EBU R128 meter and gain stage. Capture uses a separate
graph and controller. Clipping protection belongs in loudnessd's own gain
control rather than an external effects pipeline.

The daemon must remain disabled until integration tests establish that it:

- never links playback control to capture or monitor sources;
- keeps every stream control loop independent;
- preserves Wine and Proton streams as nodes appear and disappear;
- bypasses cleanly without interrupting playback;
- restores no stale gain when a stream identity is reused;
- never writes controller gain into WirePlumber's persistent stream volume.

The last requirement follows from the retired prototype: it wrote a 200%
Chromium stream volume through `pactl`, and WirePlumber restored that broad
application-level value onto later Chromium streams.

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

