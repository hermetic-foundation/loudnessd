{
  description = "Per-application perceived-loudness normalization for PipeWire";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    {
      self,
      nixpkgs,
    }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          loudnessd = pkgs.callPackage ./default.nix { };
        in
        {
          default = loudnessd;
          inherit loudnessd;
        }
      );

      checks = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          loudnessd = self.packages.${system}.default;
          cargoDenySource = nixpkgs.lib.fileset.toSource {
            root = ./.;
            fileset = nixpkgs.lib.fileset.unions [
              ./Cargo.lock
              ./Cargo.toml
              ./deny.toml
              ./src
            ];
          };
          # Configuration checks do not execute the daemon or need a Rust rebuild.
          moduleTestPackage = pkgs.writeShellScriptBin "loudnessd" "exit 1";
          controllerModuleConfiguration =
            settings:
            nixpkgs.lib.nixosSystem {
              inherit system;
              modules = [
                self.nixosModules.default
                {
                  services.loudnessd = {
                    enable = true;
                    package = moduleTestPackage;
                    inherit settings;
                  };
                }
              ];
            };
          defaultModuleConfiguration = controllerModuleConfiguration { };
          defaultService = defaultModuleConfiguration.config.systemd.user.services.loudnessd;
          moduleConfiguration = controllerModuleConfiguration {
            playback = {
              targetLufs = -17.0;
              historyWindowSeconds = 300.0;
              historyPercentile = 100.0;
              fastCutHoldSeconds = 0.0;
              cutRateDbPerSecond = 2.5;
            };
            capture = {
              historyWindowSeconds = 1.0;
              historyPercentile = 1.0;
              fastCutHoldSeconds = 2.0;
              cutRateDbPerSecond = 1.5;
            };
            applications.browser.capture = false;
          };
          service = moduleConfiguration.config.systemd.user.services.loudnessd;
          invalidControllerSettings = {
            silenceGateLufs = -12.0;
            deadbandLu = -1.0;
            maximumBoostDb = -1.0;
            maximumCutDb = -1.0;
            boostRateDbPerSecond = 0.0;
            cutRateDbPerSecond = 0.0;
            historyWindowSeconds = 0.0;
            historyPercentile = 0.0;
            fastCutHoldSeconds = -1.0;
          };
          invalidModuleConfiguration = controllerModuleConfiguration {
            playback = invalidControllerSettings;
            capture = invalidControllerSettings;
          };
          outOfRangeControllerSettings = {
            historyWindowSeconds = 301.0;
            historyPercentile = 100.1;
            cutRateDbPerSecond = -1.0;
          };
          outOfRangeModuleConfiguration = controllerModuleConfiguration {
            playback = outOfRangeControllerSettings;
            capture = outOfRangeControllerSettings;
          };
          controllerAssertions =
            configuration:
            nixpkgs.lib.filter (
              # Successful NixOS assertions can have deliberately lazy messages.
              entry: !entry.assertion && nixpkgs.lib.hasPrefix "services.loudnessd.settings." entry.message
            ) configuration.config.assertions;
          externalConfig = pkgs.writeText "loudnessd-external.toml" ''
            # Preserve externally managed values and comments verbatim.
            [defaults]
            playback = false
            capture = true

            [playback]
            history_window_seconds = 12.0
            history_percentile = 75.0
            fast_cut_hold_seconds = 0.5
            cut_rate_db_per_second = 4.0
          '';
          externalModuleConfiguration = nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [
              self.nixosModules.default
              {
                services.loudnessd = {
                  enable = true;
                  package = moduleTestPackage;
                  configFile = externalConfig;
                  settings.playback = invalidControllerSettings;
                  settings.capture = invalidControllerSettings;
                };
              }
            ];
          };
          externalService = externalModuleConfiguration.config.systemd.user.services.loudnessd;
          realtimePolicy = pkgs.writeText "loudnessd-realtime.rules" moduleConfiguration.config.security.polkit.extraConfig;
          customRealtimeConfiguration = nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [
              self.nixosModules.default
              {
                services.loudnessd = {
                  enable = true;
                  realtimeGroup = "loudnessd-realtime";
                };
              }
            ];
          };
          customRealtimePolicy = pkgs.writeText "loudnessd-custom-realtime.rules" customRealtimeConfiguration.config.security.polkit.extraConfig;
          existingRealtimeConfiguration = nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [
              self.nixosModules.default
              {
                services.loudnessd = {
                  enable = true;
                  realtimeGroup = null;
                };
              }
            ];
          };
        in
        {
          package = loudnessd;
          rustfmt =
            pkgs.runCommand "loudnessd-rustfmt-check"
              {
                nativeBuildInputs = [
                  pkgs.cargo
                  pkgs.rustfmt
                ];
                source = loudnessd.src;
              }
              ''
                cp -r "$source" source
                chmod -R u+w source
                cd source
                cargo fmt --check
                touch "$out"
              '';
          clippy = loudnessd.overrideAttrs (old: {
            pname = "loudnessd-clippy-check";
            nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
            buildPhase = ''
              runHook preBuild
              cargo clippy -j "''${NIX_BUILD_CORES:-1}" --offline --all-targets -- -D warnings
              runHook postBuild
            '';
            doCheck = false;
            installPhase = ''
              touch "$out"
            '';
            postInstall = "";
          });
          cargo-deny = loudnessd.overrideAttrs (old: {
            pname = "loudnessd-cargo-deny-check";
            src = cargoDenySource;
            nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.cargo-deny ];
            buildPhase = ''
              runHook preBuild
              cargo deny check licenses bans sources
              runHook postBuild
            '';
            doCheck = false;
            installPhase = ''
              touch "$out"
            '';
            postInstall = "";
          });
          shellcheck =
            pkgs.runCommand "loudnessd-shellcheck"
              {
                nativeBuildInputs = [ pkgs.shellcheck ];
                harnesses = [
                  ./tests/live/audio-soak.sh
                  ./tests/live/hardware-rate-matrix.sh
                  ./tests/live/suspend-resume.sh
                ];
              }
              ''
                shellcheck $harnesses
                touch "$out"
              '';
          portable-systemd =
            pkgs.runCommand "loudnessd-portable-systemd-check"
              {
                nativeBuildInputs = [ pkgs.systemd ];
                serviceFile = "${loudnessd}/lib/systemd/user/loudnessd.service";
              }
              ''
                grep -Fxq 'ExecStart=${nixpkgs.lib.getExe loudnessd} --daemon' "$serviceFile"
                mkdir -p home runtime
                HOME="$PWD/home" \
                  XDG_RUNTIME_DIR="$PWD/runtime" \
                  SYSTEMD_UNIT_PATH=${loudnessd}/lib/systemd/user:${pkgs.pipewire}/lib/systemd/user:${pkgs.systemd}/example/systemd/user \
                  systemd-analyze --user verify "$serviceFile"
                touch "$out"
              '';
          realtime-policy =
            assert moduleConfiguration.config.security.rtkit.enable;
            assert builtins.hasAttr "audio" moduleConfiguration.config.users.groups;
            assert builtins.hasAttr "loudnessd-realtime" customRealtimeConfiguration.config.users.groups;
            assert
              !(nixpkgs.lib.hasInfix "org.freedesktop.RealtimeKit1.acquire-real-time" existingRealtimeConfiguration.config.security.polkit.extraConfig);
            pkgs.runCommand "loudnessd-realtime-policy-check"
              {
                nativeBuildInputs = [ pkgs.nodejs ];
                inherit realtimePolicy customRealtimePolicy;
                policyTest = ./tests/nix/realtime-policy.cjs;
              }
              ''
                node "$policyTest" "$realtimePolicy" audio
                node "$policyTest" "$customRealtimePolicy" loudnessd-realtime
                touch "$out"
              '';
          module =
            assert builtins.elem "graphical-session.target" service.wantedBy;
            assert builtins.elem "pipewire.service" service.partOf;
            assert controllerAssertions defaultModuleConfiguration == [ ];
            assert controllerAssertions moduleConfiguration == [ ];
            assert builtins.length (controllerAssertions invalidModuleConfiguration) == 18;
            assert nixpkgs.lib.all (entry: !entry.assertion) (controllerAssertions invalidModuleConfiguration);
            assert
              nixpkgs.lib.count (entry: !entry.assertion) (controllerAssertions outOfRangeModuleConfiguration)
              == 6;
            assert controllerAssertions externalModuleConfiguration == [ ];
            pkgs.runCommand "loudnessd-module-check"
              {
                nativeBuildInputs = [ pkgs.python3 ];
                defaultExecStart = defaultService.serviceConfig.ExecStart;
                execStart = service.serviceConfig.ExecStart;
                externalExecStart = externalService.serviceConfig.ExecStart;
                externalConfigPath = externalConfig;
                exampleConfig = ./config.example.toml;
              }
              ''
                python3 - <<'PY'
                import os
                from pathlib import Path
                import shlex
                import tomllib

                def service_config(name):
                    command = shlex.split(os.environ[name])
                    assert len(command) == 4, command
                    assert command[1:3] == ["--daemon", "--config"], command
                    return Path(command[3])

                defaults = tomllib.loads(service_config("defaultExecStart").read_text())
                assert defaults["defaults"] == {"playback": True, "capture": True}
                assert defaults.get("applications", {}) == {}
                common = {
                    "maximum_boost_db": 6.0,
                    "boost_rate_db_per_second": 0.2,
                    "history_window_seconds": 30.0,
                    "history_percentile": 90.0,
                    "fast_cut_hold_seconds": 1.0,
                }
                expected = {
                    "playback": dict(common, target_lufs=-16.0, silence_gate_lufs=-50.0,
                                     deadband_lu=0.75, maximum_cut_db=24.0),
                    "capture": dict(common, target_lufs=-18.0, silence_gate_lufs=-55.0,
                                    deadband_lu=1.0, maximum_cut_db=18.0),
                }
                example = tomllib.loads(Path(os.environ["exampleConfig"]).read_text())
                for direction, settings in expected.items():
                    assert defaults[direction] == settings, defaults[direction]
                    assert "cut_rate_db_per_second" not in defaults[direction]
                    assert example[direction] == settings, example[direction]

                configured = tomllib.loads(service_config("execStart").read_text())
                assert configured["defaults"] == defaults["defaults"]
                assert configured["applications"] == {"browser": {"capture": False}}
                assert configured["playback"] == dict(
                    expected["playback"], target_lufs=-17.0, history_window_seconds=300.0,
                    history_percentile=100.0, fast_cut_hold_seconds=0.0,
                    cut_rate_db_per_second=2.5)
                assert configured["capture"] == dict(
                    expected["capture"], history_window_seconds=1.0,
                    history_percentile=1.0, fast_cut_hold_seconds=2.0,
                    cut_rate_db_per_second=1.5)

                external_path = service_config("externalExecStart")
                assert external_path == Path(os.environ["externalConfigPath"])
                external_text = external_path.read_text()
                assert external_text.startswith("# Preserve externally managed values and comments verbatim.\n")
                assert tomllib.loads(external_text) == {
                    "defaults": {"playback": False, "capture": True},
                    "playback": {"history_window_seconds": 12.0, "history_percentile": 75.0,
                                 "fast_cut_hold_seconds": 0.5, "cut_rate_db_per_second": 4.0},
                }
                PY
                touch "$out"
              '';
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              clippy
              pkg-config
              rustc
              rustfmt
              shellcheck
            ];
            nativeBuildInputs = [ pkgs.rustPlatform.bindgenHook ];
            buildInputs = [ pkgs.pipewire ];
          };
        }
      );

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt-tree);

      nixosModules.default = import ./nix/nixos-module.nix { inherit self; };
    };
}
