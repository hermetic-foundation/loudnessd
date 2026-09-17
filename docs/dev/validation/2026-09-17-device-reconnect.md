# Hardware-backed source reconnect: 2026-09-17

## Result

Pass for active source removal and recreation through the ALSA card profile.
The capture route failed open when its Scarlett source disappeared and became
healthy again after the source returned. A literal USB hot-unplug remains a
separate release check.

## Environment

- Candidate package:
  `/nix/store/yzv8lmfhc7mdph5124g8zhqqcb50fjm6-loudnessd-0.1.0`
- Host: `midi-desktop-1`, x86_64 Linux 6.18.51
- PipeWire: 1.6.8
- Device: Focusrite Scarlett Solo USB
- Initial and restored card profile: `HiFi`, index 1
- Removed card profile: `off`, index 0
- Source: Scarlett Mic 1, mono float32 at 48 kHz
- Client: restart-tolerant `pw-record`, discarding audio to `/dev/null`

The Scarlett source is independent of the active Feixiang playback sink. The
test recorded the original profile and source volume and installed a cleanup
trap that restored both before terminating the recorder.

## Observations

1. loudnessd installed a healthy capture route for application identity
   `loudnessd.validation.hw-reconnect`, stream node 123.
2. Switching the card to profile `off` removed the managed stream from
   `status-json`; no matching loudnessd filter remained in the graph.
3. Restoring profile `HiFi` recreated the physical source and produced a
   healthy capture route for stream node 123 without restarting loudnessd.
4. The daemon journal reported `route endpoint disappeared; reconnecting`,
   followed by `recovered broken route for stream 123`.
5. Cleanup removed the recorder and its managed route. The default source name
   returned to Scarlett Mic 1, the source volume returned to 50%, and the card
   profile remained at index 1.

No audio payload or temporary test artifact was retained. This check proves
recovery from a hardware-backed source disappearing at the PipeWire graph
boundary. A physical USB disconnect is still required to cover kernel device
removal, enumeration, and card identity changes.
