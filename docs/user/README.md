# User guide

## Current status

`loudnessd` is experimental. The daemon normalizes each application stream
independently through transient PipeWire filters. Playback and capture have
separate policy and controller state. A final linked-channel peak guard prevents
normalization gain from clipping without replacing perceived-loudness control.

## Installation

Run the package directly:

```console
nix run github:hermetic-foundation/loudnessd -- --list-streams
```

### Portable systemd user service

The repository includes `systemd/loudnessd.service` for non-NixOS systems.
Distribution packages should install it to `lib/systemd/user/loudnessd.service`
under their package prefix. For a source or Cargo installation, install it for
the current user:

```console
unit=$HOME/.config/systemd/user/loudnessd.service
install -Dm644 systemd/loudnessd.service "$unit"
sed -i "s|/usr/bin/loudnessd|$(command -v loudnessd)|" "$unit"
systemctl --user daemon-reload
systemctl --user enable --now loudnessd.service
```

Distribution packages that install the executable at `/usr/bin/loudnessd` can
ship the unit unchanged. Installations using another prefix must replace that
path during packaging, as the Nix derivation does.

The portable unit runs `loudnessd --daemon` without an explicit config path.
On first start, loudnessd creates the generic configuration at
`$XDG_CONFIG_HOME/loudnessd/config.toml`, or
`$HOME/.config/loudnessd/config.toml` when `XDG_CONFIG_HOME` is unset. It never
overwrites an existing configuration.

The Nix package installs this portable unit under
`$out/lib/systemd/user/loudnessd.service`. NixOS users should use the module
below instead; its generated service supplies the immutable configuration path
directly.

### NixOS module

To install it through NixOS, add the flake input and module:

```nix
{
  inputs.loudnessd = {
    url = "github:hermetic-foundation/loudnessd";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { nixpkgs, loudnessd, ... }: {
    nixosConfigurations.example = nixpkgs.lib.nixosSystem {
      modules = [
        loudnessd.nixosModules.default
        {
          services.loudnessd = {
            enable = true;
            settings.defaults = {
              playback = true;
              capture = true;
            };
          };
        }
      ];
    };
  };
}
```

The module generates an immutable TOML configuration in the Nix store and
starts `loudnessd` as a systemd user service after PipeWire. It does not write
configuration under the user's home directory.

An existing TOML file can be used instead of generated settings:

```nix
services.loudnessd = {
  enable = true;
  configFile = ./loudnessd.toml;
};
```

When `configFile` is set, it is authoritative and `settings` are not used to
generate the service configuration.

Inspect the service with:

```console
systemctl --user status loudnessd
journalctl --user -u loudnessd
```

The daemon owns transient processing nodes and replacement links. A clean stop
restores ordinary direct routes before removing its filters. Restored direct
routes intentionally remain after the daemon disconnects. If a device move
supersedes a route, the daemon releases the old filter and reconnects to the
new route.

Before cutover, loudnessd writes the original direct endpoints to a private
`$XDG_RUNTIME_DIR/loudnessd-routes.toml` journal. After an unclean exit, the
next service start restores missing direct links before normalization resumes.
The journal is removed after a clean bypass. It contains PipeWire object IDs,
not audio or application data.

## Runtime control

The service exposes a private per-user socket under `$XDG_RUNTIME_DIR`. Inspect
or temporarily change the running policy with:

```console
loudnessd msg status
loudnessd msg status-json
loudnessd msg disable
loudnessd msg enable
loudnessd msg reload
loudnessd msg set application-a playback off
loudnessd msg set application-a capture on
loudnessd msg reset application-a
```

`status-json` reports the same daemon, route, controller, source, output, peak,
gain, and limiter state as structured JSON for soak tests and monitoring tools.
It also includes the meter sequence, configured target, and gain-limit state so
monitors can distinguish silence from a stalled audio callback and exclude
target-unreachable streams from convergence scoring, including while their gain
is still slewing toward a configured limit. Streams deliberately left on their
original direct route appear under `skipped_streams` with their node ID,
direction, application identity, and specific reason. The text status prints
the same records as `skipped_stream=...` lines.

Run a bounded soak monitor and optionally retain its raw NDJSON observations:

```console
loudnessd monitor --duration 3600 --interval 1000 --expect-active 2 \
  --output loudnessd-soak.ndjson
```

The final JSON summary reports IPC failures, unhealthy routes, stalled callback
sequences, daemon restarts, active-stream shortfalls, in-progress slew
observations, skipped-stream observations, convergence within 1.5 LU after the
controller settles, and maximum limiter reduction. It also records expected,
minimum, and maximum active stream counts, the maximum simultaneous skipped
stream count, initial, final, and peak resident memory, memory growth, and
average daemon CPU use. CPU accounting remains valid across a daemon restart
because status includes both the process ID and Linux process start time. The
NDJSON contains status measurements and metadata only; it never contains audio
samples.

`disable` bypasses every active stream but leaves the daemon available.
`reload` rereads the original `--config` path. `set` and `reset` are in-memory
overlays and disappear when the daemon restarts.

Export the baseline plus runtime overlays as deterministic TOML:

```console
loudnessd msg export > loudnessd.toml
```

Export never modifies the active baseline. Persist the result explicitly as a
standalone config file or translate it into NixOS module settings.

## Inspecting streams

List application playback and capture streams without changing the PipeWire
graph:

```console
loudnessd --list-streams
```

The output includes direction, PipeWire node ID, application identity, process
name, and media name when PipeWire provides them.

## Dry-run controller

The development protocol accepts one observation per line:

```text
DOMAIN APPLICATION_ID STREAM_ID LUFS ELAPSED_MILLISECONDS
```

For example:

```console
printf 'playback application-a stream-1 -24 1000\ncapture application-b stream-2 -24 1000\n' | loudnessd
```

## Configuration

Pass a TOML policy with `--config PATH`. Playback and capture can be controlled
independently at both the default and application level:

```toml
[defaults]
playback = true
capture = true

[applications."application-a"]
capture = false

[applications."application-b"]
playback = false
capture = true
```

An omitted direction inherits from `[defaults]`. Matching prefers PipeWire's
`application.id`, then process binary, then application name. Streams without
those properties receive a node-scoped fallback identity.

For a standalone installation, starting `loudnessd --daemon` without
`--config` uses `$XDG_CONFIG_HOME/loudnessd/config.toml`, falling back to
`$HOME/.config/loudnessd/config.toml`. The daemon creates a generic starter
configuration there when the file is absent and never overwrites an existing
file. Stream listing and dry-run modes do not create configuration files.

Supplying `--config PATH` makes that file authoritative and read-only from
loudnessd's perspective. This is how the NixOS module passes either its
generated Nix-store configuration or `services.loudnessd.configFile`.

## Limitations

- Only application streams with unambiguous, channel-labelled routes are
  normalized; unsupported topology is left untouched and its reason remains
  visible through `loudnessd msg status` and `status-json` while the stream
  exists.
- Application metadata varies between native, Wine, and Proton software, so
  inspect `--list-streams` before relying on a per-application override.
- Native Chromium and PipeWire playback and capture clients have been validated
  on a multi-monitor NixOS desktop. Skyrim playback under Wine has also been
  validated using the `TESV: Skyrim` identity reported by `--list-streams`.
  Wine and Proton capture streams still require live validation before they
  should be treated as a stable matching contract.
- The current release supports PipeWire's negotiated planar floating-point DSP
  buffers. The filter follows the graph sample rate at runtime.
