# `-16 LUFS` playback and browser WebRTC probe: 2026-09-17

## Result

Partial pass. The revised playback target reduced Helium's steady boost by
about 3 dB and the true-peak limiter contained every observed peak. A real
Chromium WebRTC client captured the default physical microphone through an
independent healthy route, but the room signal remained below the capture
silence gate, so this run does not qualify non-silent microphone convergence.

## Environment

- Host: `midi-desktop-1`, x86_64 Linux 6.18.51
- PipeWire: 1.6.8
- Chromium: 153.0.8010.36, isolated headless profile
- Candidate package:
  `/nix/store/10zcm980g5fc69zxmpm1l5ilma9nnq5x-loudnessd-0.1.0`
- Playback target: `-16 LUFS`
- Capture target: `-18 LUFS`
- Physical source: default Scarlett microphone
- Physical sink: USB HIFI Audio at 20%

## Playback comparison

Immediately before the target change, Helium measured about `-20.9 LUFS` and
received about `+7.2 dB` toward the old `-13 LUFS` target. Its output reached
`-13.8 LUFS` and `-0.97 dBTP`, effectively riding the limiter.

With the `-16 LUFS` candidate, an initial steady observation measured
`-19.83 LUFS` at the source, `+4.0 dB` gain, `-16.05 LUFS` output, and
`-1.10 dBTP`. During the subsequent 20-second sample, source loudness varied
from `-23.16` to `-18.74 LUFS`, gain varied from `+3.4` to `+5.8 dB`, and
output varied from `-18.56` to `-14.28 LUFS`. The maximum limiter reduction
was 2.61 dB and the maximum output true peak was `-1.100 dBTP`.

After the bounded sample, quieter content drove gain to about `+7.2 dB` while
remaining peak-limited. This is expected from short-term normalization but
requires subjective listening before the revised target is accepted as final.

## WebRTC capture

The page obtained a real `getUserMedia({ audio: true })` track labelled
`Default`; no synthetic media-device flag was used. Chromium exposed one mono
capture node, and loudnessd maintained an independent healthy capture filter
throughout all 40 monitor samples.

The microphone measured between `-69.72` and `-62.56 LUFS`, remained in the
capture silence state at unity gain, and peaked at `-48.30 dBTP`. This proves
browser WebRTC discovery, routing, silence gating, and playback/capture
isolation. A sustained non-silent microphone signal is still required to prove
capture convergence and limiter behavior.

## Shared monitor result

- 40 samples over 20 seconds
- 2 active healthy streams in every sample
- 0 IPC failures, daemon restarts, skipped streams, unhealthy routes, or
  stalled callbacks
- 0 bytes resident-memory growth
- 3.30% average daemon CPU use on one core
- 90.63% aggregate eligible convergence; the short run included playback gain
  transitions and is not used to pass the 95% steady-state release gate

The isolated Chromium process, temporary profile, page, and capture route were
removed after the run. Helium and the target-trial daemon remained active.

## Extended physical microphone probe

The same candidate subsequently observed a direct `pw-record` client reading
the Scarlett Mic 1 source for 120 seconds. Audio was discarded to `/dev/null`;
the test retained only structured loudnessd observations.

- The capture route was active and healthy for 119 observations, with no
  skipped streams.
- The real-time sequence advanced from 6 to 6,098 without stalling.
- Source loudness remained between `-69.10` and `-68.55 LUFS`; output loudness
  remained between `-69.10` and `-68.54 LUFS`.
- Every observation correctly remained in the silence state at unity gain.
- Maximum true peak was `-51.12 dBTP`, and limiter reduction remained zero.

This longer probe confirms stable physical-device routing and silence-gate
behavior on the release candidate. It still does not satisfy the non-silent
physical-microphone convergence gate because no sustained signal crossed the
capture silence threshold.
