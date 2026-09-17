# Browser pause and resume qualification

Candidate `962a9ce622fe` was tested on 2026-09-17 with Chromium 152 and the
desktop PipeWire graph. Chromium used a fresh temporary profile and a
disposable null sink, so the test produced no audible output and could not
change another application's route or volume.

The page created a real Web Audio oscillator, suspended its `AudioContext`
after eight seconds, and resumed it after eighteen seconds. Chromium remained
alive as process `463004` throughout the sequence.

- Playback initially appeared as Chromium node 143 through loudnessd filter
  node 112. The route was healthy and produced non-silent source and output
  loudness readings.
- During suspension, Chromium either retained a silent stream or temporarily
  removed its audio stream. Every remaining Chromium stream reported silence
  below the configured gate.
- Playback resumed as Chromium node 125 through filter node 131. The new route
  was healthy and immediately produced non-silent source and output readings.
- The browser process was not recreated. Replacing the PipeWire stream node is
  normal Chromium behavior when an audio context resumes.

The isolated Chromium process, temporary profile, null sink, and candidate
daemon were removed afterward. No test sink remained in the live graph, and
the configured loudnessd user service was restored and responsive.

This satisfies the real-browser pause/resume gate. It does not substitute for
the separate browser WebRTC capture requirement.
