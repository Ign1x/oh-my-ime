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
            librime
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
          # GTK 4 finds its own typelibs in the gtk4 output, and the namespaces they
          # import in the outputs of their libraries.
          giTypelibs = pkgs.lib.makeSearchPath "lib/girepository-1.0" (
            map pkgs.lib.getLib (
              with pkgs;
              [
                gtk4
                glib
                gdk-pixbuf
                pango
                graphene
                harfbuzz
                gobject-introspection
              ]
            )
          );
        in
        pkgs.mkShell {
          packages =
            buildLibs
            ++ (with pkgs; [
              pkg-config
              # End-to-end tests: a headless compositor, its X server, a virtual
              # keyboard and screenshots.
              sway
              # Validates both generated Hyprland setup formats without starting a desktop.
              hyprland
              xwayland
              wtype
              grim
              # The hosting tests run the real Fcitx 5 and IBus, both with pinyin.
              (qt6Packages.fcitx5-with-addons.override { addons = [ qt6Packages.fcitx5-chinese-addons ]; })
              (ibus-with-plugins.override { plugins = [ ibus-engines.libpinyin ]; })
            ])
            ++ extraPackages;

          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
          # The Rime engine's schemas and dictionaries.
          OMIME_RIME_SHARED_DATA = "${pkgs.rime-data}/share/rime-data";
          GI_TYPELIB_PATH = giTypelibs;

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
        # The Rime engine, with librime and Rime's data: `nix profile install .#rime`.
        rime = pkgs.callPackage ./nix/rime.nix { };
      });

      overlays.default = final: _prev: {
        oh-my-ime = final.callPackage ./nix/package.nix { };
      };

      checks = forAllSystems (pkgs: {
        package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        rime = self.packages.${pkgs.stdenv.hostPlatform.system}.rime;
      });

      devShells = forAllSystems (pkgs: {
        # Everything for development, including Hyprland for manual testing.
        default = shell pkgs (
          with pkgs;
          [
            cargo-deny
            foot
            grim
            wlrctl
            jq
          ]
        );
        # Just enough to build and run the end-to-end tests (used by CI). The Python
        # engine example needs Python with PyGObject and GTK 4.
        e2e = shell pkgs [
          (pkgs.python3.withPackages (ps: [ ps.pygobject3 ]))
          pkgs.gtk4
        ];
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
