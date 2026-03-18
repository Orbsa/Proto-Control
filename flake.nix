{
  description = "rotocontrol - Custom control interface for Melbourne Instruments Roto-Control";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "rotocontrol";
          version = "0.1.0";
          src = ./daemon;
          cargoLock.lockFile = ./daemon/Cargo.lock;
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.alsa-lib pkgs.pipewire pkgs.systemdMinimal ];
        };

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = [
            pkgs.cargo
            pkgs.rustc
            pkgs.rust-analyzer
            pkgs.clippy
            pkgs.rustfmt
            pkgs.pkg-config
          ];
          buildInputs = [
            pkgs.alsa-lib
            pkgs.pipewire
            pkgs.systemdMinimal
          ];
          RUST_LOG = "debug";
        };
      });
}
