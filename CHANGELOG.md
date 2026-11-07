# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `omime choose` opens a keyboard and pointer input-method chooser. Browse without
  changing the active method, confirm with Enter or a click, or cancel with Escape.
  Lists scroll, follow enabled-method changes, and preserve the original app's
  remembered choice. The modal overlay releases keyboard focus when it closes;
  existing immediate switches and their passive notifications keep their behavior.
- Optional per-application input-mode memory: enable `apps.remember` and
  `apps.remember_mode` to keep Rime Chinese in one application and Latin letters in
  another, including after restarting. The mode is restored before queued keys;
  older engines retain their existing behavior. Settings exposes the option and
  restoration errors, which `omime doctor` also reports.
- Engine protocol 1.2 negotiates mode capabilities and confirms actual modes for
  each context. Shared observation revisions prevent delayed key replies from
  undoing newer menu changes; the SDK suppresses these extensions on older daemons.
- Rime's confirmed mode appears in ordinary tray icons/tooltips and settings;
  `omime status --json` and `--watch --json` add the stable `input_mode` field.
- Reproducible, isolated desktop/application probes and a versioned compatibility
  baseline for Firefox, foot and XIM/xterm. KWin's privileged IM v1 connection is
  probed separately; KDE/GNOME input backends remain future work. See
  [application acceptance](docs/compatibility.md).

### Changed

- The minimum supported Rust version is 1.92, matching the locked dependencies.
- Echo and Sparkle still ship with every installation, but are offered rather than
  enabled by default. Add them to the switching cycle with `omime enable echo`,
  `omime enable sparkle`, or the settings app's **Add input method…** dialog.
  `omime switch echo` or `omime switch sparkle` tries one without adding it to the cycle.
  `omime setup --engine sparkle` enables Sparkle explicitly; `omime doctor` now
  lists available engines and how to enable one when none is enabled.

### Fixed

- Native engines get time to save state on shutdown, restart, settings reload and
  uninstall, with bounded SIGTERM/SIGKILL fallback for hung processes. Rime now
  finalizes its dictionary on SIGTERM and window failures, preserving the last
  learned phrase and exiting cleanly.
- Rime and Sparkle support symlinked data directories while restricting their target
  directories to `0700` before writes.
- Desktop notifications stop opening settings after they are resolved, dismissed or
  expired. A recurring problem gets a new notification after it is resolved.
  Notification delivery recovers after a stalled server call.
- Configuration, saved state and the input-method catalog retain their file permissions
  when updated and use `0600` for new files. Atomic saves clean up temporary files after
  errors and keep the previous contents, including Sparkle's recent emoji. Sparkle's
  history is restricted to `0600`; its state directory and Rime's user-data directory
  are restricted to `0700`, including existing directories.
- Native engine menus consider at most 256 entries per update, keep the first
  occurrence of each ID, and discard cyclic submenus. Malformed menus no longer
  expand into repeated entries or stall the daemon. Tray clicks retain their
  engine identity, so a delayed click after switching engines is ignored.
- Editing a malformed configuration section through settings or `omime config`
  reports an error and leaves the file unchanged without stopping omimed. Inline
  and dotted tables remain editable with comments preserved. Hotkeys containing
  control characters are rejected so NUL in config.toml cannot crash startup or
  reload.
- XIM clients that exhaust input-context or input-method IDs no longer terminate
  omimed. After 65,534 context allocations on a connection, CreateIc receives
  `XIM_ERROR` (`BadAlloc`) and `XCreateIC` returns NULL; the connection and its existing
  contexts keep working. New contexts need a new XIM connection, for example by
  restarting the application. A protocol-handler panic drops only that connection;
  repeated Open/Close can trigger this, but libX11 opens one input method per connection.
- Fcitx5-module clients are limited to 1,024 input contexts per connection across both
  bus names, with 8,192 in total, and 4,000 bytes of surrounding text per context.
  Destroying contexts or disconnecting releases their quota, including creations still
  in flight when the client disconnects. Moving outside the retained text clears it in
  engines and blocks deletion until fresh text arrives. Extreme cursor rectangles and
  invalid scales no longer overflow popup placement or stop the daemon.
- Long engine commits reach Wayland applications in ordered chunks of at most
  4,000 UTF-8 bytes before following keys, including commits from hosted input
  methods. Oversized preedits are truncated with valid cursor offsets, and engine
  output is bounded to protect the shared compositor connection. Queue limits also
  cover hosted forwarding and completed keys; accepted presses retain room for
  their releases when the queue fills.
- Setup checks the compositor's input protocols before changing files or taking over
  Fcitx5/IBus, preserving input on unsupported desktops such as KDE Plasma and GNOME.
  Inherited Wayland sockets are refused when they cannot be safely verified.
  Dry runs warn and still preview; failed takeovers stop the daemon they launched
  before showing framework restart commands. Launch-environment update failures
  preserve working input.
  Doctor explains missing protocols when omimed is absent. omimed notifies once and
  exits with status 69 on unsupported compositors, even when session-bus authentication
  stalls and the compositor replies late, without a systemd restart loop.
  Protocol checks work without a libwayland runtime search path. Slow compositor
  startup keeps the connection and requested engine, and early seat names survive
  session initialization.
- Reselecting the active Rime schema preserves its Chinese/Latin mode and ongoing
  composition. This also prevents a focus change from unexpectedly resetting Rime
  to Chinese when application input-method memory is enabled without mode memory.

## [0.4.0] - 2026-11-06

### Added

- Tag-triggered binary Release workflow for native x86_64 and ARM64 Linux builds.
  Platform and Rime archives are tested after installation with Nix; releases include
  SHA-256 checksums and a minimal flake whose NixOS/Home Manager modules download
  prebuilt packages. Manual workflow runs validate assets without publishing.
- Shared localization for settings, tray menus, notifications, the switcher, CLI
  help, diagnostics, setup and engine management, plus Rime settings and Sparkle's
  operation hint. Simplified Chinese joins the English source text in an embedded
  PO catalog. Language selection honors nonempty `LC_ALL`, `LC_MESSAGES`, `LANG`
  and `LANGUAGE` preferences; unsupported messages fall back to English. CI checks
  translation coverage and placeholders. Command IDs, JSON keys and configuration
  markers keep their original spelling across languages.
- Session setup now configures systemd and UWSM application launches as well as
  Hyprland, with preview, preservation of existing settings and journaled undo.
  `omime setup --start` updates the current D-Bus activation and user-manager
  environments after input is ready; a manager for a different Wayland display is
  left alone. Existing applications still need restarting.
- `omime doctor --app PID` checks a running application's input environment and
  known loaded input plugins. It identifies Compose fallback and compares the
  application's settings with systemd's launch environment, without reading typed
  text or printing unrelated environment variables. `doctor` also uses the daemon's
  engine list when its service has paths absent from the terminal.
- Rime settings (`omime settings rime`, or **Configure Rime…** in the tray): choose
  schemas, candidates per page, Shift behavior, horizontal or vertical candidates,
  system/light/dark colors and font size. The window is in English and Chinese;
  schema names use simplified characters except in traditional Chinese locales.
  Rime's settings preserve other customizations in `default.custom.yaml`, and
  appearance is stored in `rime.toml`. Files are replaced through temporary files;
  a failed save keeps the window open for retry. Closing it restarts Rime to deploy
  the saved changes. The Nix package and installer include the settings command.
- Settings: `omime-settings`, a window for what config.toml and `omime` used to be
  needed for. It lists your input methods in switching order, to reorder, remove and
  open the settings of, and adds more from what Fcitx 5, IBus and the installed engines
  offer, grouped by language and searchable. It records hotkeys as you press them, sets
  or forgets the input method of each application, turns the tray, the switcher and
  notifications on or off, sets the key timeout, and shows what omimed reports about the
  session. It is
  in English and Chinese, light or dark as the desktop prefers, and works only through
  the control API: it follows omimed as it stops and starts, and omimed writes
  config.toml, keeping your comments. `omime settings` opens it, as do "Settings…" at the
  end of the tray menu and the desktop entry "oh-my-ime Settings".
- NixOS and Home Manager modules (`nixosModules.default`, `homeManagerModules.default`,
  [docs/nix.md](docs/nix.md)): `programs.oh-my-ime` installs the package and its user
  unit, sets the session variables and the Qt input modules, and hosts Fcitx 5 with
  addons, IBus with engines and the Rime engine (`fcitx5`, `ibus`, `rime`). The
  frameworks are only on the unit's `PATH`, so they cannot start as the desktop's
  input method. Home Manager's `settings` writes `config.toml`. `nix flake check`
  evaluates both.
- A Rime engine, `omime-rime` (`engines/rime`): Rime's schemas are its input methods,
  Xiaohe shuangpin (`rime/double_pinyin_flypy`) first and then pinyin, with simplified
  characters and Shift switching between Chinese and English letters, which it sets up
  the first time in `~/.local/share/oh-my-ime/rime`. Its candidate window follows the
  text cursor, light or dark, takes clicks and turns pages with arrows and the wheel,
  and shows the preedit where the application does not. Each text field has a
  composition of its own. Its menu switches English letters, full width, simplified
  characters and English punctuation, and deploys again. While Rime deploys, keys go
  to applications as they are. It binds librime's C API by hand; the Nix package is
  `.#rime`, with librime and nixpkgs' rime-data, and `scripts/install.sh` builds it
  where librime is installed.
- An example engine in Python, `examples/python-gtk4`: it speaks the engine protocol
  with PyGObject's D-Bus, without the SDK, and draws a GTK 4 window, which becomes the
  popup at the text cursor like any engine's. An end-to-end test types, clicks and looks
  at the window's pixels; the end-to-end shell has Python, PyGObject and GTK 4 for it.
- Engines' floating panels: windows on the desktop rather than at the text cursor, such
  as a status bar or a palette. `omime-overlay` gives Slint components a layer surface
  that takes the pointer but not the keyboard (`Pointer::Take`), the SDK names the
  compositor's display (`ENV_HOST_WAYLAND_DISPLAY`), and the engine guide says how. As
  an example, Sparkle's menu turns on a panel with the emoji inserted last at the bottom
  right of the screen, which a click inserts again; Sparkle keeps the emoji and the
  switch in `$XDG_STATE_HOME/oh-my-ime/sparkle`.
- Desktop notifications about trouble that used to be only in omimed's log: an engine
  that stopped (it kept crashing, did not start in time, could not be launched or broke
  the connection to the compositor), another input method holding a seat, such as a
  Fcitx 5 that still runs, and a mistake in config.toml. Each is told once, again only
  after it was resolved or an hour later, and its notification goes away once it is
  resolved; a click opens the settings on their About page. In English and Chinese.
  `[notifications] enabled = false` turns them off.
- `omime setup` configures Hyprland's input environment and autostart in Lua or
  legacy `.conf`, with a read-only preview and a journal for undo that preserves
  unrelated edits. `--engine` sets the first-login method, `--start` activates the
  current session, and `--take-over` requests an existing Fcitx5/IBus to exit.
  Subsequent logins restore the user's saved choice, including direct input.
- Unmodified Wayland input methods run as engines. A manifest with
  `protocol = "input-method-v2"` (and no `api`) starts a program such as `fcitx5` on a
  private session bus, gives it a virtual `zwp_input_method_v2` and
  `zwp_virtual_keyboard_v1` on its bridge connection, and switches it like any other
  engine: Fcitx 5 runs inside oh-my-ime with all its input methods and addons. Its popups
  are engine windows, so they follow the text cursor of Wayland and X11 applications, and
  it serves XIM and Fcitx5-module applications too. `data/engines/fcitx5/engine.toml` is
  the manifest for Fcitx 5.
- Fcitx 5 and IBus are offered as engines wherever they are installed, without
  `omime install`: their manifests ship on the engine search path (Nix package and
  `scripts/install.sh`) and name the program they need in the new manifest key
  `requires`. With the new `enabled_by_default = false` they are offered, not
  enabled: `omime enable fcitx5` puts Fcitx 5 in the cycle, and `omime switch` works
  either way.
- IBus (1.5.32 or later) runs as an engine the same way, through the new `omime-ibus`:
  it starts `ibus-daemon` without a panel and IBus's GTK panel as the Wayland input
  method (`ibus-ui-gtk3 --enable-wayland-im`), so IBus's candidate window is a popup at
  the text cursor, and it writes the daemon's address where `ibus engine`, `ibus-setup`
  and friends look for it. `data/engines/ibus/engine.toml` is the manifest.
- Users choose input methods, not frameworks: the input methods inside a hosted Fcitx 5
  or IBus are choices of their own, such as `fcitx5/shuangpin` and `ibus/libpinyin`, in
  `omime list`, `omime switch`, `Ctrl+Shift+Space`, the tray menu and per-application
  engines. omimed asks the framework which input methods it has and which one is in
  use, follows its own switching, and remembers both across restarts. `omime list` now
  lists input methods; `omime engines` lists the installed engines as `omime list` did.
- A tray item (StatusNotifierItem) shows the input method in use in status bars such
  as waybar's tray: in the tooltip ("双拼 (Fcitx 5)"), and with the icon of Fcitx 5's or
  IBus's own tray item, which omimed follows on the private bus. A click toggles direct
  input, a middle click switches to the next input method, and its menu switches to any
  input method, each with its icon. `omime status` reports the input method too. `[tray] enabled = false`
  hides it.
- Engine protocol 1.1: engines can have input methods of their own, such as the schemas
  of a Rime engine, announced with `SetInputMethods` and `SetCurrentInputMethod` and
  chosen by users as `engine/id` like the input methods inside Fcitx 5 and IBus; omimed
  asks for one with `SelectInputMethod` and keeps the choice across restarts. The SDK
  has `Host::set_input_methods`, `Host::set_current_input_method`,
  `Engine::select_input_method` and `Engine::registered`, which runs on the engine's
  thread right after it registered, before any other call.
- A switcher: when you switch with the hotkeys or `omime switch`/`next`/`toggle`, the
  input methods show for a moment at the bottom of the screen, the one now in use
  highlighted. It is a program of its own, `omime-panel`, which omimed starts and keeps
  running (`[panel] enabled = false` turns it off), drawn with Slint on a layer surface
  (namespace `omime-switcher`) by the new `omime-overlay` library. Switches from the
  tray menu and those that follow you between applications do not show it.
- The windows hosted input methods open themselves, such as Fcitx 5's configuration tool,
  IBus's emoji picker or Qingjian's settings, open on the desktop as ordinary windows:
  they used to connect to omimed's bridge like the input method, which showed nothing but
  its popups. Hosted input methods now get their bridge connection as `WAYLAND_SOCKET`,
  and `WAYLAND_DISPLAY` names the real compositor.
- Engine windows open at the text cursor of Wayland applications that use the Fcitx5
  module (`QT_IM_MODULE=fcitx`, `GTK_IM_MODULE=fcitx`) too, on Hyprland and sway: omimed
  asks the compositor's IPC where the focused window is. Some applications reach an input
  method no other way, WeChat for one.
- X11 applications started with `XMODIFIERS=@im=fcitx` or `@im=ibus`, as setups of those
  input methods leave it, reach omimed too: it answers XIM clients by those names as well,
  unless Fcitx or IBus runs on the display.
- The control API's new `Switched(input_method, reason)` signal says who switched:
  omimed's hotkeys, a command, the tray menu or an engine.
- Engines can have a menu (engine protocol 1.1): actions, switches and choices such as
  Chinese or English punctuation, announced with `SetProperties`
  (`Host::set_properties` in the SDK) and activated with `ActivateProperty`
  (`Engine::activate_property`). The control API's `Properties`, `ActivateProperty`
  and `PropertiesChanged` offer the menu of the engine in use to status bars and
  settings programs; `omime properties` shows it and `omime activate <id>` activates
  an entry. The tray menu shows it below the input methods, with its submenus.
- Fcitx 5's and IBus's switches are the menu of their engine: Chinese punctuation, full
  width, simplified or traditional Chinese and the like, taken from the menu of their
  tray item on the private bus (without their input methods and their own entries) and
  clicked there. IBus does not update that menu when a property changes, so omimed has
  its panel build it again after IBus's engines change their properties.
- The control API grows what a settings program needs, described in
  `protocol/io.github.ign1x.OhMyIme.Daemon1.xml`: `Engines` and `InputMethods` list
  every engine and every input method with descriptions, languages and state, enabled
  or not, including the input methods a framework has beyond what it is set up with
  (kept in `~/.cache/oh-my-ime/catalog.toml`, and read from the files Fcitx 5 and IBus
  install while they have never run); `Config` and `Configure` read and set
  options of `config.toml`, which omimed writes itself, keeping comments; `Apps`,
  `SetAppInputMethod` and `ForgetApp` edit what `[apps] remember` keeps; the signals
  `InputMethodsChanged` and `ConfigChanged` say when to read again; `Version`. New
  commands use them: `omime list --all`, `omime config [<key> <value>]`, `omime apps
  [forget <app>]`; `omime enable` and `disable` now go through omimed.
- Input methods inside a framework can be enabled one by one: the `engines` list of
  `config.toml` takes entries such as `fcitx5/shuangpin` next to whole engines, in
  switching order, and `omime enable fcitx5/rime` and `omime disable fcitx5/pinyin`
  edit it. Fcitx 5 switches only to input methods of its current group, so selecting
  one that is not there adds it to the group first, which Fcitx 5 saves.
- Engines can have settings: `[engine.settings]` in the manifest names a program, which
  `omime settings <engine>`, "Configure …" in the tray menu and the control API's
  `OpenSettings` open as an ordinary window on the desktop, with the engine running.
  It can talk on the engine's private bus (`bus = "engine"`), and the engine can be
  restarted when it closes (`restart = true`). Fcitx 5's manifest opens
  `fcitx5-configtool` on Fcitx 5's bus; IBus's opens `ibus-setup` and restarts IBus
  afterwards, so that it reads what was changed.
- Engines learn whether an application shows the preedit itself (`client-preedit` in
  the context, `ContextInfo::client_preedit` in the SDK): xterm and other XIM
  applications that leave drawing it to the input method do not, so an engine with a
  window shows it there. The context also says how the application reaches omimed
  (`frontend`: `wayland`, `xim` or `fcitx`).
- Engines stop with omimed even when omimed is killed (`PR_SET_PDEATHSIG`), and hosted
  input methods get SIGTERM and a moment to save their state before SIGKILL.
- What engines print goes into omimed's log line by line, under the target `engine`
  and with the engine's id, instead of mixing into it unattributed.
- A hosted input method that hangs no longer swallows every key for good: when it
  reads nothing it is sent for three seconds, omimed kills and restarts it.

### Changed

- The recommended session environment leads every kind of application to omimed:
  `QT_IM_MODULE=fcitx` with `QT_IM_MODULES=wayland;fcitx`, and `XMODIFIERS=@im=fcitx`
  (Hyprland snippets, guide). It used to clear the module variables, which left Qt 5
  applications and Qt applications on X11, such as WeChat and WPS, without an input
  method. `omime doctor` checks for these variables, and no longer warns about Fcitx5
  modules, which reach omimed.

- `[tray] enabled` takes effect with `omime reload`. omimed also reloads its
  configuration on SIGHUP, and the systemd unit can reload it
  (`systemctl --user reload omimed`).
- Keys omimed does not know in `config.toml` and in engine manifests, misspelt or from a
  newer version, are left out with a warning (`omime doctor` lists them) instead of
  making the whole file invalid, so files written for newer versions still load.

### Fixed

- Native SDK engines install their D-Bus receiver before registering, so restoring
  an input method immediately after restart cannot lose the first selection call.
- Wayland text commits and forwarded keys retain their order across application
  state updates, with bounded waiting when an application supplies no feedback.
- Native input contexts have bounded extra time for first activation, so loading
  a Rime schema does not immediately trigger the ordinary key timeout.
- CI's Nix development shells provide librime at runtime, including for cached
  binaries built outside Nix. Rime settings tests retain the library-path wrapper
  when testing a local engine with an installed daemon.
- Closing an engine's settings window after typing no longer crashes sway 1.12.
  Hidden candidate windows now release their input-popup role objects, which sway
  otherwise keeps tracking across focus changes. Showing candidates recreates the
  role and reuses the held buffer.
- Engines with GTK 4 windows no longer make omimed log a warning on every frame: the
  compositor's presentation feedback also names the daemon's own outputs, which the
  bridge now keeps from engines.
- With Fcitx 5's default settings, the input method chosen in omimed, for example with
  `omime setup --engine fcitx5/pinyin`, is in use in the first text field: chosen
  while no field had focus, it only became Fcitx 5's default, and fields started on
  Fcitx 5's keyboard layout, so "nihao" stayed "nihao". omimed asks Fcitx 5 again once
  it focuses a field, gives a Fcitx 5 that starts again the input method that was in
  use, and keys that wait longer than a second for the switch go to the input method
  afterwards instead of to the application. The end-to-end tests no longer read the
  system's `/etc/xdg`, whose Fcitx 5 settings had hidden this.
- Input methods of a hosted framework that config.toml names, such as
  `fcitx5/shuangpin`, have their names (Shuangpin, 双拼) before the framework runs, in
  `omime list`, the tray and the settings, instead of their ids.
- First keys wait for a hosted framework to finish selecting its input method.
  IBus also waits for panel initialization and input-context focus, preventing
  its default layout or asynchronous startup from passing the first letters through.
  Setup checks selection acknowledgement and reports missing input-method addons.
- `omime doctor` recognizes `engine/method` configuration entries, checks the
  first-login method, and excludes omimed's hosted frameworks from competing processes.
- Updated the transitive Markdown parser to `pulldown-cmark 0.13.6`, fixing
  RUSTSEC-2026-0433.
- Delayed native and hosted key forwarding preserves the modifiers of the original
  event and restores the physical state afterwards.
- Replies abandoned by a switch, focus change or process exit cannot reset a newer
  composition. Timeout recovery queues its reset before accepting more engine keys.
- Deactivation completes in-flight Fcitx5 key calls, including batch calls and input
  contexts destroyed or changed to sensitive fields while an engine is busy.
- Invalid viewport parameters, duplicate viewports, buffer scales and transforms
  are rejected before reaching the compositor. Cleanup does not destroy a retired
  shm pool twice; attributed upstream faults quarantine their engine before reconnecting.
- Echo and Sparkle keep separate compositions for each context. Multi-seat engine
  selection and popup placement remain shared and are documented as incomplete.
- `scripts/install.sh` installs `omime-ibus`, which hosting IBus needs.
- Source installation includes the systemd user unit and session templates, with
  absolute paths for the daemon and bundled engines under the selected prefix.
- Names with an underscore show it in the tray menu, instead of underlining the
  character after it.
- A key that a hosted input method sends back only after it was released no longer
  stays held in the application: a busy Fcitx 5 or IBus could leave a key repeating
  there until the next key.
- In X11 applications, keys an input method sends back keep their modifiers: with
  Fcitx 5 or IBus as the engine, Ctrl+C in xterm no longer types a c, through XIM and
  through the Fcitx5 modules alike.
- Engine sockets accept only processes of the same user, as the architecture guide
  said they did: they are checked with `SO_PEERCRED`.
- An engine that keeps running while omimed reconnects to the compositor shows its
  windows again afterwards, and engines are told that the text fields of the lost
  connection are gone.
- A mistake in `config.toml` no longer goes unnoticed: `omime reload` reports it and
  omimed keeps the configuration it had, instead of silently falling back to the
  defaults for the whole file.
- On Hyprland with `input:virtualkeyboard:share_states = 1`, leaving a text field with
  Ctrl held (Ctrl+W, Ctrl+Tab, a click elsewhere) no longer leaves Ctrl held for every
  application until the next text field. omimed's virtual keyboard now lives only as
  long as its keyboard grab, as Fcitx 5's does: Hyprland merges an input method's
  virtual keyboard into the state of all keyboards once the grab ends.

## [0.3.0] - 2026-10-30

### Added

- Applications using the Fcitx5 GTK or Qt input method module (`GTK_IM_MODULE=fcitx`,
  `QT_IM_MODULE=fcitx`), such as X11 applications under XWayland, type through omimed's
  engines: omimed serves the Fcitx5 module protocol under `org.fcitx.Fcitx5` and
  `org.freedesktop.portal.Fcitx` whenever fcitx5 does not hold those names.
- X11 applications using XIM (`XMODIFIERS=@im=omime`), such as xterm, Java, Wine or
  GTK with `GTK_IM_MODULE=xim`, type through omimed's engines too: omimed serves XIM on
  the X display in `$DISPLAY` when it starts. Applications that leave drawing the preedit
  to the input method, such as xterm, do not show it.
- Engine windows open at the text cursor of X11 applications as well. Compositors show
  input popups only for Wayland text fields, so omimed shows the window on a layer
  surface of its own (namespace `omime-popup`, for compositor rules), placed below the
  cursor (above it near the bottom of the screen). X11 coordinates are mapped to the
  compositor's by output names, which also covers Hyprland's
  `xwayland:force_zero_scaling`.

### Fixed

- `SetPopupVisible` had no effect: an engine's window showed whenever a text field had
  focus, before the engine asked for it, and the engine could not hide it.

## [0.2.0] - 2026-10-30

### Added

- Per-application engines: with `remember = true` in the new `[apps]` section of
  config.toml, each application gets back the engine last chosen in it, also after a
  restart. omimed learns the focused application from the compositor's
  `zwlr_foreign_toplevel_manager_v1`, and engines now receive its app id (`app-id`).
- `omime doctor` explains what is wrong with a setup, one line per finding: whether
  omimed runs and holds every seat (and which other input method holds one), whether
  the compositor reports focused windows, `config.toml`, broken engine manifests,
  engines that cannot start and input method variables in the environment. The
  control API's new `Diagnose` method provides the daemon's part.
- `omime status --watch [--json]` prints one line per change of the status, also while
  omimed stops and starts, for status bars such as waybar; the control API's new
  `StatusChanged` signal carries the changes.
- `omime install <dir>` copies an engine directory into
  `~/.local/share/oh-my-ime/engines` after checking its manifest, and
  `omime uninstall <id>` removes it again; both work without a running omimed.
- Simplified Chinese versions of the guides in `docs/zh-CN`, linked from README.zh-CN.md.
- A Nix package: `nix profile install github:Ign1x/oh-my-ime`, or the flake's package and
  overlay in a NixOS configuration. It contains the systemd user unit, and works without
  a dev shell: omimed finds the bundled engines and Sparkle its graphics libraries. CI
  runs the end-to-end tests against it.

### Fixed

- Keys typed with Control held no longer fail to reach the engine. xkb types NUL for
  Ctrl+Space, which D-Bus strings cannot carry; `KeyEvent::text` now never contains
  control characters.

### Changed

- Warnings about failed or late engine requests name the request.

## [0.1.0] - 2026-10-29

The first release: a working input method platform for Hyprland and other
compositors with `input-method-v2`, with engines that draw their own UI.

### Added

- Project foundation: Cargo workspace, pinned toolchain, Nix dev shell with test compositors,
  CI (fmt, clippy, tests, cargo-deny), bilingual README and architecture documentation.
- `omime-proto`: D-Bus names, protocol version and wire types, engine manifests, the
  configuration file and XDG paths.
- `omimed`: the daemon.
  - `input-method-v2` frontend with keyboard grab, virtual keyboard passthrough, key
    repeat for consumed keys, and the commit/preedit rules Hyprland needs.
  - UI bridge: engine windows become IME popups through a multiplexing Wayland proxy,
    with a virtual `xdg_shell`, pointer routing, held buffers while hidden, shm
    validation, dependency-ordered cleanup and reconnection.
  - Engine host: discovery, launch, supervision with crash restarts, startup and key
    timeouts, ordered per-field request queues.
  - Switching with hotkeys (`Ctrl+Space`, `Ctrl+Shift+Space`) and the
    `io.github.ign1x.OhMyIme.Daemon1` control API; single instance per session.
  - Password and PIN fields bypass engines and the keyboard grab.
- Engine protocol v1 (`protocol/io.github.ign1x.OhMyIme.Engine1.xml`) and the Rust SDK
  `omime-sdk`.
- `omime` CLI: `list`, `status [--json]`, `switch`, `next`, `toggle`.
- `echo` engine for tests and as a tutorial.
- `sparkle` demo engine: emoji by `:shortcode` from an animated deck drawn with Slint,
  with mouse selection and an arrow that follows the text cursor.
- UI bridge: engine windows are hidden until their engine calls `SetPopupVisible(true)`;
  the last fractional scale is handed to new engine windows.
- End-to-end tests in a private headless sway session, run in CI, including slow and
  misbehaving engines and a lost compositor connection.
- omimed remembers the engine in use across restarts; `omime enable`, `omime disable`
  (editing `config.toml` in place) and `omime reload`.
- `scripts/install.sh`, Hyprland snippets (Lua and legacy), a systemd user unit, a sample
  configuration and the Hyprland guide (`docs/hyprland.md`).

[Unreleased]: https://github.com/Ign1x/oh-my-ime/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/Ign1x/oh-my-ime/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/Ign1x/oh-my-ime/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Ign1x/oh-my-ime/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Ign1x/oh-my-ime/releases/tag/v0.1.0
