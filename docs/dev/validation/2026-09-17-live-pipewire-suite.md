# Live PipeWire suite: 2026-09-17

## Result

Pass. Every ignored Rust test that requires a live PipeWire user session passed
serially on candidate `ab5559398faa828e59a48f96f39b7eef3688ba1e`.

## Environment

- Host: `midi-desktop-1`, x86_64 Linux 6.18.51
- PipeWire client library: 1.6.8
- Test scheduling: serial, with Cargo limited to two build jobs
- Graph scope: the active user session, using disposable test nodes and links

## Command

```console
nix develop -c cargo test -j 2 -- --ignored --test-threads=1
```

## Coverage

All six live tests passed:

- registry snapshots exposed links with resolvable ports;
- connected inactive filters registered their nodes;
- unconnected filters owned ports without registering nodes;
- client-owned and lingering links followed their documented lifetimes; and
- route installation and ordered bypass used only disposable nodes and restored
  the original direct route; and
- route installation cleaned up its staged filter and links when the source
  application exited immediately before cutover.

The run completed with no test failure and did not disturb existing desktop
audio streams. These tests cover live API and transaction mechanics, but do not
replace the extended private-graph soak or hardware capture qualification.
