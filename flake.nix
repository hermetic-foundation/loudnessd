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
        in
        {
          package = self.packages.${system}.default;
          module =
            assert builtins.elem "graphical-session.target" service.wantedBy;
            pkgs.runCommand "loudnessd-module-check"
              {
                execStart = service.serviceConfig.ExecStart;
              }
              ''
                generatedConfig="''${execStart##*--config }"
                grep -Fq '[defaults]' "$generatedConfig"
                grep -Fq 'playback = true' "$generatedConfig"
                grep -Fq 'capture = false' "$generatedConfig"
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
