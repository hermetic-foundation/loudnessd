# Application exit during route transitions: 2026-09-17

## Result

Pass for both route-transition boundaries. A live transaction test removed a
disposable source immediately after replacement links were staged during
installation. The private lifecycle harness separately terminated a second
application while `loudnessd msg disable` was performing ordered bypass for it
and a surviving stereo stream.

## Environment

- Host: `midi-desktop-1`, x86_64 Linux 6.18.51
- PipeWire client and private server: 1.6.8
- Candidate package:
  `/nix/store/1rrb6gx7nyyg3wjvqdwniw4l584abxai-loudnessd-0.1.0`
- Harness commit: `74233094d656`
- Install-boundary test commit: `d6347f1efbdc`
- Graph: disposable private 48 kHz stereo PipeWire server and WirePlumber
- CPU limit: two cores through a systemd user-service quota

## Installation boundary

The live Rust test created disposable source, normalizer, and destination
nodes, confirmed the original direct link, and then installed through a backend
hook. The hook dropped the source owner immediately after both replacement
links were confirmed but before cutover. A rejected transaction was permitted
to fail only while removing the original or activating the filter; if the
server accepted the now-stale transaction, the test explicitly released it.

In either outcome, the source disappeared and every staged replacement link
was absent before the test returned. The test ran against the desktop PipeWire
server but did not discover or modify existing application routes.

## Bypass boundary

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
