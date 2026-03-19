{
  description = "proto-control - PipeWire/Discord/TeamSpeak integration daemon for the Melbourne Instruments Roto-Control";

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
          pname = "proto-control";
          version = "1.0.0";
          # Merge daemon/ source with the tray icon from the repo root
          src = pkgs.runCommand "proto-control-src" {} ''
            cp -r ${./daemon} $out
            chmod -R u+w $out
            cp ${./melbourne.png} $out/assets/tray.png
          '';
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
