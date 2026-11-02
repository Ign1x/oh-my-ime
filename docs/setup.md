# First-run setup on Hyprland

English | [简体中文](zh-CN/setup.md)

`omime setup` connects an installed oh-my-ime to a Hyprland session. It configures
application input routes and autostart, selects a first input method when requested,
and keeps enough information to undo its changes.

Install the platform and the input method first. For Chinese input through Fcitx5,
install Fcitx5 with its Pinyin addon; Qt applications also need the matching Fcitx5
Qt input plugin. The bundled Sparkle demo is available without a hosted framework.
See [installation and application requirements](hyprland.md).

## Configure and start

Preview the changes:

```sh
omime setup --engine fcitx5/pinyin --take-over --dry-run
```

Apply them and start input in this session:

```sh
omime setup --engine fcitx5/pinyin --take-over --start
```

For IBus with libpinyin installed, use `--engine ibus/libpinyin`; for the bundled
emoji engine, use `--engine sparkle`. Without `--engine`, setup
preserves the current input-method configuration. Without `--start`, it prepares
the next login and leaves the current session running as it is.

`--take-over` suspends recognized Fcitx5/IBus startup commands in the selected
Hyprland file. Together with `--start`, it also requests the competing framework on
this session's D-Bus connection to exit. It does not stop every process with that
name. If another method still owns a seat, setup reports that input is not ready.
Starting input waits for the selected framework to accept the input method. Its
first keys wait for activation in the focused field; missing addons are reported.

Log out and back into Hyprland after setup so the compositor and applications
inherit the input environment. Then run `omime doctor`. Existing applications may
need to be restarted before they use the new GTK/Qt/XIM route.

## Configuration location

Setup uses `$XDG_CONFIG_HOME/hypr/hyprland.lua` if it exists, otherwise an existing
`hyprland.conf`. With neither file present, it creates the Lua configuration used
by recent Hyprland releases. `$XDG_CONFIG_HOME` defaults to `~/.config`.

If Hyprland is started with a custom configuration, select that same file:

```sh
omime setup --config ~/.config/hypr/my-desktop.lua --engine sparkle
```

Both Lua and legacy `.conf` files are supported. Setup adds a marked include to
the main file and creates its own snippet under `$XDG_CONFIG_HOME/oh-my-ime/`.
The snippet sets `GTK_IM_MODULE`, `QT_IM_MODULE`, `QT_IM_MODULES`, `XMODIFIERS` and
starts the installed daemon. Unrelated settings, key bindings and comments stay.

Startup commands inside custom shell scripts, other included files or arbitrary
Lua expressions need to be adjusted in their owning configuration. Compound
commands that combine an input method with other programs are not rewritten.
Nix/Home Manager and other dotfile managers may make the main file a symlink;
manage its include declaratively using the [manual setup](hyprland.md#3-start-omimed-with-hyprland).
Setup reports a symlink instead of replacing it.

## First input method and remembered choices

`--engine` sets `initial_input_method` in `oh-my-ime/config.toml` and enables the
engine if needed. The daemon uses it only when no saved choice exists. After a
user switches methods, later logins restore that choice, including direct input.

With `--start --engine ID`, setup also selects that method in the current daemon.
Repeating `omime setup --start` without `--engine` preserves the current choice.

## Undo and recovery

```sh
omime setup --undo --dry-run
omime setup --undo
```

Undo removes the managed include and snippet and restores the settings and startup
lines it changed. Log out and back in to use the restored startup and environment.
The current session keeps its input service until then.

Edits made outside managed sections are retained, including settings added after
setup. If a managed section itself was edited, undo reports the conflict before
changing any file. Originals and installed contents are in
`$XDG_STATE_HOME/oh-my-ime/setup.toml` (by default
`~/.local/state/oh-my-ime/setup.toml`); keep this file for recovery.

Re-running setup updates its own configuration without duplicating startup entries.
A failed write rolls back the files already changed. After an interrupted run,
`omime setup --undo` uses the journal to recover the configuration before another
setup is attempted.
