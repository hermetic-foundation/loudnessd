# Sample-rate matrix

Candidate `286dde87670b` was exercised on 2026-09-17 with private PipeWire
graphs pinned to each tested rate. `pw-top` confirmed that the sink and fixture
clients used the requested graph clock instead of being resampled through a
48 kHz graph.

Each run used two simultaneous stereo routes, replaced the private sink while
the clients remained active, paused and resumed one client, and required the
route identities to remain healthy and unchanged for two seconds before
profiling. Every run reported zero IPC failures, daemon restarts, active-stream
shortfalls, skipped streams, unhealthy routes, callback stalls, and PipeWire
errors.

## Playback

| Graph rate | Samples | Average CPU, one core | RSS growth |
| ---: | ---: | ---: | ---: |
| 22,050 Hz | 6 | 2.50% | 0 B |
| 32,000 Hz | 6 | 3.83% | 0 B |
| 44,100 Hz | 6 | 4.83% | 0 B |
| 48,000 Hz | 6 | 5.50% | 0 B |
| 88,200 Hz | 8 | 10.37% | 0 B |
| 96,000 Hz | 8 | 11.12% | 0 B |
| 192,000 Hz | 8 | 21.00% | 8 KiB |

The first 88.2 kHz attempt began profiling after one healthy route observation
and recorded graph errors during a delayed policy relink. DSP busy time remained
well below its deadline. Requiring two seconds of stable route identity before
profiling removed that measurement race; the repeated run and all subsequent
rates completed without errors.

## Duplex capture isolation

The capture runs used one application identity with independent playback and
capture streams and controllers.

| Graph rate | Samples | Average CPU, one core | RSS growth |
| ---: | ---: | ---: | ---: |
| 44,100 Hz | 8 | 5.00% | 0 B |
| 48,000 Hz | 8 | 5.75% | 0 B |
| 96,000 Hz | 8 | 9.00% | 0 B |

## Mono routes

At 48 kHz, the same fault and invariant sequence passed with mono graph nodes.
The playback run averaged 2.87% of one core and the duplex playback/capture run
averaged 3.62%. Both retained two active independent application streams with
zero shortfalls, skipped streams, unhealthy routes, callback stalls, PipeWire
errors, or resident-memory growth.

## Hardware scope

Read-only format inspection found that the default USB output advertises a
44.1-192 kHz range and the Scarlett capture driver advertises a 44.1-96 kHz
range. These tests did not change the active desktop graph or hardware clock.
They qualify the private stereo DSP paths at those rates, not physical-device
continuity.

Physical-device tests at every advertised rate remain a release requirement.
Those tests must be scheduled when changing the desktop graph cannot interrupt
an active audio session.
