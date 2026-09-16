# Native capture probe: 2026-09-16

## Result

Partial pass. Native PipeWire capture routing, direction isolation, callback
progress, bounded resources, and cleanup passed. The physical microphone was
below the capture silence gate, so this run does not establish non-silent
capture convergence.

## Environment

- Commit: `ce036ebf0c1559eafee191279a81157a3e5a43ff`
- Host: `midi-desktop-1`, x86_64 Linux 6.18.51
- PipeWire: 1.6.8
- Source: Focusrite Scarlett Solo Mic 1
- Format: mono float32, 48 kHz
- Recorder: `pw-cat`, with output discarded to `/dev/null`
- Monitor interval: 250 ms

## Observations

- The recorder was classified only as a capture stream.
- The inserted route became active and remained healthy.
- Meter sequence advanced from the real-time callback with no stalled samples.
- Source loudness was approximately -68.1 LUFS and therefore correctly
  classified as silence against the -55 LUFS capture gate.
- No limiter reduction occurred.
- A five-second monitor collected 17 samples with zero IPC failures, unhealthy
  routes, or callback stalls.
- Resident memory was approximately 7.4 MB and grew by 12 KiB during the short
  sample.
- Average daemon CPU use was approximately 1.2% of one core.
- Recorder exit removed the managed stream and every `loudnessd-*` PipeWire
  node. No recorded audio or temporary evidence file was retained.

## Remaining capture evidence

- Repeat the native recorder probe with a sustained non-silent microphone
  signal and verify convergence around -18 LUFS without clamping.
- Validate a browser WebRTC client.
- Validate a bidirectional voice application while simultaneous playback is
  active, proving controller and meter isolation in both directions.
- Validate an available Wine or Proton capture client. Stable release remains
  blocked when no such client is available.
