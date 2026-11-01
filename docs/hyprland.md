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
| Next input method | `Ctrl+Shift+Space`, or `omime next` |
| Pick an input method | `omime switch fcitx5/shuangpin`, `omime switch sparkle`, `omime switch direct`, or the tray menu |
| See what to choose from | `omime list` (`*` marks the input method in use); `omime list --all` adds what you can enable (`-`) |
| See what is installed | `omime engines` (`*` marks enabled engines) |
| Find out what is wrong | `omime doctor` (exit status 1 when it finds a problem) |
| Enable / disable an engine | `omime enable echo`, `omime disable echo` |
| Configure an engine | `omime settings fcitx5`, or "Configure …" in the tray menu |
| Install / remove an engine | `omime install ~/src/my-engine`, `omime uninstall my-engine` |
| Reload the configuration | `omime reload`, `systemctl --user reload omimed` or SIGHUP |
| See or set options | `omime config`, `omime config apps.remember true` |
| What each application gets back | `omime apps`, `omime apps forget <app>` |
| The engine's menu (punctuation and the like) | `omime properties`, `omime activate <id>` |
| Watch the status | `omime status --watch --json` (one line per change, for status bars) |

omimed starts with the engine you used last. The hotkeys and the key timeout live in
`~/.config/oh-my-ime/config.toml`; [`data/config.toml`](../data/config.toml) documents
every option. If the file has a mistake, `omime reload` says what it is and omimed keeps
the configuration it had; options it does not know (misspelt, or from a newer version)
are left out, and `omime doctor` lists them.

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

omimed shows the input method in use in the tray of status bars (StatusNotifierItem):
waybar's `tray` module, KDE's system tray and others. The tooltip names it, such as
"双拼 (Fcitx 5)" for an input method inside Fcitx 5, and for Fcitx 5 and IBus the icon is
theirs too. A click switches between direct input and the last engine, a middle click to
the next input method, and the menu lists the input methods and direct input, then the
switches of the engine in use (such as Chinese punctuation) and its settings. To hide it,
set this in `~/.config/oh-my-ime/config.toml` and run `omime reload`:

```toml
[tray]
enabled = false
```

### Switcher

When you switch with the hotkeys, or with `omime switch`, `next` and `toggle` (from a key
binding of your own, say), the input methods show for a moment at the bottom of the
screen, the one now in use highlighted: the switcher, `omime-panel`, which omimed starts.
Switches from the tray menu and those that follow you between applications with
`[apps] remember` do not show it. It is a layer surface in the namespace
`omime-switcher`, for compositor rules; to turn it off:

```toml
[panel]
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
installed, and must not run on its own (step 2). oh-my-ime ships its manifest (the Nix
package and `scripts/install.sh` install it), so once `fcitx5` is installed, `omime
engines` offers it; enable it to have it in the cycle of `Ctrl+Shift+Space`, or switch
to it right away:

```sh
omime enable fcitx5
omime switch fcitx5
```

Without the shipped manifest, install it from a checkout of the repository with
`omime install /path/to/oh-my-ime/data/engines/fcitx5`, or write
`~/.local/share/oh-my-ime/engines/fcitx5/engine.toml` yourself:

```toml
[engine]
id = "fcitx5"
name = "Fcitx 5"
exec = ["fcitx5"]
protocol = "input-method-v2"
```

Fcitx 5 keeps its configuration in `~/.config/fcitx5` as usual. To change it, open its
configuration tool with `omime settings fcitx5`, or "Configure Fcitx 5…" in the tray menu
while Fcitx 5 is in use: omimed starts it on Fcitx 5's private bus, where Fcitx 5 is.
What Fcitx 5 offers to switch for the input method in use, such as Chinese punctuation,
full width and simplified or traditional Chinese, is in the tray menu too, and in
`omime properties` (`omime activate <id>` switches it).

The input methods you set up in Fcitx 5 are what you choose from, each on its own:
`omime list` shows `fcitx5/shuangpin`, `fcitx5/pinyin` and so on (its keyboard layouts are
not among them; direct input is omimed's), and `omime switch fcitx5/shuangpin`,
`Ctrl+Shift+Space` and the tray menu select them. omimed asks Fcitx 5 which input methods
it has and remembers them, so they are listed even before Fcitx 5 starts, and gives
Fcitx 5 the input method in use back when it starts again.

`omime enable fcitx5` enables everything in Fcitx 5's group. To choose input methods one
by one, enable them by id instead: `omime enable fcitx5/rime` enables Rime even if it is
not in Fcitx 5's group (omimed adds it there when you switch to it), and `omime disable
fcitx5/pinyin` leaves the other input methods of Fcitx 5 enabled. In `config.toml` that
is a list such as `engines = ["fcitx5/shuangpin", "fcitx5/rime", "sparkle"]`, in
switching order.

Switch with omimed's hotkeys, `omime switch` and the tray, not with Fcitx 5's trigger
key: omimed takes `Ctrl+Space` before any engine sees it. Hotkeys that Fcitx 5 handles
itself, such as `Ctrl+Shift` between its input methods, keep working, and omimed follows
them. Only the popups of Fcitx 5 are shown; its other windows cannot be.

### IBus as an engine

IBus (1.5.32 or later) runs inside oh-my-ime the same way, with all its engines
(ibus-libpinyin, ibus-rime, ibus-hangul, ...). `omime-ibus`, which oh-my-ime provides,
starts `ibus-daemon` and IBus's GTK panel as the Wayland input method
(`ibus-ui-gtk3 --enable-wayland-im`), so IBus draws its candidate window, and omimed
places it at the text cursor. IBus must be installed, and must not run on its own.
Like Fcitx 5, it is offered once it is installed (`omime install
/path/to/oh-my-ime/data/engines/ibus` without the shipped manifest):

```sh
omime enable ibus
omime switch ibus
```

The engines you chose in IBus (`ibus-setup`, or `preload-engines` in dconf) are what you
choose from, each on its own, like Fcitx 5's: `omime switch ibus/libpinyin`,
`Ctrl+Shift+Space` and the tray menu select them. Without a choice, omimed lists the
engine IBus uses, and each one you switch to with IBus's own tools.

The hosted daemon writes its address where IBus tools look for it, so they work as
usual: `ibus engine libpinyin` switches too, and `ibus-setup` configures engines,
starting each engine's own setup tool. These tools save to dconf from your session.
The hosted IBus itself runs on omimed's private session bus, where dconf's service is
not reachable: it reads the settings dconf holds when it starts, but cannot save
changes of its own. Open `ibus-setup` with `omime settings ibus` or "Configure IBus…"
in the tray menu, and omimed restarts IBus when you close it, so IBus reads what you
changed. Switch with omimed's hotkeys and `omime switch`; omimed takes `Ctrl+Space`
before IBus sees it. The switches of the IBus engine in use, such as libpinyin's
Chinese or English mode and full or half width, are in the tray menu and in
`omime properties`.

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
  omimed answers by the names of Fcitx and IBus too, unless one of them runs on that
  display, so applications started with `XMODIFIERS=@im=fcitx` or `@im=ibus` (as setups
  of those input methods leave them) find it as well.
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

Wayland applications type through the compositor's text-input without these variables,
which is best. Some cannot: WeChat, for one, reaches an input method only through its
built-in Fcitx5 module. A Wayland application with `QT_IM_MODULE=fcitx` or
`GTK_IM_MODULE=fcitx` reaches omimed through the Fcitx5 module, and on Hyprland and sway
omimed asks the compositor where its window is, so engine windows open at its text cursor
too (elsewhere they cannot be placed, Wayland does not tell clients where their windows
are). With `ibus` it reaches nothing yet. Prefer setting these variables only for the
applications that need them, in a wrapper script or a `.desktop` file:

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
  `GTK_IM_MODULE=fcitx`/`QT_IM_MODULE=fcitx`, which also suits Wayland applications that do
  not use the compositor's text-input, such as WeChat (see
  [X11 applications](#x11-applications)); `ibus` reaches nothing yet. `omime doctor` warns
  about input method variables in the environment.
- **Popups disappear over fullscreen windows, or sit slightly off with some fractional
  scaling setups.** Both are Hyprland limitations that affect every input method.
