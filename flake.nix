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
          # Runtime libraries, including those toolkits (winit/Slint, Mesa) dlopen.
          runtimeLibs = with pkgs; [
            fontconfig
            freetype
            libGL
            libxkbcommon
            # CI may reuse a Rime binary linked outside this Nix shell.
            librime
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
              # The setup safety tests need a compositor without input-method-v2.
              weston
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

      nixosModules.default = ./nix/nixos.nix;
      homeManagerModules.default = ./nix/home-manager.nix;

      checks = forAllSystems (
        pkgs:
        let
          lib = pkgs.lib;
          system = pkgs.stdenv.hostPlatform.system;

          # The module's options and values; building the system itself is not the
          # check, so only its derivation is written.
          nixos = nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [
              self.nixosModules.default
              {
                # A container keeps the evaluation away from hardware options.
                boot.isContainer = true;
                system.stateVersion = "24.05";
                programs.oh-my-ime = {
                  enable = true;
                  fcitx5 = {
                    enable = true;
                    addons = [ pkgs.qt6Packages.fcitx5-chinese-addons ];
                  };
                  ibus = {
                    enable = true;
                    engines = [ pkgs.ibus-engines.libpinyin ];
                  };
                  rime.enable = true;
                };
              }
            ];
          };

          # Home Manager is not an input of this flake, so the module is evaluated
          # against the options it uses rather than a whole Home Manager.
          home = lib.evalModules {
            specialArgs = { inherit pkgs; };
            modules = [
              self.homeManagerModules.default
              {
                programs.oh-my-ime = {
                  enable = true;
                  fcitx5 = {
                    enable = true;
                    addons = [ pkgs.qt6Packages.fcitx5-chinese-addons ];
                  };
                  ibus = {
                    enable = true;
                    engines = [ pkgs.ibus-engines.libpinyin ];
                  };
                  settings = {
                    engines = [
                      "fcitx5/shuangpin"
                    ];
                    initial_input_method = "fcitx5/shuangpin";
                  };
                };
              }
              {
                options.home.packages = lib.mkOption { type = lib.types.anything; };
                options.home.sessionVariables = lib.mkOption { type = lib.types.anything; };
                options.xdg.configFile = lib.mkOption { type = lib.types.anything; };
                options.systemd.user.services = lib.mkOption { type = lib.types.anything; };
              }
            ];
          };
        in
        {
          package = self.packages.${system}.default;
          rime = self.packages.${system}.rime;

          nixos-module = pkgs.runCommand "check-nixos-module" { } ''
            # The system is not built: its derivation path is only written. Dropping the
            # string's context keeps this runCommand from depending on the system.
            echo ${builtins.unsafeDiscardStringContext nixos.config.system.build.toplevel.drvPath} > $out
            echo ${builtins.unsafeDiscardStringContext (lib.concatStringsSep "," nixos.config.systemd.user.services.omimed.wantedBy)} >> $out
            echo ${builtins.unsafeDiscardStringContext nixos.config.systemd.user.services.omimed.environment.PATH} >> $out
            echo ${builtins.unsafeDiscardStringContext nixos.config.systemd.user.services.omimed.environment.OMIME_ENGINE_PATH} >> $out
            echo "${nixos.config.environment.sessionVariables.QT_IM_MODULES}" >> $out
            echo 'RestartPreventExitStatus=${toString nixos.config.systemd.user.services.omimed.serviceConfig.RestartPreventExitStatus}' >> $out
            grep -qx 'RestartPreventExitStatus=69' $out
            # The unit starts with the session, and the frameworks sit on its PATH.
            grep -qx 'graphical-session.target' $out
            grep -q 'fcitx5-with-addons' $out
            grep -q 'ibus-with-plugins' $out
            # The user's profiles stay on it, for engines' programs.
            grep -q '%h/.nix-profile/bin' $out
            grep -q 'oh-my-ime-rime.*/share/oh-my-ime/engines' $out
            grep -qx 'wayland;fcitx' $out
          '';

          home-manager-module = pkgs.runCommand "check-home-manager-module" { } ''
            cat > $out <<'EOF'
            ExecStart=${builtins.unsafeDiscardStringContext home.config.systemd.user.services.omimed.Service.ExecStart}
            config=${home.config.xdg.configFile."oh-my-ime/config.toml".source}
            packages=${toString (builtins.length home.config.home.packages)}
            QT_IM_MODULES=${home.config.home.sessionVariables.QT_IM_MODULES}
            RestartPreventExitStatus=${toString home.config.systemd.user.services.omimed.Service.RestartPreventExitStatus}
            EOF
            # The settings become omimed's configuration, and the unit is the one from
            # data/omimed.service, with the package's absolute paths.
            grep -qxF 'initial_input_method = "fcitx5/shuangpin"' ${
              home.config.xdg.configFile."oh-my-ime/config.toml".source
            }
            grep -qxF 'engines = ["fcitx5/shuangpin"]' ${
              home.config.xdg.configFile."oh-my-ime/config.toml".source
            }
            # Only the platform: the frameworks are on the unit's PATH, not in the
            # profile.
            grep -qx 'packages=1' $out
            grep -q 'fcitx5-with-addons' <<< '${builtins.unsafeDiscardStringContext (toString home.config.systemd.user.services.omimed.Service.Environment)}'
            grep -qx 'QT_IM_MODULES=wayland;fcitx' $out
            grep -qx 'RestartPreventExitStatus=69' $out
          '';
        }
      );

      devShells = forAllSystems (pkgs: {
        # Add cargo-deny and foot to the shared end-to-end test tools.
        default = shell pkgs (
          with pkgs;
          [
            cargo-deny
            foot
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
