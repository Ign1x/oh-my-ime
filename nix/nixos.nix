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
  native = cfg.backend != "wayland";
  installed = if native then import ./native-package.nix {
    inherit pkgs lib frameworks;
    package = cfg.package;
    rime = if cfg.rime.enable then cfg.rime.package else null;
  } else cfg.package;
in
{
  options.programs.oh-my-ime = {
    enable = lib.mkEnableOption "oh-my-ime, a Wayland-first input method platform";

    backend = lib.mkOption {
      type = lib.types.enum [ "wayland" "kde" "gnome" ];
      default = "wayland";
      description = ''
        Desktop input route. `wayland` uses input-method-v2 (Hyprland/sway).
        `kde` is launched by KWin with its privileged connection; select oh-my-ime
        in Plasma's Virtual Keyboard settings. `gnome` registers an engine with
        the session IBus daemon; add oh-my-ime in GNOME's Input Sources.
      '';
    };

    autoStart = lib.mkOption {
      type = lib.types.bool;
      default = cfg.backend != "kde";
      defaultText = lib.literalExpression ''backend != "kde"'';
      description = "Start the user service with the graphical session. KWin must launch the KDE backend itself.";
    };

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
        assertion = !config.i18n.inputMethod.enable || (cfg.backend == "gnome" && config.i18n.inputMethod.type == "ibus");
        message = ''
          programs.oh-my-ime can share i18n.inputMethod only with type = "ibus"
          and backend = "gnome". Other combinations compete for the desktop input route.
        '';
      }
      {
        assertion = cfg.backend != "kde" || !cfg.autoStart;
        message = "The KDE backend must be launched by KWin, so programs.oh-my-ime.autoStart must be false.";
      }
      {
        assertion = cfg.backend != "gnome" || (!cfg.fcitx5.enable && !cfg.ibus.enable);
        message = "GNOME's native IBus route supports native engines; programs.oh-my-ime.fcitx5/ibus are hosted frameworks and require the Wayland backend.";
      }
    ];

    environment.systemPackages = [
      installed
    ]
    ++ lib.optionals (cfg.qtModules && cfg.backend != "gnome") [
      pkgs.qt6Packages.fcitx5-qt
      pkgs.libsForQt5.fcitx5-qt
    ];

    # Engines that other packages install (share/oh-my-ime/engines) are found through
    # the system profile, which NixOS links only parts of share/ into.
    environment.pathsToLink = [ "/share/oh-my-ime" "/share/ibus/component" ];

    # GNOME keeps ownership of the input service; oh-my-ime is one of its engines.
    i18n.inputMethod = lib.mkIf (cfg.backend == "gnome") {
      enable = lib.mkDefault true;
      type = lib.mkDefault "ibus";
      ibus.engines = [ installed ];
      ibus.waylandFrontend = lib.mkDefault true;
    };

    # The unit lives in the package; linking it here makes it part of the systemd
    # user manager's unit files. wantedBy and the environment become a drop-in for that
    # unit. The frameworks are only on its PATH: installed globally, Fcitx 5's
    # autostart entry would start it as the desktop's input method, in omimed's place.
    systemd.packages = lib.optional (cfg.backend != "kde") cfg.package;
    systemd.user.services.omimed = lib.mkIf (cfg.backend != "kde") {
      wantedBy = lib.optional cfg.autoStart "graphical-session.target";
      serviceConfig.ExecStart = lib.mkIf (cfg.backend == "gnome") (lib.mkForce "${installed}/bin/omimed --ibus");
      serviceConfig.RestartPreventExitStatus = 69;
      serviceConfig.KillMode = "mixed";
      serviceConfig.TimeoutStopSec = 6;
      environment = {
        PATH = lib.mkForce servicePath;
      }
      // lib.optionalAttrs cfg.rime.enable {
        OMIME_ENGINE_PATH = "${cfg.rime.package}/share/oh-my-ime/engines";
      };
    };

    # GTK 4 and Qt 6.8+ use the compositor's text-input; the empty GTK_IM_MODULE keeps
    # GTK 3 from loading an immodule that is not there.
    environment.sessionVariables = lib.mkIf (cfg.sessionVariables && cfg.backend != "gnome") {
      GTK_IM_MODULE = "";
      QT_IM_MODULE = "fcitx";
      QT_IM_MODULES = "wayland;fcitx";
      XMODIFIERS = "@im=fcitx";
    };
  };
}
