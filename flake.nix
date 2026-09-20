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
          moduleConfiguration = nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [
              self.nixosModules.default
              {
                services.loudnessd = {
                  enable = true;
                  package = moduleTestPackage;
                  settings.playback.targetLufs = -17.0;
                  settings.applications.browser.capture = false;
                };
              }
            ];
          };
          service = moduleConfiguration.config.systemd.user.services.loudnessd;
          invalidModuleConfiguration = nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [
              self.nixosModules.default
              {
                services.loudnessd = {
                  enable = true;
                  settings.playback = {
                    targetLufs = -16.0;
                    silenceGateLufs = -12.0;
                  };
                };
              }
            ];
          };
          invalidControllerAssertion = nixpkgs.lib.findFirst (
            entry:
            entry.message == "services.loudnessd.settings.playback.silenceGateLufs must be below targetLufs"
          ) null invalidModuleConfiguration.config.assertions;
          externalConfig = pkgs.writeText "loudnessd-external.toml" ''
            [defaults]
            playback = false
            capture = true
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
            assert invalidControllerAssertion != null;
            assert !invalidControllerAssertion.assertion;
            pkgs.runCommand "loudnessd-module-check"
              {
                execStart = service.serviceConfig.ExecStart;
                externalExecStart = externalService.serviceConfig.ExecStart;
              }
              ''
                generatedConfig="''${execStart##*--config }"
                grep -Fq '[defaults]' "$generatedConfig"
                grep -Fq 'playback = true' "$generatedConfig"
                grep -Fq 'capture = false' "$generatedConfig"
                grep -Fq '[playback]' "$generatedConfig"
                grep -Fq 'target_lufs = -17.0' "$generatedConfig"
                grep -Fq 'maximum_boost_db = 18.0' "$generatedConfig"
                grep -Fq '[capture]' "$generatedConfig"
                grep -Fq 'target_lufs = -18.0' "$generatedConfig"
                externalConfig="''${externalExecStart##*--config }"
                grep -Fq 'playback = false' "$externalConfig"
                grep -Fq 'capture = true' "$externalConfig"
                touch $out
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
