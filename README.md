# loudnessd

`loudnessd` is an experimental per-application perceived-loudness normalizer
for PipeWire. It is currently developed inside this flake, but the Rust crate,
Nix package, example configuration, and documentation are self-contained so
they can move to a dedicated repository later.

The current milestone is read-only and dry-run only. It can inspect PipeWire
streams and exercise the controller, but it does not modify the audio graph.

- [User documentation](docs/user/README.md)
- [Developer documentation](docs/dev/README.md)

