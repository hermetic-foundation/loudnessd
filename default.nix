# SPDX-License-Identifier: AGPL-3.0-or-later

{
  lib,
  pkg-config,
  pipewire,
  rustPlatform,
  ...
}:

rustPlatform.buildRustPackage {
  pname = "loudnessd";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.lock
      ./Cargo.toml
      ./src
    ];
  };
  cargoLock.lockFile = ./Cargo.lock;
  nativeBuildInputs = [
    pkg-config
    rustPlatform.bindgenHook
  ];
  buildInputs = [ pipewire ];

  meta = {
    description = "Per-application perceived-loudness controller for PipeWire";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "loudnessd";
  };
}
