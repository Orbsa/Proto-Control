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

        # Libraries needed for iced GUI (winit + tiny-skia, both X11 and Wayland)
        guiLibs = with pkgs; [
          wayland
          libxkbcommon
          xorg.libX11
          xorg.libXcursor
          xorg.libXrandr
          xorg.libXi
        ];
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "rotocontrol";
          version = "0.1.0";
          src = ./daemon;
          cargoLock.lockFile = ./daemon/Cargo.lock;
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.alsa-lib pkgs.pipewire pkgs.systemdMinimal ] ++ guiLibs;
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
          ] ++ guiLibs;

          # winit/wgpu need these in LD_LIBRARY_PATH on NixOS
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath guiLibs;

          RUST_LOG = "debug";
        };
      });
}
