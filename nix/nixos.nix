# NixOS module: `programs.oh-my-ime`.
#
# omimed is a user daemon that owns the seat's input method; the system side is the
# package, the unit (so sessions through systemd start it), the applications'
# environment, and the hosted frameworks on the unit's PATH. docs/hyprland.md has the
# manual equivalent.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.oh-my-ime;

  # What omimed runs, built here rather than globally installed: an engine picks its
  # program from omimed's PATH (data/engines/*/engine.toml), so the frameworks only
  # need to be reachable from the unit.
  fcitx5 = pkgs.qt6Packages.fcitx5-with-addons.override { addons = cfg.fcitx5.addons; };
  ibus = pkgs.ibus-with-plugins.override { plugins = cfg.ibus.engines; };
  frameworks = lib.optional cfg.fcitx5.enable fcitx5 ++ lib.optional cfg.ibus.enable ibus;
  servicePath = import ./service-path.nix { inherit lib frameworks; };
in
{
  options.programs.oh-my-ime = {
    enable = lib.mkEnableOption "oh-my-ime, a Wayland-first input method platform";

    package = lib.mkOption {
      type = lib.types.package;
      # The module ships inside the flake, so the package next to it is this flake's
      # package for the system, built with the user's nixpkgs.
      default = pkgs.callPackage ./package.nix { };
      defaultText = lib.literalMD "the package built from this flake";
      description = ''
        The oh-my-ime package that provides omimed and omime. Its bundled Echo and
        Sparkle demos are offered but disabled by default; use `omime enable echo`
        or `omime enable sparkle`, or add them in the settings app.
      '';
    };

    fcitx5 = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Make Fcitx 5 available to omimed as an engine: its input methods and addons
          become part of oh-my-ime's switch list.
        '';
      };

      addons = lib.mkOption {
        type = lib.types.listOf lib.types.package;
        default = [ ];
        example = lib.literalExpression "[ pkgs.qt6Packages.fcitx5-chinese-addons ]";
        description = ''
          Fcitx 5 addons to host, such as
          {package}`pkgs.qt6Packages.fcitx5-chinese-addons` for the Chinese input
          methods.
        '';
      };
    };

    ibus = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Make IBus available to omimed as an engine: its engines become part of
          oh-my-ime's switch list.
        '';
      };

      engines = lib.mkOption {
        type = lib.types.listOf lib.types.package;
        default = [ ];
        example = lib.literalExpression "[ pkgs.ibus-engines.libpinyin ]";
        description = "IBus engines to host, such as {package}`pkgs.ibus-engines.libpinyin`.";
      };
    };

    rime = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Offer the Rime engine (Xiaohe shuangpin first, and Rime's other schemas),
          with librime and Rime's data.
        '';
      };

      package = lib.mkOption {
        type = lib.types.package;
        default = pkgs.callPackage ./rime.nix { };
        defaultText = lib.literalMD "the Rime engine built from this flake";
        description = "The package of oh-my-ime's Rime engine.";
      };
    };

    qtModules = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Install the Fcitx 5 Qt input modules. Qt 5 and Qt applications on X11 reach
        omimed through `QT_IM_MODULE=fcitx`, which needs them.
      '';
    };

    sessionVariables = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Set `QT_IM_MODULE`, `QT_IM_MODULES`, `XMODIFIERS` and `GTK_IM_MODULE` for the
        session, the routes applications use to find omimed (docs/hyprland.md).
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = !config.i18n.inputMethod.enable;
        message = ''
          programs.oh-my-ime and i18n.inputMethod cannot both be enabled: a Wayland
          seat has room for one input method, and two would fight over it. Disable
          i18n.inputMethod or programs.oh-my-ime.
        '';
      }
    ];

    environment.systemPackages = [
      cfg.package
    ]
    ++ lib.optionals cfg.qtModules [
      pkgs.qt6Packages.fcitx5-qt
      pkgs.libsForQt5.fcitx5-qt
    ];

    # Engines that other packages install (share/oh-my-ime/engines) are found through
    # the system profile, which NixOS links only parts of share/ into.
    environment.pathsToLink = [ "/share/oh-my-ime" ];

    # The unit lives in the package; linking it here makes it part of the systemd
    # user manager's unit files. wantedBy and the environment become a drop-in for that
    # unit. The frameworks are only on its PATH: installed globally, Fcitx 5's
    # autostart entry would start it as the desktop's input method, in omimed's place.
    systemd.packages = [ cfg.package ];
    systemd.user.services.omimed = {
      wantedBy = [ "graphical-session.target" ];
      serviceConfig.RestartPreventExitStatus = 69;
      environment = {
        PATH = lib.mkForce servicePath;
      }
      // lib.optionalAttrs cfg.rime.enable {
        OMIME_ENGINE_PATH = "${cfg.rime.package}/share/oh-my-ime/engines";
      };
    };

    # GTK 4 and Qt 6.8+ use the compositor's text-input; the empty GTK_IM_MODULE keeps
    # GTK 3 from loading an immodule that is not there.
    environment.sessionVariables = lib.mkIf cfg.sessionVariables {
      GTK_IM_MODULE = "";
      QT_IM_MODULE = "fcitx";
      QT_IM_MODULES = "wayland;fcitx";
      XMODIFIERS = "@im=fcitx";
    };
  };
}
