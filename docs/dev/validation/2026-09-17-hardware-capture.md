# Non-silent hardware capture: 2026-09-17

## Result

Pass. A hardware-backed microphone stream crossed the capture silence gate,
settled near the configured `-18 LUFS` target, preserved its source control,
and remained continuous without clipping, limiter engagement, or route failure.

## Environment

- Candidate package:
  `/nix/store/yzv8lmfhc7mdph5124g8zhqqcb50fjm6-loudnessd-0.1.0`
- Host: `midi-desktop-1`, x86_64 Linux 6.18.51
- PipeWire: 1.6.8
- Source: Focusrite Scarlett Solo Mic 1
- Format: mono float32 at 48 kHz
- Capture target: `-18 LUFS`
- Client: `pw-record`, with audio discarded to `/dev/null`
- Observation interval: 500 ms for 120 seconds

The normal 50% source setting attenuates the quiet room signal below the
configured `-55 LUFS` capture gate. For this qualification only, the source
preamp was set to 300%. This calibrated the real hardware signal into the
normalizer's active range; no synthetic samples were injected. A cleanup trap
restored the exact 50% source setting on every exit path.

## Observations

- All 240 observations reported a healthy capture route.
- One initial observation was waiting for a meter reading, 18 were converging,
  and 221 were settled.
- The stream carried non-silent hardware input in 239 observations.
- Source loudness reached `-24.10 LUFS`; output loudness reached
  `-18.46 LUFS`.
- Gain rose from unity to `+5.65 dB` without reaching either gain clamp.
- 220 of 221 settled, unclamped, un-limited observations were within `1.5 LU`
  of target, a 99.55% convergence ratio.
- Maximum output true peak was `-2.73 dBTP`; the limiter never engaged.
- The real-time meter sequence advanced to 6,725 without a stalled callback.
- No audio payload was retained.

After the client exited, loudnessd removed the managed stream and filter. The
source returned to 50% volume, and no process, graph object, or temporary test
artifact remained. The temporary preamp is part of the test fixture, not a
recommended user setting or a change to loudnessd's target.
