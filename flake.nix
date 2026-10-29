{
  description = "oh-my-ime — a modern, Wayland-first, UI-agnostic input method platform for Linux";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      shell =
        pkgs: extraPackages:
        let
          # Libraries that are linked at build time.
          buildLibs = with pkgs; [
            dbus
            fontconfig
            freetype
            libxkbcommon
            wayland
          ];
          # Libraries that toolkits (winit/Slint, Mesa) dlopen at runtime.
          runtimeLibs = with pkgs; [
            fontconfig
            freetype
            libGL
            libxkbcommon
            vulkan-loader
            wayland
          ];
        in
        pkgs.mkShell {
          packages =
            buildLibs
            ++ (with pkgs; [
              pkg-config
              # End-to-end tests: a headless compositor, its X server and a virtual
              # keyboard.
              sway
              xwayland
              wtype
            ])
            ++ extraPackages;

          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;

          shellHook = ''
            # The host session may point GTK/Qt at fcitx; test sessions must use
            # text-input-v3 so oh-my-ime actually sees the traffic.
            unset GTK_IM_MODULE QT_IM_MODULE XMODIFIERS
          '';
        };
    in
    {
      packages = forAllSystems (pkgs: rec {
        oh-my-ime = pkgs.callPackage ./nix/package.nix { };
        default = oh-my-ime;
      });

      overlays.default = final: _prev: {
        oh-my-ime = final.callPackage ./nix/package.nix { };
      };

      checks = forAllSystems (pkgs: {
        package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      });

      devShells = forAllSystems (pkgs: {
        # Everything for development, including Hyprland for manual testing.
        default = shell pkgs (
          with pkgs;
          [
            cargo-deny
            hyprland
            foot
            grim
            wlrctl
            jq
          ]
        );
        # Just enough to build and run the end-to-end tests (used by CI).
        e2e = shell pkgs [ ];
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
