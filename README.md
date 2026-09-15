# loudnessd

`loudnessd` is an experimental per-application perceived-loudness normalizer
for PipeWire, written in Rust and packaged as a standalone Nix flake. The
daemon inserts transient inline filters for application playback and capture
streams while leaving the system master volume independent.

- [User documentation](docs/user/README.md)
- [Developer documentation](docs/dev/README.md)
