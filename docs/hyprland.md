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
| Enable / disable an engine | `omime enable echo`, `omime disable echo` |
| Install / remove an engine | `omime install ~/src/my-engine`, `omime uninstall my-engine` |
| Reload the configuration | `omime reload` |

omimed starts with the engine you used last. The hotkeys and the key timeout live in
`~/.config/oh-my-ime/config.toml`; [`data/config.toml`](../data/config.toml) documents
every option.

### Waybar

`omime status --json` prints `text`, `tooltip` and `class`, which waybar understands:

```json
"custom/ime": {
    "exec": "omime status --json",
    "return-type": "json",
    "interval": 1,
    "on-click": "omime toggle",
    "on-click-right": "omime next"
}
```

## Which applications work

oh-my-ime talks to applications through the compositor's text-input protocol:

| Toolkit / application | Status |
|---|---|
| GTK 3 and 4, Qt 6, foot, kitty, alacritty, wezterm (Wayland) | ✅ text-input-v3 |
| Firefox (Wayland) | ✅ |
| Chromium and Electron apps (Wayland) | ✅ with `--enable-wayland-ime --wayland-text-input-version=3` on older versions |
| X11 applications under XWayland | 🚧 not yet (XIM bridge planned for Phase 2) |

## Troubleshooting

- **Nothing happens when I type.** Check `omime status`. If omimed isn't running, start it
  in a terminal with `OMIME_LOG=debug omimed` and read what it says.
- **`another input method is active on this seat`.** fcitx5, IBus or another input method
  is running; see step 2. omimed keeps retrying and takes over once the seat is free.
- **An application ignores the input method.** Make sure `GTK_IM_MODULE`/`QT_IM_MODULE`
  are not set to `fcitx` or `ibus` in its environment; those modules talk to fcitx5 or
  IBus directly and bypass the compositor.
- **Popups disappear over fullscreen windows, or sit slightly off with some fractional
  scaling setups.** Both are Hyprland limitations that affect every input method.
