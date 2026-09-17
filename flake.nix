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
          moduleConfiguration = nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [
              self.nixosModules.default
              {
                services.loudnessd = {
                  enable = true;
                  settings.applications.browser.capture = false;
                };
              }
            ];
          };
          service = moduleConfiguration.config.systemd.user.services.loudnessd;
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
                  configFile = externalConfig;
                };
              }
            ];
          };
          externalService = externalModuleConfiguration.config.systemd.user.services.loudnessd;
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
              cargo clippy --offline --all-targets -- -D warnings
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
                harness = ./tests/live/audio-soak.sh;
              }
              ''
                shellcheck "$harness"
                touch "$out"
              '';
          module =
            assert builtins.elem "graphical-session.target" service.wantedBy;
            assert builtins.elem "pipewire.service" service.partOf;
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
