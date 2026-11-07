# First-run setup on Hyprland

English | [简体中文](zh-CN/setup.md)

`omime setup` connects an installed oh-my-ime to a Hyprland session. It configures
application input routes and autostart, selects a first input method when requested,
and keeps enough information to undo its changes.

Install the platform and the input method first. For Chinese input through Fcitx5,
install Fcitx5 with its Pinyin addon; Qt applications also need the matching Fcitx5
Qt input plugin. The bundled Echo and Sparkle demos are offered but disabled by
default; Sparkle needs no hosted framework.
See [installation and application requirements](hyprland.md).

## Configure and start

Run setup from a Hyprland session. Before changing any files or stopping Fcitx5 or
IBus, it checks for `zwp_input_method_manager_v2` and
`zwp_virtual_keyboard_manager_v1`. KDE Plasma and GNOME are not supported yet;
setup names the missing protocols and refuses, including with `--take-over`.
An unreachable or unresponsive display also prevents applying changes.
`--dry-run` prints the warning first and still previews, explaining that applying
would be refused. An inherited `WAYLAND_SOCKET` takes precedence over
`WAYLAND_DISPLAY` and cannot be safely probed for reuse by omimed, so setup refuses
it too. Run setup from a Hyprland terminal without an inherited Wayland socket.
On a text console with neither Wayland variable set, setup can prepare
the next Hyprland login; `--start` still requires a Wayland session.

Preview the changes:

```sh
omime setup --engine fcitx5/pinyin --take-over --dry-run
```

Apply them and start input in this session:

```sh
omime setup --engine fcitx5/pinyin --take-over --start
```

For IBus with libpinyin installed, use `--engine ibus/libpinyin`; for the bundled
emoji engine, use `--engine sparkle`. Setup adds the selected engine to `engines`
and sets `initial_input_method`, so Sparkle works even with no engines enabled.
You can also enable demos with `omime enable echo` / `omime enable sparkle`, or
**Add input method…** in the settings app. Without `--engine`, setup
preserves the current input-method configuration. Without `--start`, it prepares
the next login and leaves the current session running as it is.

`--take-over` suspends recognized Fcitx5/IBus startup commands in the selected
Hyprland file. Together with `--start`, it also requests the competing framework on
this session's D-Bus connection to exit. It does not stop every process with that
name. If another method still owns a seat, setup reports that input is not ready.
Starting input waits for the selected framework to accept the input method. Its
first keys wait for activation in the focused field; missing addons are reported.
If startup fails after a takeover, the error lists the frameworks it stopped and
their restart commands: `fcitx5 -d` or `ibus-daemon -drx`. Setup first stops the
omimed instance it launched, releasing its input seat and bus names. Stop any
other omimed instance before restoring the old framework
(`systemctl --user stop omimed` for the service). If only the application launch-environment update fails,
input stays running; the error explains how to retry that update.

`--start` also updates D-Bus activation and the systemd user manager's environment
on this session bus. Applications started through them afterwards inherit the input
settings, including launchers managed by UWSM. If the user manager names another
Wayland display, setup leaves its environment alone and reports the mismatch.
Already running applications keep their original environment and need restarting.

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

The same input settings are written to
`$XDG_CONFIG_HOME/environment.d/90-oh-my-ime.conf` for systemd application launches
and a marked block in `$XDG_CONFIG_HOME/uwsm/env-hyprland` for UWSM. These are part
of the same setup/undo journal. Existing UWSM settings and unrelated later edits
are kept. Files managed by a dotfile manager are handled declaratively instead.
This configures the session once; individual applications use their normal launchers.

Startup commands inside custom shell scripts, other included files or arbitrary
Lua expressions need to be adjusted in their owning configuration. Compound
commands that combine an input method with other programs are not rewritten.
Nix/Home Manager and other dotfile managers may make the main file a symlink;
manage its include declaratively using the [manual setup](hyprland.md#3-start-omimed-with-hyprland).
Setup reports a symlink instead of replacing it.

## Check an application

If only one application cannot type, use its process ID from a process monitor:

```sh
omime doctor --app 12345
```

The report reads that process's input-related launch variables and known input
plugins in its loaded libraries. It can identify a Qt Compose fallback, or settings
that differ from the systemd manager because the application was opened before
setup. It also checks the daemon and installed engines. It reads no document text,
keystrokes, window titles or unrelated environment values; plugin detection does
not prove that an application's input field sends keys to the daemon.

`omime doctor` without `--app` checks the terminal's environment and the systemd
launch environment. A service may have engine manifests and program paths absent
from a terminal; the daemon's engine list is included to avoid false missing-engine
warnings.

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

## Troubleshooting

If setup or `omime doctor` reports an unsupported desktop, keep using its existing
input method and run setup after logging into Hyprland. `omime doctor` checks the
protocols when omimed is absent. Starting omimed on an unsupported compositor logs
the missing protocols, sends one desktop notification (unless
`[notifications] enabled = false`) and exits with status 69. The systemd unit does
not restart it for this status. The protocol check begins before session-bus
services start and keeps reading Wayland events while they initialize. Even a
late unsupported reply with stalled bus authentication leads to this exit.
Notification delivery waits at most two seconds.
`omime setup --undo` remains available on any desktop.
