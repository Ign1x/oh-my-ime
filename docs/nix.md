# NixOS and Home Manager modules

English | [简体中文](zh-CN/nix.md)

The flake exports two modules that turn the package into declarative configuration:

- `nixosModules.default` configures the system: the package, the systemd user unit,
  the input environment, the Qt input modules, and Fcitx 5 or IBus as engines on
  the unit's `PATH`.
- `homeManagerModules.default` configures the user: the package, the user unit,
  `home.sessionVariables`, and `~/.config/oh-my-ime/config.toml` generated from
  Nix, on NixOS or any other distribution.

The manual steps they replace are in [the Hyprland guide](hyprland.md); use one of
the modules or the manual setup, not both. Do not enable the NixOS module and the
Home Manager module together either: both ship an `omimed.service`.

## NixOS

Add the flake and import the module:

```nix
{
  inputs.oh-my-ime.url = "github:Ign1x/oh-my-ime";

  outputs =
    { nixpkgs, oh-my-ime, ... }:
    {
      nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          oh-my-ime.nixosModules.default
          (
            { pkgs, ... }:
            {
              programs.oh-my-ime = {
                enable = true;
                fcitx5 = {
                  enable = true;
                  # Fcitx 5's Chinese input methods, including Xiaohe shuangpin.
                  addons = [ pkgs.qt6Packages.fcitx5-chinese-addons ];
                };
              };
            }
          )
        ];
      };
    };
}
```

`fcitx5.enable` builds
`qt6Packages.fcitx5-with-addons.override { addons = ...; }` and puts it on the
`omimed` unit's `PATH`, where the Fcitx 5 engine looks for `fcitx5`. For IBus, use
`ibus.enable` with `ibus.engines = [ pkgs.ibus-engines.libpinyin ]`, which builds
`ibus-with-plugins`. Both can be enabled: each becomes an engine of its own. They
are installed for omimed, not on the system `PATH`: installed globally, Fcitx 5's
autostart entry would start it as the desktop's input method, in omimed's place. The
unit's `PATH` keeps the user's profiles after them, so engines whose manifests name a
program in a profile still find it.

`rime.enable` offers oh-my-ime's own [Rime engine](../engines/rime) (Xiaohe shuangpin
first), with librime and nixpkgs' `rime-data`: the unit finds it through
`OMIME_ENGINE_PATH`. Engines that other packages install in `share/oh-my-ime/engines`
are found through the system profile, which the module links that directory into.

`qtModules` (default `true`) installs `qt6Packages.fcitx5-qt` and
`libsForQt5.fcitx5-qt`, which Qt 5 and Qt applications on X11 need to reach omimed
through `QT_IM_MODULE=fcitx`. `sessionVariables` (default `true`) sets:

```sh
GTK_IM_MODULE=""
QT_IM_MODULE="fcitx"
QT_IM_MODULES="wayland;fcitx"
XMODIFIERS="@im=fcitx"
```

Then log out and back in (or switch to a new session), start the unit if your
session does not start `graphical-session.target` itself, and check the result:

```sh
systemctl --user start omimed
omime doctor
omime list --all
```

The NixOS module does not write `config.toml`; select engines with `omime setup`,
`omime enable fcitx5` (or `omime enable ibus`), and the settings window. Use the
Home Manager module's `settings` option, or write the file yourself, for a
declarative configuration.

`programs.oh-my-ime` conflicts with `i18n.inputMethod.enable`: a Wayland seat has
room for one input method. The module fails evaluation if both are enabled.

## Home Manager

Import the module and enable it:

```nix
{ inputs, pkgs, ... }:
{
  imports = [ inputs.oh-my-ime.homeManagerModules.default ];

  programs.oh-my-ime = {
    enable = true;
    fcitx5 = {
      enable = true;
      addons = [ pkgs.qt6Packages.fcitx5-chinese-addons ];
    };

    settings = {
      engines = [
        "fcitx5/shuangpin"
        "sparkle"
      ];
      initial_input_method = "fcitx5/shuangpin";
    };
  };
}
```

`settings` is the content of `~/.config/oh-my-ime/config.toml`, the file described
in [the configuration](../data/config.toml): `engines` lists the enabled engines
and input methods in switching order, and `initial_input_method` is the first
login's choice. `"fcitx5/shuangpin"` is the Xiaohe shuangpin input method inside
the hosted Fcitx 5, so it needs `fcitx5.enable` and
`qt6Packages.fcitx5-chinese-addons`. The bundled `sparkle` engine needs nothing
else.

The option defaults to `{ }`, which writes no file and leaves omimed's defaults.
When it is set, the file is a symlink into the Nix store, which is read-only:
changes made with `omime config`, `omime enable`, or the settings window do not
persist, and the next `home-manager switch` restores the generated file. Make those
changes in `settings` instead, then run `home-manager switch` and `omime reload`.

`sessionVariables` (default `true`) sets the same four variables in
`home.sessionVariables`. `fcitx5.enable`, `ibus.enable` and `rime.enable` work as in
the NixOS module: the frameworks are on the unit's `PATH`, not in `home.packages`,
and the Rime engine is on its engine path.

On NixOS, Home Manager can import the module through
`home-manager.users.<name>.imports`; the NixOS module is not needed then, but the
session variables come from Home Manager and apply only to the user. The Qt input
modules from `qtModules` are not installed either: add
`pkgs.qt6Packages.fcitx5-qt` and `pkgs.libsForQt5.fcitx5-qt` to `home.packages` for
Qt 5 and X11 Qt applications.

## With sessions that do not use systemd

Both modules add the user unit, which starts `omimed` with
`graphical-session.target`. If the compositor does not start that target (for
example a bare Hyprland launched from a TTY without UWSM), start the daemon from
the compositor instead, as shown in
[step 3 of the Hyprland guide](hyprland.md#3-start-omimed-with-hyprland). Keep the
module: it still installs the package, the frameworks and the environment.
