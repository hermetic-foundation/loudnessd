# PipeWire restart qualification

Candidate `ac4c345d44ee` was tested against private PipeWire servers on
2026-09-17. The test graph and control socket used an isolated runtime and did
not connect to the desktop audio session.

## Disconnect detection

Terminating the private PipeWire server caused loudnessd to exit with status 1
and report the core `EPIPE` event:

```text
PipeWire connection failed: connection error (object 0, sequence 40, result -32)
```

The listener ignores recoverable object errors. A 20-second lifecycle run
subsequently exercised sink replacement, including a transient link-factory
`EEXIST`, without terminating the daemon. It retained one healthy route in all
20 observations with zero IPC failures, skipped streams, callback stalls,
route failures, or resident-memory growth.

## Service recovery

The committed package was launched as a transient user service with
`Restart=on-failure`. After replacing its private PipeWire server, systemd
recorded two restart attempts while the socket was unavailable. Loudnessd then
returned under a new PID and served a valid `status-json` response against the
replacement server.

Recovery journals now include the PipeWire server cookie. A journal from a
different server identity is deleted instead of replayed against recyclable
global object IDs. Unit coverage verifies that mismatch behavior, while the
lifecycle harness continues to verify same-server recovery after forced daemon
termination.

This qualifies daemon and systemd recovery after a PipeWire server restart. It
does not claim application-stream continuity across that restart: PipeWire
clients must reconnect after the server replaces their graph objects.

## Verification

The exact candidate passed 106 library tests, 4 CLI tests, strict Clippy, the
release package build, ShellCheck, and NixOS module evaluation through
`nix flake check`. Build concurrency and aggregate compiler CPU were limited to
two cores.
