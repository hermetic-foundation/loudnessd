# Application exit during bypass: 2026-09-17

## Result

Pass for the bypass boundary. The private lifecycle harness terminated a
second application while `loudnessd msg disable` was performing ordered bypass
for it and a surviving stereo stream. The daemon restored the survivor's two
direct channels, removed every managed filter and the recovery journal, then
successfully re-enabled normalization for the survivor.

## Environment

- Host: `midi-desktop-1`, x86_64 Linux 6.18.51
- PipeWire client and private server: 1.6.8
- Candidate package:
  `/nix/store/1rrb6gx7nyyg3wjvqdwniw4l584abxai-loudnessd-0.1.0`
- Harness commit: `74233094d656`
- Graph: disposable private 48 kHz stereo PipeWire server and WirePlumber
- CPU limit: two cores through a systemd user-service quota

## Assertions

The test required all of the following before continuing:

- the exiting and surviving application routes were both active and healthy;
- the disable request returned `ok` while the exiting client terminated;
- managed, active, and skipped counts all returned to zero while disabled;
- the exiting application and every loudnessd filter disappeared;
- both direct channels of the surviving application were present;
- no recovery journal remained; and
- re-enabling produced one stable healthy normalized route.

The subsequent eight-second monitor interval retained that one route in every
sample with zero IPC failures, daemon restarts, skipped streams, unhealthy
routes, callback stalls, PipeWire errors, or resident-memory growth. Average
daemon CPU use was 2.37% of one core.

## Remaining boundary

This run does not qualify application exit during route installation. The
daemon creates and installs a ready filter within one event-loop tick, so its
`connecting` state is not observable through IPC. That boundary needs a live
transaction test with an injected endpoint removal between replacement-link
staging and cutover; timing a short-lived client is not accepted as proof.
