{ self }:

{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.loudnessd;
  system = pkgs.stdenv.hostPlatform.system;
  toml = pkgs.formats.toml { };
  applicationSettings = lib.mapAttrs (
    _: settings: lib.filterAttrs (_: value: value != null) settings
  ) cfg.settings.applications;
  configFile = toml.generate "loudnessd.toml" {
    defaults = cfg.settings.defaults;
    applications = applicationSettings;
  };
  effectiveConfigFile = if cfg.configFile == null then configFile else cfg.configFile;
in
{
  options.services.loudnessd = {
    enable = lib.mkEnableOption "loudnessd PipeWire loudness normalization service";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${system}.default;
      defaultText = lib.literalExpression "inputs.loudnessd.packages.${pkgs.system}.default";
      description = "The loudnessd package to install.";
    };

    configFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      example = lib.literalExpression "./loudnessd.toml";
      description = ''
        Existing TOML configuration to pass to loudnessd. When set, this file is
        authoritative and settings are not used to generate the service configuration.
      '';
    };

    settings = {
      defaults = {
        playback = lib.mkOption {
          type = lib.types.bool;
          default = true;
          description = "Whether playback normalization is enabled by default.";
        };

        capture = lib.mkOption {
          type = lib.types.bool;
          default = true;
          description = "Whether capture normalization is enabled by default.";
        };
      };

      applications = lib.mkOption {
        type = lib.types.attrsOf (
          lib.types.submodule {
            options = {
              playback = lib.mkOption {
                type = lib.types.nullOr lib.types.bool;
                default = null;
                description = "Override playback normalization for this application.";
              };

              capture = lib.mkOption {
                type = lib.types.nullOr lib.types.bool;
                default = null;
                description = "Override capture normalization for this application.";
              };
            };
          }
        );
        default = { };
        example = {
          browser.capture = false;
          recorder.playback = false;
        };
        description = "Per-application overrides keyed by PipeWire application ID.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    systemd.user.services.loudnessd = {
      description = "Per-application PipeWire loudness normalization";
      documentation = [ "https://github.com/hermetic-foundation/loudnessd" ];
      after = [ "pipewire.service" ];
      wants = [ "pipewire.service" ];
      partOf = [
        "graphical-session.target"
        "pipewire.service"
      ];
      wantedBy = [ "graphical-session.target" ];
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} --daemon --config ${effectiveConfigFile}";
        Restart = "on-failure";
        RestartSec = 2;
      };
    };
  };
}
