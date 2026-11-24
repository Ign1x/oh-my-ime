# Home Manager module: `programs.oh-my-ime`.
#
# The user half of nix/nixos.nix: the daemon and its unit, the session variables, the
# configuration file, and the hosted frameworks. The NixOS module covers the system
# half for NixOS users; this one also serves Home Manager on other distributions.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.oh-my-ime;

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

  toml = pkgs.formats.toml { };
in
{
  options.programs.oh-my-ime = {
    enable = lib.mkEnableOption "oh-my-ime, a Wayland-first input method platform";

    backend = lib.mkOption {
      type = lib.types.enum [ "wayland" "kde" "gnome" ];
      default = "wayland";
      description = ''
        Use input-method-v2 (`wayland`), KWin's privileged input-method-v1 (`kde`),
        or the session IBus engine (`gnome`). Select the installed input method in
        Plasma's Virtual Keyboard or GNOME's Input Sources settings. GNOME needs
        the distribution's session IBus service and toolkit modules.
      '';
    };

    autoStart = lib.mkOption {
      type = lib.types.bool;
      default = cfg.backend != "kde";
      defaultText = lib.literalExpression ''backend != "kde"'';
      description = "Start omimed with the graphical session. KWin must launch the KDE backend itself.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { };
      defaultText = lib.literalMD "the package built from this flake";
      description = "The oh-my-ime package that provides omimed and omime.";
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

    settings = lib.mkOption {
      type = toml.type;
      default = { };
      example = {
        engines = [
          "fcitx5/shuangpin"
        ];
        initial_input_method = "fcitx5/shuangpin";
      };
      description = ''
        Written to {file}`$XDG_CONFIG_HOME/oh-my-ime/config.toml`; the keys are those
        of the configuration file (data/config.toml), with the same defaults. Empty
        writes nothing, so omimed uses its own defaults. The bundled Echo and
        Sparkle demos are offered but disabled by default; add `"echo"` or
        `"sparkle"` to `settings.engines` to enable them declaratively.

        The file is then a symlink into the Nix store, which is read-only: omimed's
        own edits (`omime config`, `omime enable`, and the settings window) do not
        persist, and the next `home-manager switch` restores the generated file.
        Change this option instead, then run `home-manager switch` and `omime reload`.
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
        assertion = cfg.backend != "kde" || !cfg.autoStart;
        message = "The KDE backend must be launched by KWin; programs.oh-my-ime.autoStart must be false.";
      }
      {
        assertion = cfg.backend != "gnome" || (!cfg.fcitx5.enable && !cfg.ibus.enable);
        message = "GNOME's native IBus route supports native engines; hosted Fcitx5/IBus engines require the Wayland backend.";
      }
    ];
    home.packages = [ installed ];

    # The user unit from data/omimed.service, with the package's absolute paths. The
    # frameworks are only on its PATH: in the profile, Fcitx 5's autostart entry would
    # start it as the desktop's input method, in omimed's place.
    systemd.user.services.omimed = lib.mkIf (cfg.backend != "kde") {
      Unit = {
        Description = "oh-my-ime input method daemon";
        Documentation = [ "https://github.com/Ign1x/oh-my-ime" ];
        PartOf = [ "graphical-session.target" ];
        After = [ "graphical-session.target" ];
      };

      Service = {
        # omimed takes io.github.ign1x.OhMyIme on the session bus.
        Type = "dbus";
        BusName = "io.github.ign1x.OhMyIme";
        ExecStart = "${installed}/bin/omimed" + lib.optionalString (cfg.backend == "gnome") " --ibus";
        ExecReload = "${cfg.package}/bin/omime reload";
        Restart = "on-failure";
        RestartPreventExitStatus = 69;
        RestartSec = 1;
        KillMode = "mixed";
        TimeoutStopSec = 6;
        Environment = [
          "PATH=${servicePath}"
        ]
        ++ lib.optional cfg.rime.enable "OMIME_ENGINE_PATH=${cfg.rime.package}/share/oh-my-ime/engines";
      };

      Install.WantedBy = lib.optional cfg.autoStart "graphical-session.target";
    };

    # GTK 4 and Qt 6.8+ use the compositor's text-input; the empty GTK_IM_MODULE keeps
    # GTK 3 from loading an immodule that is not there.
    home.sessionVariables = lib.mkIf (cfg.sessionVariables && cfg.backend != "gnome") {
      GTK_IM_MODULE = "";
      QT_IM_MODULE = "fcitx";
      QT_IM_MODULES = "wayland;fcitx";
      XMODIFIERS = "@im=fcitx";
    };

    xdg.configFile = lib.mkIf (cfg.settings != { }) {
      "oh-my-ime/config.toml".source = toml.generate "config.toml" cfg.settings;
    };
  };
}
