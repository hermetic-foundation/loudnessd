# User guide

## Current status

`loudnessd` is experimental. The current binary does not process audio or run
as a service. It provides a read-only PipeWire stream listing and a dry-run
controller interface for development and testing.

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
        { programs.loudnessd.enable = true; }
      ];
    };
  };
}
```

The module currently installs the dry-run tool only. It will gain daemon
options when the native PipeWire processing backend is ready for user testing.

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
