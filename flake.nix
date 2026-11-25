{
  description = "jf — lightweight Jellyfin HTPC client";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # Loaded at runtime by winit/glutin (dlopen), so missing from the RPATH.
      runtimeLibs =
        pkgs: with pkgs; [
          libGL
          libxkbcommon
          wayland
          libx11
          libxcursor
          libxi
          libxrandr
        ];
    in
    {
      packages = forAll (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "jf";
          version = "0.2.0";
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./assets
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.makeWrapper
          ];
          buildInputs = [ pkgs.mpv-unwrapped ];
          postInstall = ''
            # No icon of its own: the launcher uses Jellyfin's from the icon theme.
            install -Dm644 assets/jf.desktop $out/share/applications/jf.desktop
          '';
          # fc-match (fonts) and wpctl (volume): the system's come first if present.
          postFixup = ''
            patchelf --add-rpath ${pkgs.lib.makeLibraryPath (runtimeLibs pkgs)} $out/bin/jf
            wrapProgram $out/bin/jf --suffix PATH : ${
              pkgs.lib.makeBinPath [
                pkgs.fontconfig
                pkgs.wireplumber
              ]
            }
          '';
          meta = {
            description = "Lightweight Jellyfin HTPC client";
            license = pkgs.lib.licenses.mit;
            mainProgram = "jf";
            platforms = systems;
          };
        };
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            pkg-config
          ];
          buildInputs = [ pkgs.mpv-unwrapped ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
        };
      });
    };
}
