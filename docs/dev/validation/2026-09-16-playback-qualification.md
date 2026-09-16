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

## Failed extended soak

An intended eight-hour run started at 2026-09-16 10:39:26 CDT with candidate
`/nix/store/0998kh4y7777zawdzrvmsdlsjsva19ca-loudnessd-0.1.0`. It was stopped
after 3 hours 44 minutes because it began affecting real Skyrim and ChatGPT
voice streams on the host. This is a failed release run, not partial evidence
that can be promoted into a pass.

| Measurement | Result at stop |
| --- | ---: |
| Monitor samples | 13,391 |
| Daemon restarts | 0 |
| Unhealthy route observations | 1 |
| Resident-memory range | 6.15-10.57 MiB |
| Average process CPU | 3.65% of one core |
| Continuous settled samples within 1.5 LU | 9,707 / 9,708 |
| Intermittent settled samples within 1.5 LU | 8,693 / 9,258 |
| Maximum intermittent output true peak | +0.767 dBTP |
| Intermittent samples above the -1 dBTP ceiling | 13,143 / 13,390 |
| Reported intermittent limiter reduction | 0.0 dB |

The run failed two independent release gates. First, its sample-peak limiter
allowed sustained inter-sample overs. The post-filter BS.1770 meter proves the
configured true-peak ceiling was not enforced. Second, after Skyrim started,
the daemon twice timed out restoring broken routes and attempted several
duplicate stream insertions. Both generator streams accumulated hundreds of
PipeWire errors (more than 600 each when observed live), and a simultaneous
ChatGPT voice call developed audible intermittent crackling.

Stopping the transient unit ran the harness cleanup. Its trap restarted the
ordinary service, which was then explicitly disabled through `loudnessd msg
disable` so its ordered bypass restored direct routes. A post-cleanup graph
inspection found no loudnessd or soak nodes, and a fresh `pw-top` sample showed
zero errors for Skyrim and both ChatGPT streams. PipeWire and the graphical
session were not restarted.

The true-peak implementation was replaced after this run, but it still needs a
fresh full-duration qualification. The next extended harness must run in an
isolated PipeWire instance or otherwise exclude unrelated host streams; it may
not attach an experimental candidate to interactive game, voice, or desktop
audio. It must also persist generator error counters so continuity failures
remain auditable after cleanup.
