# Loudness dynamics design

This document describes the three-layer implementation added on 2026-11-12.
It is not a record of deployment or a declaration that all
[release gates](release-gates.md) have passed. See the
[implementation validation record](validation/2026-11-12-loudness-dynamics.md)
for checks performed and remaining qualification work.

## Intent

Normalize the louder portions of an application stream without continually
pulling intentional quiet passages up to the target. Automatic amplification
is bounded assistance, not a promise that every source will reach the target.
Keep master volume downstream and independent.

Each media stream owns its own history and gain state. Playback and capture
never share those states, even when they belong to the same application.

The design separates three decisions:

| Layer | Measurement | Action | Timing |
| --- | --- | --- | --- |
| True-peak limiter | Oversampled post-gain waveform peaks | Prevent digital clipping | Audio path, fixed 10 ms lookahead |
| Fast cut | Current pre-gain 400 ms momentary loudness plus applied gain | Reduce immediately when above target and deadband; hold the cut's ceiling | Audio path, 1-second hold by default |
| History controller | P90 of recent pre-gain momentary loudness observations | Set the baseline gain; immediate cuts and slow upward recovery | 30-second history, 100 ms control cadence by default |

"Immediate" means no downward slew after a usable measurement requests a cut.
The fast cut still depends on the momentary meter's measurement window; it
does not measure instantaneous perceived loudness or predict incoming audio.
The peak limiter is separate from this measurement and control timing.

These digital controls do not measure sound pressure at the listener and do
not guarantee hearing-safe listening levels.

## Defaults

| Setting | Playback | Capture |
| --- | --- | --- |
| Loudness target | -16 LUFS | -18 LUFS |
| Silence gate | -50 LUFS | -55 LUFS |
| Deadband | 0.75 LU | 1 LU |
| Maximum automatic boost | +6 dB | +6 dB |
| Maximum cut magnitude | 24 dB | 18 dB |
| Maximum upward recovery | 0.2 dB/s | 0.2 dB/s |
| Main-controller downward slew | None | None |
| History duration | 30 seconds | 30 seconds |
| History percentile | 90 | 90 |
| Fast-cut hold | 1 second | 1 second |

These are configurable defaults, not hidden hard limits that override user
configuration. A +6 dB boost is a ceiling, not a gain that every stream must
eventually receive. If a source requires more amplification to reach the target,
it remains below target once the ceiling is reached.

## Shared history and percentile

The history stores loudness values and their observed durations, not raw audio.
The momentary readings are overlapping 400 ms measurements, normally sampled
by the main controller every 100 ms. This is one shared long-term history per
stream, not separate 30-second histories for upward and downward decisions.
The ordinary 3-second source/output meters remain available for reporting;
they do not provide a second reduction-control window.

Startup uses the valid observations available so far; there is no mandatory
30-second warmup. Reported short-term source/output LUFS fall back to the
momentary reading until their 3-second window is available.

P90 is the duration-weighted nearest-rank threshold. Sort the accepted readings
by LUFS and select the first value whose cumulative observed duration reaches
90% of accepted duration in the current window. It is not a waveform peak, a
mean of LUFS values, a mean of the upper decile, or 90% of digital full scale.

For example, nine seconds at -30 LUFS followed by one second at -4 LUFS still
has P90 of -30 LUFS. Another 100 ms at -4 LUFS moves that example's P90 to
-4 LUFS. The independent fast cut can already have acted before P90 changes.

Gated, non-finite, absent, and stale measurements age existing history without
adding a loudness value to the percentile distribution or authorizing a boost.
Bypassed and connecting streams also age. Ended streams lose both history and
gain state so recycled PipeWire node IDs do not inherit another stream's state.

The main cadence uses elapsed monotonic time, not the number of PipeWire loop
wakeups. After a delayed tick, unobserved time ages history; the newest snapshot
receives no more than 100 ms of observation weight. A 10-second stall must not
turn one short loud reading into ten seconds of loud history.

History duration is limited to 300 seconds and storage to 4,096 intervals.
Adjacent identical observations can share an interval. Percentile sorting and
history allocation happen on the control thread, not the real-time callback.

## Gain calculation

For an accepted history reference, the main controller computes:

```text
desired_gain_db = clamp(target_lufs - reference_lufs,
                        -maximum_cut_db, maximum_boost_db)
```

Within the configured deadband it holds gain. Otherwise, a required downward
change reaches the requested target without a ramp by default. An upward change
is bounded by `boost_rate_db_per_second * observed_seconds`. An explicitly
configured legacy `cut_rate_db_per_second` can still limit main-controller cuts;
omitting it selects the default immediate behavior.

The fast-cut envelope evaluates the current source reading with the gain
already applied. It can lower gain but never generates an upward request.
Recovery toward an existing main-controller request is separately rate-limited
in the audio path, so a delayed control loop cannot cause an upward jump.
Applied cuts affect every sample of the processed buffer rather than
interpolating downward across it. Upward buffer transitions remain interpolated.
The final linked-channel true-peak limiter is unchanged.

## Hold and handover

A new stronger fast cut establishes an absolute gain ceiling and starts a full
one-second hold. Equal or weaker readings do not continually restart the hold.
A stronger cut may replace the ceiling and restart the hold. The hold ages with
processed audio time. A sustained loud source also constrains recovery after
the timer expires, preventing a rebound into the same loud material.

The fast-cut envelope publishes actual gain and a wrapping event sequence.
`StreamControl` adopts a new event's actual gain into the main controller once.
The event remains observable after its hold expires, so a stalled main loop
can still inherit it. The main controller does not start an additional hold.
During an active hold, its higher requests cannot exceed the audio-path ceiling.

Configuration reloads preserve filters whose directional settings and enabled
policy did not change. In particular, changing one application's capture policy
does not rebuild its unchanged playback filter. When replacement is necessary,
the daemon checkpoints gain, remaining hold, and event sequence after
deactivation, then restores them before activating the replacement. Route
recovery uses the same checkpoint mechanism. Time spent between filters is
subtracted from the remaining hold; reconstruction does not start a new second.

These checkpoints are in memory, not persistent volume settings or edits to a
user config. A complete daemon restart starts new DSP state; the route journal
restores connectivity, not a persisted gain envelope.

## Worked recovery timelines

Upward recovery is continuous rather than a discrete jump every five seconds.
At the default 100 ms cadence, the maximum requested increase is 0.02 dB per
observation. The audio-path envelope enforces the same rate between updates.

| Starting gain | Destination | Minimum recovery once permitted |
| --- | --- | --- |
| 0 dB | +6 dB | 30 seconds |
| -6 dB | 0 dB | 30 seconds |
| -6 dB | +6 dB | 60 seconds |
| -12 dB | +6 dB | 90 seconds |

The one-second hold and any continuing history/source constraint can add time.
The deadband can stop recovery before an exact target is reached. There is no
fixed extra 30-second timer applied after every event.

Consider a populated 30-second history containing quiet audio followed by a
brief loud burst. The fast cut can attenuate the burst even if P90 remains
quiet. The cut's ceiling holds for one second, then gain can recover only
slowly. A longer loud passage can also raise P90, keeping the baseline low
after the passage ends. Whether that reference remains influential for all
30 seconds depends on its rank and duration; P90 is not a maximum-value hold.
A new stronger event can interrupt recovery and cut immediately again.

## Ownership and observability

| Component | Responsibility |
| --- | --- |
| `normalization/dsp/meter.rs` | Momentary and short-term source/output measurements |
| `normalization/history.rs` | Bounded duration-weighted percentile history |
| `normalization/control.rs` | Configuration, directional policy, baseline gain and history aging |
| `normalization/dsp/fast_cut.rs` | Actual audio-path gain, fast-cut hold and bounded recovery |
| `normalization/stream.rs` | Snapshot freshness, observation weight and cut adoption |
| `pipewire/filter/process.rs` | Applying DSP and publishing coherent measurements |
| `daemon/control_clock.rs` | Real elapsed control cadence |
| `daemon/dynamics.rs` | Reload comparison and replacement checkpoints |

Use `loudnessd msg status-json` to distinguish the following:

- `source_momentary_lufs`: current 400 ms pre-gain measurement;
- `reference_lufs`: history percentile used by the main controller;
- `requested_gain_db`: main-controller request;
- `gain_db`: actual gain applied by the audio-path envelope;
- `fast_cut_hold_seconds`: remaining fast-cut hold;
- `limiter_db`: current peak-limiter reduction, separate from normalization gain.

Raw 3-second `source_lufs` and `output_lufs` remain visible. Monitor convergence
uses the percentile reference plus actual gain when available, rather than
requiring every intentionally quiet passage to reach the target. This is a
controller metric, not proof that all post-limiter audio reaches target LUFS.

See the [user guide](../user/README.md#configuration) for TOML and NixOS
configuration, including legacy cut-rate migration.
