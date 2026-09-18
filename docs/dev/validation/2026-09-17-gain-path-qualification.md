# Playback gain-path qualification: 2026-09-17

## Result

Pass for candidate `846c91481e4d`. A two-minute private-graph playback run
exercised both positive and negative normalization gain with production-like
PipeWire scheduling. The run used the release binary built from the candidate
source, a `-16 LUFS` playback target, two isolated stereo fixtures, and the
strict soak assertions.

This bounded qualification establishes that the revised controller status and
gain-path fixtures work together. It does not replace the required exact-candidate
30-minute soak.

## Measurements

- 120 status samples over 120 seconds
- 0 IPC failures and 0 daemon restarts
- 2 active streams in every observation
- 0 skipped-stream, unhealthy-route, stalled-callback, or active-shortfall
  observations
- 60 of 60 convergence-eligible observations within `1.5 LU`
- gain range from `-1.5000026 dB` to `+18.0 dB`
- 167 boost observations and 24 cut observations
- maximum limiter reduction of `4.2410374 dB`
- maximum post-filter true peak of `-1.0989114 dBTP`
- resident memory fixed at `8,626,176` bytes, with 0-byte growth
- average daemon CPU use of `3.524997%` of one core
- 0 PipeWire profiler errors under the harness's mandatory zero-error check

The fixtures contained sustained quiet, moderate, loud, and silent intervals.
Only non-silent healthy active observations contributed to the gain-direction
counts. This prevents silence-gated periods from satisfying boost or cut
coverage accidentally.

## Controller evidence

The monitor considered an unclamped stream converged only after its measured
post-filter loudness entered the target deadband. Gain-command completion alone
could not satisfy the convergence gate. Streams whose required gain exceeded a
configured limit remained separately identifiable through `gain_clamped`.

The run retained normal PipeWire scheduling. Earlier diagnostic runs using a
debug binary or an explicit real-time disable produced either excessive CPU use
or profiler errors and are not release evidence.

## Artifacts

The local validation artifacts were written with the prefix
`/tmp/loudnessd-gain-path-final-20260917-1512`. They contain NDJSON status,
the final summary, service logs, and the continuous `pw-top` timeline. They do
not contain audio samples.
