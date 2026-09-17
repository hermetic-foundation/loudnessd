# Hardware-backed source reconnect: 2026-09-17

## Result

Pass for active source removal and recreation through both the ALSA card
profile and the kernel USB driver. In each case, the capture route failed open
when its source disappeared and became healthy again after the source returned.

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
boundary.

## USB driver removal and re-enumeration

The same test client subsequently ran while device `3-1.4.1` was unbound from
and rebound to the kernel USB driver. The active Feixiang playback device is on
a different USB path and remained untouched.

1. Before removal, loudnessd held a healthy capture route for stream node 120.
2. Kernel unbind removed the USB device, its ALSA card, and its PipeWire source.
   The managed stream disappeared from `status-json`, and no matching filter
   remained in the graph.
3. Kernel bind re-enumerated the device. The restart-tolerant capture client
   retained stream node 120, and loudnessd recreated a healthy route without a
   daemon restart.
4. The journal recorded `route endpoint disappeared; reconnecting`, followed
   by `recovered broken route for stream 120`.
5. PipeWire assigned a new card and source object, proving recovery did not
   depend on stale numeric IDs. The stable default-source name, `HiFi` profile,
   and 50% source volume were restored.
6. Cleanup removed the client and managed route, leaving no stale node or
   temporary artifact.

The USB bind operation was protected by a cleanup trap that attempted rebind
on every exit path. This covers kernel removal, re-enumeration, PipeWire object
replacement, route cleanup, and route recovery without a physical cable event.
