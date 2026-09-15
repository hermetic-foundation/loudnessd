# User guide

## Current status

`loudnessd` is experimental. The current daemon observes PipeWire stream
lifecycle events but does not process or reroute audio yet. It also provides a
read-only stream listing and a dry-run controller interface for development and
testing.

## Installation

Run the package directly:

```console
nix run github:hermetic-foundation/loudnessd -- --list-streams
```

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

An omitted direction inherits from `[defaults]`. Application matching keys and
precedence remain provisional until broader PipeWire metadata testing is
complete.

For a standalone installation, starting `loudnessd --daemon` without
`--config` uses `$XDG_CONFIG_HOME/loudnessd/config.toml`, falling back to
`$HOME/.config/loudnessd/config.toml`. The daemon creates a generic starter
configuration there when the file is absent and never overwrites an existing
file. Stream listing and dry-run modes do not create configuration files.

Supplying `--config PATH` makes that file authoritative and read-only from
loudnessd's perspective. This is how the NixOS module passes either its
generated Nix-store configuration or `services.loudnessd.configFile`.
