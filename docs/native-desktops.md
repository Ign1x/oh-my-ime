# Native KDE and GNOME input

English | [简体中文](zh-CN/native-desktops.md)

These adapters are available in the development source. Published 0.5.0 binaries
predate them. Install a build containing the adapters and Rime, then select the
appropriate route below. [Acceptance](compatibility.md) records the tested scope.

## KDE Plasma Wayland

```sh
omime setup --desktop kde --engine rime --dry-run
omime setup --desktop kde --engine rime
```

Log out and back in. Open **System Settings → Keyboard → Virtual Keyboard** and
select **oh-my-ime** (the category name varies by Plasma version). KWin launches
omimed and passes its privileged input-method connection. Opening a terminal and
running a second omimed does not provide that connection.

The adapter uses `zwp_input_method_v1` for keyboard/text and `zwp_input_panel_v1`
for candidate surfaces on the same connection. The regular Wayland engine UI and
hosted framework routes remain available. KWin's v1 protocol does not provide an
application ID; per-app preferences depend on another supported identity source.

Setup installs a user desktop entry in
`$XDG_DATA_HOME/applications/io.github.ign1x.OhMyIme.InputMethod.desktop` and updates
the requested engine configuration. It does not change Plasma's selected keyboard.
If migrating from a standalone omimed user service, disable that service before
selecting the KWin entry so there is only one daemon for the session.

## GNOME Wayland

```sh
omime setup --desktop gnome --engine rime --dry-run
omime setup --desktop gnome --engine rime
```

Log out and back in. In **Settings → Keyboard → Input Sources**, add **oh-my-ime**
under Chinese, then select it with GNOME's input-source switcher. System IBus must
be installed and running; the platform connects to it using `omimed --ibus`.

Setup installs a component at `$XDG_DATA_HOME/ibus/component/oh-my-ime.xml` and a
GNOME autostart entry. Dynamic registration also makes the component discoverable
when distributions do not scan user IBus component directories. IBus continues to
own application input, including its toolkit routes. Existing IBus input sources
can be selected normally. Keep the distribution's IBus environment settings.

Rime sends inline preedit and semantic candidates to IBus; GNOME Shell draws the
candidate window and handles page/click callbacks. Rime runs without an engine
Wayland window in this mode. Its Slint popup appearance settings therefore do not
change the Shell candidate window. Rime and Echo are supported; hosted Fcitx5/IBus
frameworks and engines needing their own Wayland popup require a different route.

`omime settings` and CLI engine switching use the same daemon control interface.
Application identity depends on the client's IBus FocusInId metadata; GNOME-mediated
contexts may expose a generic identity. Input-source selection remains a GNOME
setting, separate from selecting a Rime schema inside oh-my-ime.

## Lifecycle and undo

KDE/GNOME setup rejects `--start` and `--take-over`: startup and source selection
belong to the desktop. `--config` remains specific to Hyprland/sway. Setup retains
a reversible journal; `omime setup --undo` restores managed registration/configuration
files. Select another desktop input source before removing the active registration.
When switching between desktop setup types, undo the previous journal first.

`omime doctor` distinguishes IBus connection from engine selection. A connected
adapter alone does not mean oh-my-ime is the selected input source. Runtime output
is tied to the active context; focus loss retires pending text and candidates.
Reported password/PIN/hidden/private fields bypass the engine. Applications that
omit sensitive-field metadata cannot receive that guarantee from the adapter.

For NixOS or Home Manager, use `programs.oh-my-ime.backend = "kde"` or `"gnome"`
with the module from this source tree. KDE is launched by KWin; GNOME uses `--ibus`.
The NixOS GNOME option enables system IBus. Home Manager expects it to be provided
by the distribution. Native modes do not install the Wayland backend's global
Fcitx environment. See [Nix](nix.md) for package/engine options.

## Reproduce acceptance

Build once with `nix develop .#e2e --command cargo build --workspace --locked`.
Provide installed desktop executables explicitly if they are absent from PATH:

```sh
nix develop .#e2e --command python3 tests/desktop/native-kwin.py \
  --kwin /path/to/kwin_wayland --wire-log --out /tmp/omime-kwin-native
nix develop .#e2e --command python3 tests/desktop/native-ibus.py \
  --monitor --out /tmp/omime-ibus-native
nix develop .#e2e --command python3 tests/desktop/native-gnome.py \
  --shell /path/to/gnome-shell --out /tmp/omime-gnome-native
```

Each run creates its own XDG directories, bus and processes. Existing sessions are
not changed. The KWin test uses a virtual output, and the Shell test requires a
headless-capable GNOME build. Screenshot capture on KWin 6.8.3's virtual renderer
crashed the compositor during testing, so candidate geometry is not visually
accepted by that run. Physical displays, mixed DPI, fullscreen and the complete
GTK/Qt/Electron application matrix require further acceptance.
