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

## Isolated endpoint-recovery qualification

Candidate `7b9934e05885bfa9220d52057f839b4876eaa063` passed a separate
15-second bounded run in the fully private PipeWire graph. Both fixtures were
selected by their stable `application.id` values rather than display names.
The harness replaced the sink with a new PipeWire object while both streams
remained alive; both routes detected the endpoint loss, released stale graph
ownership, and returned to healthy normalization without a daemon restart.

| Measurement | Result |
| --- | ---: |
| Monitor samples | 15 |
| Active streams in every sample | 2 |
| IPC failures | 0 |
| Daemon restarts | 0 |
| Skipped-stream observations | 0 |
| Unhealthy-route observations | 0 |
| Stalled-callback observations | 0 |
| Fixture and filter PipeWire errors | 0 |
| Private PipeWire and WirePlumber errors | 0 |
| Initial resident memory | 8.10 MiB |
| Final resident memory | 8.11 MiB |
| Resident-memory growth | 4 KiB |
| Average process CPU | 3.53% of one core |

That run observed zero absolute errors. The current harness starts one
continuous `pw-top` profiler after endpoint recovery and pause/resume fault
injection, then retains its complete steady-state timeline. This avoids the
per-process baseline reset that made independently launched snapshots
unsuitable for cumulative comparison.

The short run ended while both deliberately quiet fixtures were still slewing
toward the `-13 LUFS` target, so it supplies recovery and resource evidence but
no convergence-ratio evidence. It does not replace the eight-hour release soak.
The same graph with an unoptimized debug binary consumed most of one core and
tripped the kernel realtime watchdog; the harness now disables realtime only
inside its private runtime, and release qualification always uses the optimized
package artifact.

## Isolated pause/resume qualification

Candidate `872f506a632e` passed a 15-second private-graph run after the harness
sent PipeWire `Pause` and `Start` commands to the intermittent fixture's same
application node. The test waited for a post-filter meter reading before the
pause, then required that node's meter sequence to advance, post-filter LUFS to
return, and its route to remain healthy after restart. The application process
and node identity were preserved.

| Measurement | Result |
| --- | ---: |
| Active streams in every monitor sample | 2 |
| IPC failures | 0 |
| Daemon restarts | 0 |
| Skipped-stream observations | 0 |
| Unhealthy-route observations | 0 |
| Stalled-callback observations | 0 |
| Fixture and filter error-counter growth | 0 |
| Resident-memory growth | 4 KiB |
| Average process CPU | 3.47% of one core |

This proves the normalized processing path resumes after an explicit PipeWire
node pause. A real browser pause/resume integration run remains required because
browser session managers may apply additional node properties or graph policy.

## Continuous profiler qualification

Candidate `193320c8f12b` passed a 120-second private-graph run with the profiler
active for the full monitoring interval. The run included sink replacement and
same-node pause/resume before steady monitoring.

| Measurement | Result |
| --- | ---: |
| Monitor samples | 120 |
| Settled convergence ratio | 96.48% |
| IPC failures | 0 |
| Daemon restarts | 0 |
| Active-stream shortfalls | 0 |
| Skipped-stream observations | 0 |
| Unhealthy-route observations | 0 |
| Stalled-callback observations | 0 |
| Maximum fixture/filter PipeWire errors | 0 |
| Resident-memory growth | 0 bytes |
| Average process CPU | 4.16% of one core |

An earlier pair of independent final snapshots appeared to show filter errors,
but `pw-top` resets displayed counters for each profiler process. The continuous
timeline is the authoritative evidence and showed no xrun. This bounded run
still does not replace the required eight-hour soak.

## Application-control invariant qualification

Candidate `f8d62f5029cab04a9458da8fe2adf99d2f00698a` passed separate
15-second playback and duplex-capture runs with distinct non-default fixture
volumes. Both runs preserved the complete application `Props` state and the
post-recovery target exactly across sink replacement, pause/resume, and active
normalization. The compared state includes scalar volume, mute, per-channel
volume, soft volume, and monitor controls. Both runs also reported zero
PipeWire errors, route failures, callback stalls, IPC failures, and memory
growth. This proves the assertion and bounded behavior; the same assertion must
remain enabled for the required eight-hour run.

## Remaining release evidence

- Complete the eight-hour varied-content soak and measure memory growth from
  hour one through hour eight.
- Run the eight-stream memory case.
- Retain the now-automated application control and target invariant throughout
  the full eight-hour soak.
- Exercise limiter-bound material and verify the configured peak ceiling.
- Exercise a real browser pause and resume. The synthetic node-command case now
  passes, but feeding zero-valued samples is not equivalent and browser graph
  policy still needs direct integration evidence.
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
