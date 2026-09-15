# loudnessd

`loudnessd` is an experimental per-application perceived-loudness normalizer
for PipeWire, written in Rust and packaged as a standalone Nix flake.

The current daemon milestone is read-only. It observes PipeWire stream events
and exercises the controller, but does not modify the audio graph yet.

- [User documentation](docs/user/README.md)
- [Developer documentation](docs/dev/README.md)
