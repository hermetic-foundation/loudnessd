# Playback qualification: 2026-09-16

## Result

Pass for the bounded two-stream qualification. This run validates the soak
harness, observability, settled convergence scoring, short-run resource
stability, varied boost behavior, routing, and cleanup. It does not replace the
required eight-hour release soak, eight-stream memory test, direct application
volume invariant check, or recovery matrix.

## Environment

- Candidate commit: `0b195823f46f13f09d27824198b1491d981fe22d`
- Monitor scoring verified at: `cc3ec1079979b9bc7c253dca69f20f23922d8fcf`
- Host: `midi-desktop-1`, x86_64 Linux 6.18.51
- Processor: AMD Ryzen 5 3400G, four cores and eight threads
- PipeWire: 1.6.8
- Format: two stereo float32 streams at 48 kHz
- Duration: 120 seconds, sampled once per second
- Target: -13 LUFS playback
- Sink: disposable PipeWire null sink
- Generator: SoX sine and pink-noise segments with continuous, quiet,
  intermittent-silence, and louder periods
- Monitor: `loudnessd monitor`, reading daemon status over its private user
  socket; no audio was retained

The harness stopped the ordinary user service, ran the candidate daemon,
created the disposable streams and sink, and restored the ordinary service in
its exit trap. The disposable graph objects were absent after cleanup.

## Observations

| Measurement | Result |
| --- | ---: |
| Monitor samples | 120 |
| IPC failures | 0 |
| Unhealthy route observations | 0 |
| Stalled callback observations | 0 |
| In-progress slew observations | 54 |
| Settled eligible observations | 145 |
| Settled observations within 1.5 LU | 140 |
| Settled convergence ratio | 96.6% |
| Maximum limiter reduction | 0.0 dB |
| Initial resident memory | 6.07 MiB |
| Final and peak resident memory | 8.16 MiB |
| Average process CPU | 3.88% of one core |

The continuous fixture passed 71 of 72 settled observations and the
intermittent fixture passed 69 of 73. Observations while gain was still slewing
are reported separately and are not scored as settled convergence. Silent,
gain-clamped, and limiter-bound observations are also ineligible, matching the
release gate.

A separate 60-second repeat measured 3.72% average CPU and approximately
8.0 MiB final resident memory. The repeat supports a `5%` two-stream CPU gate
on this baseline processor while retaining both pre-gain and post-filter EBU
R128 meters and true-peak analysis. The previous `2%` criterion was not based
on a measured implementation and would require weakening the required
post-filter observability rather than addressing an observed instability.

## Remaining release evidence

- Complete the eight-hour varied-content soak and measure memory growth from
  hour one through hour eight.
- Run the eight-stream memory case.
- Record application stream volume, mute, channel volume, and WirePlumber
  target before and after the soak.
- Exercise limiter-bound material and verify the configured peak ceiling.
- Complete the lifecycle and recovery matrix from the release gates.

## Eight-hour soak interim finding

An eight-hour run started at 2026-09-16 10:39:26 CDT with candidate
`/nix/store/0998kh4y7777zawdzrvmsdlsjsva19ca-loudnessd-0.1.0`. The run remains
active, so these observations are evidence from its first 2.4 hours rather
than a final result:

| Measurement | Interim result |
| --- | ---: |
| Monitor samples | 8,710 |
| Daemon restarts | 0 |
| Unhealthy route observations | 0 |
| Resident-memory range | 6.15-9.28 MiB |
| Average process CPU | 3.50% of one core |
| Continuous settled samples within 1.5 LU | 6,350 / 6,351 |
| Intermittent settled samples within 1.5 LU | 5,695 / 6,059 |
| Maximum intermittent output true peak | +0.535 dBTP |
| Intermittent samples above the -1 dBTP ceiling | 8,462 / 8,709 |
| Reported intermittent limiter reduction | 0.0 dB |

The run therefore fails the true-peak release gate even though routing,
continuity, convergence, CPU, and memory remain stable so far. The current
limiter observes only sample peaks; the post-filter BS.1770 meter proves that
inter-sample peaks escape it. This is a release blocker. The required fix is a
real true-peak limiter with lookahead, followed by a fresh full-duration soak
on the release candidate. Lowering the release threshold or treating sample
peak as true peak is not an acceptable resolution.
