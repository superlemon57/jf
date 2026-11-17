{
  description = "jf — client Jellyfin natif pour le salon, avec mpv intégré";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # Chargées à l'exécution par winit/glutin (dlopen), donc absentes du RPATH.
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
            # Pas d'icône propre : le lanceur reprend celle de Jellyfin du thème d'icônes.
            install -Dm644 assets/jf.desktop $out/share/applications/jf.desktop
          '';
          # fc-match (polices) et wpctl (volume) : ceux du système passent d'abord s'ils existent.
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
            description = "Client Jellyfin natif pour le salon, avec mpv intégré";
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
