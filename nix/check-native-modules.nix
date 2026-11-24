# Evaluate the desktop routes without building Rust or touching a desktop session.
{ pkgs }:
let
  lib = pkgs.lib;
  home = backend: extra: (lib.evalModules {
    specialArgs = { inherit pkgs; };
    modules = [
      ./home-manager.nix
      {
        options = {
          assertions = lib.mkOption { type = lib.types.anything; };
          home.packages = lib.mkOption { type = lib.types.anything; };
          home.sessionVariables = lib.mkOption { type = lib.types.anything; default = { }; };
          xdg.configFile = lib.mkOption { type = lib.types.anything; default = { }; };
          systemd.user.services = lib.mkOption { type = lib.types.anything; default = { }; };
        };
        config.programs.oh-my-ime = { enable = true; inherit backend; } // extra;
      }
    ];
  }).config;
  nixos = backend: extra: (import (pkgs.path + "/nixos/lib/eval-config.nix") {
    system = pkgs.stdenv.hostPlatform.system;
    modules = [ ./nixos.nix {
      boot.isContainer = true;
      system.stateVersion = "24.05";
      programs.oh-my-ime = { enable = true; inherit backend; } // extra;
    } ];
  }).config;
  valid = config: builtins.all (entry: entry.assertion) config.assertions;
  hWayland = home "wayland" { };
  hKde = home "kde" { rime.enable = true; };
  hGnome = home "gnome" { rime.enable = true; };
  nWayland = nixos "wayland" { };
  nKde = nixos "kde" { rime.enable = true; };
  nGnome = nixos "gnome" { rime.enable = true; };
  checks = {
    home-wayland-compatible = valid hWayland
      && hWayland.home.sessionVariables.QT_IM_MODULES == "wayland;fcitx"
      && hWayland.systemd.user.services.omimed.Install.WantedBy == [ "graphical-session.target" ];
    home-kde-compositor-launch = valid hKde && hKde.systemd.user.services == { };
    home-gnome-session-ibus = valid hGnome && hGnome.home.sessionVariables == { }
      && lib.hasSuffix " --ibus" hGnome.systemd.user.services.omimed.Service.ExecStart;
    home-invalid-kde-autostart = !(valid (home "kde" { autoStart = true; }));
    home-invalid-gnome-hosting = !(valid (home "gnome" { fcitx5.enable = true; }));
    nixos-wayland-compatible = valid nWayland
      && nWayland.environment.sessionVariables.QT_IM_MODULES == "wayland;fcitx"
      && nWayland.systemd.user.services.omimed.wantedBy == [ "graphical-session.target" ];
    nixos-kde-compositor-launch = valid nKde && !(nKde.systemd.user.services ? omimed);
    nixos-gnome-session-ibus = valid nGnome && nGnome.i18n.inputMethod.type == "ibus"
      && nGnome.i18n.inputMethod.enable
      && builtins.length nGnome.i18n.inputMethod.ibus.engines == 1
      && !(nGnome.environment.sessionVariables ? QT_IM_MODULES)
      && lib.hasSuffix " --ibus" nGnome.systemd.user.services.omimed.serviceConfig.ExecStart;
    nixos-invalid-kde-autostart = !(valid (nixos "kde" { autoStart = true; }));
    nixos-invalid-gnome-hosting = !(valid (nixos "gnome" { ibus.enable = true; }));
  };
in
assert lib.assertMsg (builtins.all (value: value) (builtins.attrValues checks)) "native desktop module checks failed";
checks
