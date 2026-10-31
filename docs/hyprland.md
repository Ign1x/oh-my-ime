# oh-my-ime on Hyprland

English | [简体中文](zh-CN/hyprland.md)

## 1. Install

### With Nix

On NixOS or any other distribution with Nix:

```sh
nix profile install github:Ign1x/oh-my-ime
```

On NixOS you can add it to your system configuration instead. With
`inputs.oh-my-ime.url = "github:Ign1x/oh-my-ime"` in your flake:

```nix
{ inputs, pkgs, ... }:
let
  oh-my-ime = inputs.oh-my-ime.packages.${pkgs.stdenv.hostPlatform.system}.default;
in
{
  environment.systemPackages = [ oh-my-ime ];
  # The systemd user unit; it starts omimed with sessions that run through systemd
  # (for example UWSM). Otherwise start omimed from Hyprland, see step 3.
  systemd.packages = [ oh-my-ime ];
  systemd.user.services.omimed.wantedBy = [ "graphical-session.target" ];
}
```

### From source

```sh
git clone https://github.com/Ign1x/oh-my-ime
cd oh-my-ime
scripts/install.sh # installs into ~/.local; PREFIX=/usr/local for a system install
```

This needs a Rust toolchain plus the wayland, libxkbcommon, dbus and fontconfig
development files, and `~/.local/bin` must be in the `PATH` Hyprland starts programs
with. On NixOS use the Nix package: binaries built this way do not find the graphics
libraries there.

## 2. Stop other input methods

A Wayland seat has room for one input method. If fcitx5 or IBus starts with your session,
remove it from your autostart (or `pkill fcitx5`, `ibus exit`). omimed tells you when the
seat is taken: `another input method is active on this seat`.

## 3. Start omimed with Hyprland

Lua configuration (`~/.config/hypr/hyprland.lua`, Hyprland 0.55 and later):

```lua
hl.on("hyprland.start", function()
    hl.exec_cmd("omimed")
end)

-- Let applications use text-input-v3 instead of an IM module.
hl.env("GTK_IM_MODULE", "")
hl.env("QT_IM_MODULE", "")

-- Optional: cycle engines from anywhere.
hl.bind("SUPER + space", hl.dsp.exec_cmd("omime next"))
```

Legacy `hyprland.conf`:

```ini
exec-once = omimed
env = GTK_IM_MODULE,
env = QT_IM_MODULE,
bind = SUPER, space, exec, omime next
```

Both snippets are in [`data/hyprland`](../data/hyprland). If your session uses systemd
(for example through UWSM), you can instead install
[`data/omimed.service`](../data/omimed.service) into `~/.config/systemd/user/` and run
`systemctl --user enable --now omimed`. The Nix package already contains the unit.

## 4. Use it

| What | How |
|---|---|
| Toggle between direct input and the last engine | `Ctrl+Space` (while a text field is focused) |
| Next engine | `Ctrl+Shift+Space`, or `omime next` |
| Pick an engine | `omime switch sparkle`, `omime switch direct` |
| See what is installed | `omime list` (`*` marks enabled engines) |
| Find out what is wrong | `omime doctor` (exit status 1 when it finds a problem) |
| Enable / disable an engine | `omime enable echo`, `omime disable echo` |
| Install / remove an engine | `omime install ~/src/my-engine`, `omime uninstall my-engine` |
| Reload the configuration | `omime reload` |
| Watch the status | `omime status --watch --json` (one line per change, for status bars) |

omimed starts with the engine you used last. The hotkeys and the key timeout live in
`~/.config/oh-my-ime/config.toml`; [`data/config.toml`](../data/config.toml) documents
every option.

### One engine per application

By default one engine serves every application. To give each application the engine
you last used in it, add this to `config.toml` and run `omime reload`:

```toml
[apps]
remember = true
```

Switching while an application has focus then only affects that application, and it
gets its engine back whenever you return to it, also after a restart. Applications you
have not switched in yet keep the current engine. Launchers and other layer-shell
surfaces are not windows: switching in them counts for the window below.

### Tray

omimed shows the current engine in the tray of status bars (StatusNotifierItem):
waybar's `tray` module, KDE's system tray and others. The tooltip names the engine and,
for Fcitx 5 and IBus, the input method they use, such as "Fcitx 5 · 双拼"; the icon is
theirs too. A click switches between direct input and the last engine, a middle click to
the next engine, and the menu lists the enabled engines and direct input. To hide it, set
this in `~/.config/oh-my-ime/config.toml` and restart omimed:

```toml
[tray]
enabled = false
```

### Waybar

`omime status --watch --json` prints the status and then one line per change, in the
format waybar understands (`text`, `tooltip` and `class`), so waybar follows it without
polling:

```json
"custom/ime": {
    "exec": "omime status --watch --json",
    "return-type": "json",
    "on-click": "omime toggle",
    "on-click-right": "omime next"
}
```

### Fcitx 5 as an engine

Fcitx 5 can run inside oh-my-ime as one engine, unmodified and with all its input methods
and addons: omimed starts it, gives it a private session bus and an input method of its
own, and shows its candidate window at the text cursor like any engine's. `fcitx5` must be
installed, and must not run on its own (step 2). Install the manifest the repository
ships (the Nix package has it at `share/oh-my-ime/extra-engines/fcitx5`):

```sh
omime install /path/to/oh-my-ime/data/engines/fcitx5
omime switch fcitx5
```

Or write `~/.local/share/oh-my-ime/engines/fcitx5/engine.toml` yourself:

```toml
[engine]
id = "fcitx5"
name = "Fcitx 5"
exec = ["fcitx5"]
protocol = "input-method-v2"
```

Fcitx 5 keeps its configuration in `~/.config/fcitx5` as usual. Its configuration tool
talks to it over D-Bus, so point it at the private bus:

```sh
DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/oh-my-ime/fcitx5/bus fcitx5-configtool
```

Switch with omimed's hotkeys and `omime switch`, not with Fcitx 5's trigger key: omimed
takes `Ctrl+Space` before any engine sees it. Hotkeys that Fcitx 5 handles itself, such as
`Ctrl+Shift` between its input methods, keep working. Only the popups of Fcitx 5 are
shown; its other windows cannot be.

### IBus as an engine

IBus (1.5.32 or later) runs inside oh-my-ime the same way, with all its engines
(ibus-libpinyin, ibus-rime, ibus-hangul, ...). `omime-ibus`, which oh-my-ime provides,
starts `ibus-daemon` and IBus's GTK panel as the Wayland input method
(`ibus-ui-gtk3 --enable-wayland-im`), so IBus draws its candidate window, and omimed
places it at the text cursor. IBus must be installed, and must not run on its own.
Install the manifest the repository ships (the Nix package has it at
`share/oh-my-ime/extra-engines/ibus`):

```sh
omime install /path/to/oh-my-ime/data/engines/ibus
omime switch ibus
```

The hosted daemon writes its address where IBus tools look for it, so they work as
usual: pick an engine with `ibus engine libpinyin` or `ibus-setup`, and configure
engines from `ibus-setup`, which starts each engine's own setup tool. These tools save
to dconf from your session. The hosted IBus itself runs on omimed's private session bus, where
dconf's service is not reachable: it reads the settings dconf holds when it starts,
but cannot save changes of its own, and sees what the tools changed the next time it
starts (`omime switch` to another engine and back). Switch with omimed's hotkeys and
`omime switch`; omimed takes `Ctrl+Space` before IBus sees it.

## Which applications work

oh-my-ime reaches Wayland applications through the compositor's text-input protocol, and
X11 applications through XIM or the Fcitx5 GTK and Qt modules:

| Toolkit / application | Status |
|---|---|
| GTK 3 and 4, Qt 6, foot, kitty, alacritty, wezterm (Wayland) | ✅ text-input-v3 |
| Firefox (Wayland) | ✅ |
| Chromium and Electron apps (Wayland) | ✅ with `--enable-wayland-ime --wayland-text-input-version=3` on older versions |
| X11 applications under XWayland, through XIM | ✅ `XMODIFIERS=@im=omime` |
| X11 GTK and Qt applications under XWayland, through the Fcitx5 modules | ✅ `GTK_IM_MODULE=fcitx`, `QT_IM_MODULE=fcitx` |

### X11 applications

XWayland applications never see `text-input`, so they reach omimed another way. omimed
serves both frontends itself; fcitx5 must not run.

- **XIM.** omimed is the XIM server `omime` on the X display in `$DISPLAY`, when
  `DISPLAY` is set at startup. Applications started with `XMODIFIERS=@im=omime` use it:
  xterm, urxvt, Emacs (X11), Java (AWT/Swing), Wine, and GTK with `GTK_IM_MODULE=xim`.
  Only X11 applications read `XMODIFIERS`, so it is safe to set it for the whole session.
- **Fcitx5 modules.** Applications started with `GTK_IM_MODULE=fcitx` or
  `QT_IM_MODULE=fcitx` talk to omimed through the Fcitx5 module protocol. The modules must
  be installed (`fcitx5-gtk`, `fcitx5-qt`).

With the systemd user unit, `DISPLAY` must be in the user manager's environment, or omimed
starts without XIM. In Hyprland, add this to `hyprland.conf` (with the Lua configuration,
pass the same command to `hl.exec_cmd`):

```ini
exec-once = dbus-update-activation-environment --systemd DISPLAY WAYLAND_DISPLAY
```

If omimed already runs, run `systemctl --user import-environment DISPLAY WAYLAND_DISPLAY`
and restart it once.

Do not set `GTK_IM_MODULE`/`QT_IM_MODULE` for the whole session. A Wayland GTK or Qt
application with `GTK_IM_MODULE=fcitx` reaches omimed and can type, but omimed cannot
place engine windows for it: Wayland does not tell clients where their windows are, so the
candidates stay invisible. With `ibus` it reaches nothing. Set these variables only for
X11 applications, in a wrapper script or a `.desktop` file:

```sh
#!/bin/sh
GTK_IM_MODULE=fcitx exec "$@"
```

Some X11 applications leave drawing the preedit to the input method; xterm is one. omimed
does not draw text itself, so in these applications what you are composing only shows
where the engine draws it in its own window (Sparkle does; Echo has no window), until it
is committed.

Engine windows for X11 applications are layer surfaces in the namespace `omime-popup`.
Hyprland animates layer surfaces. To turn that off for them, in the Lua configuration:

```lua
hl.layer_rule({
    match = { namespace = "omime-popup" },
    no_anim = true,
})
```

In legacy `hyprland.conf`:

```ini
layerrule = no_anim on, match:namespace omime-popup
```

## Troubleshooting

- **Something does not work.** Run `omime doctor` first: it checks the daemon, the seats,
  `config.toml`, the installed engines and the environment, one line per finding (`✓`
  fine, `!` warning, `✗` problem).
- **Nothing happens when I type.** Check `omime status`. If omimed isn't running, start it
  in a terminal with `OMIME_LOG=debug omimed` and read what it says.
- **`another input method is active on this seat`.** fcitx5, IBus or another input method
  is running; see step 2. omimed keeps retrying and takes over once the seat is free.
- **An application ignores the input method.** An X11 application needs
  `XMODIFIERS=@im=omime`, or, with the Fcitx5 modules installed,
  `GTK_IM_MODULE=fcitx`/`QT_IM_MODULE=fcitx`. Do not set the module variables for the
  whole session: a Wayland application then types through omimed without engine windows
  (see [X11 applications](#x11-applications)), and `ibus` reaches nothing. `omime doctor`
  warns about input method variables in the environment.
- **Popups disappear over fullscreen windows, or sit slightly off with some fractional
  scaling setups.** Both are Hyprland limitations that affect every input method.
