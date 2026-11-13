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
  controllerSettings =
    settings:
    lib.filterAttrs (_: value: value != null) {
      target_lufs = settings.targetLufs;
      silence_gate_lufs = settings.silenceGateLufs;
      deadband_lu = settings.deadbandLu;
      maximum_boost_db = settings.maximumBoostDb;
      maximum_cut_db = settings.maximumCutDb;
      boost_rate_db_per_second = settings.boostRateDbPerSecond;
      cut_rate_db_per_second = settings.cutRateDbPerSecond;
      history_window_seconds = settings.historyWindowSeconds;
      history_percentile = settings.historyPercentile;
      fast_cut_hold_seconds = settings.fastCutHoldSeconds;
    };
  controllerOptions = defaults: {
    targetLufs = lib.mkOption {
      type = lib.types.float;
      default = defaults.targetLufs;
      description = "Perceived-loudness target in LUFS.";
    };

    silenceGateLufs = lib.mkOption {
      type = lib.types.float;
      default = defaults.silenceGateLufs;
      description = "Signals below this loudness are not boosted.";
    };

    deadbandLu = lib.mkOption {
      type = lib.types.float;
      default = defaults.deadbandLu;
      description = "Allowed deviation from the target before gain changes.";
    };

    maximumBoostDb = lib.mkOption {
      type = lib.types.float;
      default = defaults.maximumBoostDb;
      description = "Maximum gain loudnessd may add.";
    };

    maximumCutDb = lib.mkOption {
      type = lib.types.float;
      default = defaults.maximumCutDb;
      description = "Maximum gain loudnessd may remove.";
    };

    boostRateDbPerSecond = lib.mkOption {
      type = lib.types.float;
      default = defaults.boostRateDbPerSecond;
      description = "Maximum upward gain slew rate.";
    };

    cutRateDbPerSecond = lib.mkOption {
      type = lib.types.nullOr lib.types.float;
      default = defaults.cutRateDbPerSecond;
      description = ''
        Optional legacy downward gain slew rate for the main controller. Null
        omits the TOML key and makes cuts immediate. An explicit positive rate
        does not slow the audio-path fast cut, which is always immediate.
      '';
    };

    historyWindowSeconds = lib.mkOption {
      type = lib.types.float;
      default = defaults.historyWindowSeconds;
      description = "Per-stream history duration in seconds, greater than zero and at most 300.";
    };

    historyPercentile = lib.mkOption {
      type = lib.types.float;
      default = defaults.historyPercentile;
      description = "Rank percentile of the momentary 400 ms loudness history, in (0, 100].";
    };

    fastCutHoldSeconds = lib.mkOption {
      type = lib.types.float;
      default = defaults.fastCutHoldSeconds;
      description = "Non-negative hold after an audio-path fast cut before upward gain recovery.";
    };
  };
  controllerAssertions = direction: settings: [
    {
      assertion = settings.silenceGateLufs < settings.targetLufs;
      message = "services.loudnessd.settings.${direction}.silenceGateLufs must be below targetLufs";
    }
    {
      assertion = settings.deadbandLu >= 0.0;
      message = "services.loudnessd.settings.${direction}.deadbandLu must be non-negative";
    }
    {
      assertion = settings.maximumBoostDb >= 0.0;
      message = "services.loudnessd.settings.${direction}.maximumBoostDb must be non-negative";
    }
    {
      assertion = settings.maximumCutDb >= 0.0;
      message = "services.loudnessd.settings.${direction}.maximumCutDb must be non-negative";
    }
    {
      assertion = settings.boostRateDbPerSecond > 0.0;
      message = "services.loudnessd.settings.${direction}.boostRateDbPerSecond must be positive";
    }
    {
      assertion = settings.cutRateDbPerSecond == null || settings.cutRateDbPerSecond > 0.0;
      message = "services.loudnessd.settings.${direction}.cutRateDbPerSecond must be null or positive";
    }
    {
      assertion = settings.historyWindowSeconds > 0.0 && settings.historyWindowSeconds <= 300.0;
      message = "services.loudnessd.settings.${direction}.historyWindowSeconds must be in (0, 300]";
    }
    {
      assertion = settings.historyPercentile > 0.0 && settings.historyPercentile <= 100.0;
      message = "services.loudnessd.settings.${direction}.historyPercentile must be in (0, 100]";
    }
    {
      assertion = settings.fastCutHoldSeconds >= 0.0;
      message = "services.loudnessd.settings.${direction}.fastCutHoldSeconds must be non-negative";
    }
  ];
  applicationSettings = lib.mapAttrs (
    _: settings: lib.filterAttrs (_: value: value != null) settings
  ) cfg.settings.applications;
  configFile = toml.generate "loudnessd.toml" {
    defaults = cfg.settings.defaults;
    playback = controllerSettings cfg.settings.playback;
    capture = controllerSettings cfg.settings.capture;
    applications = applicationSettings;
  };
  effectiveConfigFile = if cfg.configFile == null then configFile else cfg.configFile;
  realtimePolicy =
    lib.replaceStrings
      [ "@realtimeGroup@" ]
      [
        (builtins.toJSON cfg.realtimeGroup)
      ]
      (builtins.readFile ./realtime-policy.js.in);
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

    realtimeGroup = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = "audio";
      description = ''
        Trusted group allowed to acquire RTKit-bounded real-time scheduling,
        including from systemd user services outside a seated login session.
        Add service users to this group. The permission covers all processes
        of group members, not just loudnessd. Set null to retain the system's
        existing authorization policy.
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

      playback = controllerOptions {
        targetLufs = -16.0;
        silenceGateLufs = -50.0;
        deadbandLu = 0.75;
        maximumBoostDb = 6.0;
        maximumCutDb = 24.0;
        boostRateDbPerSecond = 0.2;
        cutRateDbPerSecond = null;
        historyWindowSeconds = 30.0;
        historyPercentile = 90.0;
        fastCutHoldSeconds = 1.0;
      };

      capture = controllerOptions {
        targetLufs = -18.0;
        silenceGateLufs = -55.0;
        deadbandLu = 1.0;
        maximumBoostDb = 6.0;
        maximumCutDb = 18.0;
        boostRateDbPerSecond = 0.2;
        cutRateDbPerSecond = null;
        historyWindowSeconds = 30.0;
        historyPercentile = 90.0;
        fastCutHoldSeconds = 1.0;
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
    assertions = lib.optionals (cfg.configFile == null) (
      controllerAssertions "playback" cfg.settings.playback
      ++ controllerAssertions "capture" cfg.settings.capture
    );

    environment.systemPackages = [ cfg.package ];

    security.rtkit.enable = lib.mkDefault true;
    security.polkit.extraConfig = lib.mkIf (cfg.realtimeGroup != null) realtimePolicy;
    users.groups = lib.optionalAttrs (cfg.realtimeGroup != null) {
      ${cfg.realtimeGroup} = { };
    };

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
