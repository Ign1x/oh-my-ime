# oh-my-ime architecture

English | [简体中文](zh-CN/architecture.md)

This document describes how oh-my-ime is put together and why. It is the reference for
contributors; the README gives the short version.

## Goals

- Input methods ("engines") are independent processes with full control over their UI.
- The platform owns everything that must be unique per seat (compositor protocols, key
  routing, focus, switching) and delivers text.
- Wayland first, Hyprland first, without coupling the design to one compositor.
- Engine crashes and hangs never break typing.
- Compatibility with the Fcitx5 and IBus ecosystems through clearly labelled layers.

## Constraints that shape the design

These facts come from the protocols and from the compositor sources, and they are the reason
the design looks the way it does.

1. **One input method per seat.** `zwp_input_method_manager_v2.get_input_method` succeeds for
   a single client. Later binds receive `unavailable` and become inert. When the slot frees up
   there is no notification, so a client must bind again.
2. **Popups belong to the input method's connection.** `get_input_popup_surface` takes a
   `wl_surface` argument, and libwayland resolves object arguments in the sending client's
   own object map. A surface created by another process can never become an IME popup.
3. **The compositor positions popups.** The input method only learns the text cursor rectangle
   *relative to its popup* (`text_input_rectangle`). Hyprland places the popup below the cursor,
   flips it above near the bottom edge, and shifts it left near the right edge.
4. **Hyprland does not hide popups on `deactivate`.** It draws them as long as the text field
   has keyboard focus. The input method has to unmap them itself.
5. **Virtual keyboard bypass.** Keys sent through a `zwp_virtual_keyboard_v1` that belongs to
   the same client as the active keyboard grab skip the grab, and Hyprland's keybinds. Keys from
   any other virtual keyboard are routed *into* the grab.
6. **Modifiers go only to the grab** while one exists. The input method must forward them to
   its virtual keyboard, or applications see stale modifier state.
7. **A virtual keyboard must receive a keymap before any key or modifiers event.** Anything
   else is a protocol error, and protocol errors are fatal for the whole connection.
8. **`text-input-v3` clears the preedit on every `done`**, and Hyprland only emits `done`
   after the input method commits. The input method must therefore commit after every `done`,
   re-sending its current preedit.
9. **XWayland has no text-input support.** X11 clients need XIM or GTK/Qt IM modules.
10. **Hyprland merges the state of all keyboards** (`input:virtualkeyboard:share_states`).
    It leaves an input method's virtual keyboard out only while the input method holds a
    grab. With `share_states = 1` it merges it the rest of the time, so a modifier the
    virtual keyboard still holds stays held for every application. With 2, the default, it
    stops merging a virtual keyboard once it has seen it under its input method's grab.

## Processes

```
compositor ── one client connection ── omimed ──┬── D-Bus p2p ── engine process (logic)
                                                └── private Wayland socket ── engine UI
```

- **`omimed`**: the daemon. A single-threaded actor (tokio `current_thread` + `LocalSet`) owns
  the `wl-proxy` state and therefore every Wayland object. D-Bus services (zbus) run on the
  same runtime and talk to the actor through channels.
- **Engines**: one process each, launched and supervised by `omimed`.
- **`omime`**: the command line tool. It talks to the daemon over the session bus.

## The Wayland frontend

For each `wl_seat`, `omimed` binds a `zwp_input_method_v2` and keeps an input context:

- **Activation.** On `activate`+`done`, the context is focused. If the content type is
  sensitive (password, PIN, `sensitive_data`, `hidden_text`), no keyboard grab is taken and keys
  never pass through the daemon. Otherwise the daemon grabs the keyboard.
- **Grab lifetime.** The grab is held only while the context is active. Before releasing it,
  the daemon releases every key it still holds on its virtual keyboard; afterwards it
  destroys the virtual keyboard, which lives only as long as the grab (see 10 above). A new
  grab is created only after the old one is destroyed.
- **Keys.** Grab events are decoded with xkbcommon (keysym, UTF-8, modifiers). Hotkeys are
  handled first. The remaining keys go to the active engine, whose reply decides whether the key
  is consumed or forwarded through the virtual keyboard. Releases of keys whose press was not
  consumed are always forwarded, including keys pressed before the grab started.
- **Repeat.** Applications repeat forwarded keys themselves. The daemon synthesises repeats,
  using `repeat_info`, only for keys an engine consumed, and turns that off if the compositor
  sends its own repeat events.
- **Commit discipline.** After every `done`, the daemon sends a commit that carries the current
  preedit, whether or not anything changed.
- **Text delivery.** For fields that report surrounding text, text changes and
  forwarded keys share an output queue. After a text change, the next output waits
  for the application's updated text, cursor or selection, then uses the latest
  input-method serial. This prevents consecutive commits from racing the state
  update caused by the previous commit. Releases of delivered keys can still pass
  through. Waiting stops after 200 ms if no change arrives, and stays disabled until
  feedback resumes; fields without surrounding text do not wait. Sent text is never
  replayed, and leaving a field discards its unsent output. Surrounding updates are
  not protocol acknowledgments, so unrelated state changes and applications that do
  not report updates can still race a commit.
- **Focused application.** Input methods are not told which application a text field
  belongs to. Where the compositor offers `zwlr_foreign_toplevel_manager_v1` (Hyprland,
  sway, ...), the daemon follows which window is activated and passes its app id to
  engines (`app-id`); with `[apps] remember`, the core gives each application the engine
  last chosen in it. Window titles are never read.

## The UI Bridge

The bridge is a multiplexing Wayland proxy built on [`wl-proxy`](https://crates.io/crates/wl-proxy).
All engine connections share the daemon's upstream connection, so their surfaces are, from the
compositor's point of view, the input method's own surfaces.

- **Per-engine sockets.** Each engine gets `$XDG_RUNTIME_DIR/oh-my-ime/<engine-id>/wayland`
  (directory mode `0700`). `WAYLAND_DISPLAY` is set to that relative name, and `DISPLAY` is
  removed so toolkits cannot silently fall back to X11. The daemon accepts connections itself,
  so it always knows which engine a client belongs to.
- **Global filtering.** Engines see a virtual registry. It contains the globals a UI needs
  (compositor, subcompositor, shm, seat without keyboard capability, outputs, xdg-output,
  linux-dmabuf, viewporter, fractional scale, cursor shape, single-pixel buffer,
  presentation time) and hides everything else (text input, input method, virtual
  keyboard, layer shell, capture protocols). Binds are checked against the advertised
  version and performed on the daemon's own registry. The daemon learns about globals
  synchronously, so an engine never finishes its first roundtrip with an incomplete
  registry.
- **Virtual protocols.** `xdg_wm_base` is answered by the bridge: an engine creates an
  ordinary frameless `xdg_toplevel`, the bridge answers the initial commit with
  `configure(0, 0, [])` and dismisses `xdg_popup`s. A virtual
  `zxdg_decoration_manager_v1` always says "server side", so toolkits drop their title
  bars, and a virtual `wl_data_device_manager` gives toolkits that insist on one an empty
  clipboard: engines never see the user's clipboard.
- **Popup lifecycle.** Each engine surface receives an input-popup role object right
  before the commit that should map it. Windows are hidden until
  their engine calls `SetPopupVisible(true)`, and only the active engine's windows are
  shown. To hide a window, the bridge injects `attach(NULL)` + `commit` and swallows the
  compositor's release of the last buffer, holding it on the compositor's behalf. It
  also destroys the popup role object so an unmapped popup cannot retain compositor
  focus references when an application window closes (a crash in sway 1.12). Showing
  the window again recreates the role object and re-attaches that buffer instantly,
  without waiting for the toolkit to draw. Buffers committed while hidden replace the
  held one. While an X11 application has
  focus and no Wayland text field does, a shown window goes to a mirror instead; see
  [X11 applications](#x11-applications).
- **Scale.** Compositors report an input popup's fractional scale only once it is
  mapped; the bridge hands the last known scale to new engine surfaces right away. A
  mirror's preferred fractional scale reaches the engine as if it came from its own
  surface.
- **Frame callbacks.** The bridge answers an engine window's frame callbacks itself: it
  requests a frame on whichever surface currently shows the window, the popup or the
  mirror, and answers the engine's callbacks when that surface's frame arrives. A toolkit
  never stalls when its window moves between the two.
- **Input routing.** Pointer `enter`, `motion`, `button`, `axis` and `frame` go only to the
  engine whose surface the pointer entered. `wl_surface.enter(output)` and similar events are
  filtered by owner. Pointer events on a mirror reach the engine as events on its own
  surface.
- **Cleanup.** When an engine disconnects or dies, its compositor objects are destroyed in
  dependency order: popup role, then surface extensions, subsurfaces, surfaces, buffers and
  pools, and finally seat and output objects. If an engine destroys a surface itself, the popup
  role is destroyed first.
- **Failure handling.** Requests that compositors treat as fatal and that are easy to
  check (shm pools and buffers, viewport parameters and ownership, buffer scales and
  transforms, duplicate fractional-scale objects, bind versions, virtual objects)
  are validated, and a broken engine is
  disconnected on its own. Retired shm pools are not destroyed again while awaiting
  `delete_id`. If an upstream protocol error closes the shared connection, the error
  object's owner is recorded before teardown and quarantined before reconnection.
  It remains failed until explicitly selected again. Errors that cannot be attributed
  still use the daemon's bounded reconnect policy, which allows the process crash
  budget to stop a repeatedly crashing engine before the daemon gives up. A hangup
  can precede the error event, so immediate attribution is not always available.

## X11 applications

X11 applications never speak `text-input`, so the Wayland frontend cannot reach them. Two
X11 frontends do, and both feed the same core and the same engines; an engine does not
know which frontend a key came from.

- **XIM.** Built on the [`xim`](https://crates.io/crates/xim) and
  [`x11rb`](https://crates.io/crates/x11rb) crates. When `DISPLAY` is set at startup,
  omimed becomes the XIM server `omime` on that display; applications find it with
  `XMODIFIERS=@im=omime`. Its window also takes the selections `@server=fcitx` and
  `@server=ibus` when no other XIM server holds them, for applications started with
  what setups of Fcitx and IBus leave in `XMODIFIERS`. While the X server is not up, omimed retries with a growing
  delay (2 s, doubling up to 60 s), and it reconnects when the server goes away. Only key
  presses pass through XIM; releases reach the application directly. libX11 puts a commit
  or a key sent back at the front of the application's event queue, so two in a row would
  arrive in the wrong order: omimed sends them one at a time as synchronous requests and
  waits for the application's `XIM_SYNC_REPLY` (300 ms at most) before the next. Keysyms
  come from the X server's core keyboard mapping. Each connection may allocate at most
  65,534 input contexts over its lifetime, including contexts already destroyed. Further
  CreateIc requests receive `XIM_ERROR` (`BadAlloc`), so libX11's `XCreateIC` returns NULL.
  The connection and its existing contexts keep working, including pending keys and
  queued output. New contexts need a new XIM connection, for example by restarting
  the application. omimed warns once per connection about the limit.
  Only a panic in the XIM protocol handler drops a connection as fault containment,
  releasing that connection's contexts, pending keys and queued output. Input-method
  ID exhaustion from repeated Open/Close requests is one such panic; libX11 never
  causes it because it opens one input method per connection. Other clients keep
  working. This containment requires `panic = "unwind"`, Rust's default, in release
  builds too.
- **Fcitx5 modules.** omimed serves `org.fcitx.Fcitx.InputMethod1` and
  `org.fcitx.Fcitx.InputContext1` under the bus names `org.fcitx.Fcitx5` and
  `org.freedesktop.portal.Fcitx`, and queues for them while another program, such as a
  running fcitx5, holds them. Each application context gets one input context object, and
  only its owner, the application's unique bus name, may call it. Keys arrive with
  `ProcessKeyEvent` or `ProcessKeyEventBatch`; the committed text, preedit, deletions of
  surrounding text and keys sent back reach the application as signals before the key's
  reply. With the `KeyEventOrderFix` capability, a key the engine did not consume but that
  produced text is answered as handled and sent back after the text. Password fields
  bypass the engines, as everywhere else.
  Each owner (a unique client bus connection) may hold at most 1,024 contexts across
  both names, with 8,192 contexts in total. `CreateInputContext` returns
  `org.freedesktop.DBus.Error.LimitsExceeded` before exporting an object when either
  quota is full; destroying a context or disconnecting releases its slot, including
  creations still in flight when the owner disconnects. Each context
  retains at most 4,000 bytes of surrounding text in a UTF-8 window around the cursor,
  matching the hosted-engine bound. Cursor and anchor are adjusted in characters before
  conversion to engine byte offsets; an anchor outside the window is clamped to its
  edge. Position-only updates keep their original character offsets. If the cursor
  leaves the retained window, engines receive an empty buffer with zero cursor and
  anchor offsets, and deletion requests are ignored until a full-text update arrives.
  Password and sensitive fields never cache surrounding text. Leaving a field,
  marking it sensitive, or removing its `SurroundingText` capability clears the cache;
  removing the capability also clears the active engine's copy. Refocusing starts
  without surrounding text until the application sends fresh text. Position-only
  updates cannot restore a cleared cache, and deletion requires valid cached text
  to convert engine byte counts into application character counts.
  The GTK 4 Fcitx5 module may suppress unchanged text after refocusing, and Qt 6 may
  remove surrounding-text support during widget updates. Operations that need this
  text remain unavailable until the application sends it again and keeps it valid.
- **Engine windows.** Compositors show input popups only for Wayland text fields. While an
  X11 application has focus and no Wayland text field does, the bridge shows the active
  engine's window on a layer surface of its own, a *mirror*: layer `overlay`, namespace
  `omime-popup`, which needs `zwlr_layer_shell_v1` (Hyprland, sway and other wlroots
  compositors have it). The mirror shows the engine's buffers with the scale, transform and
  viewport of the engine's surface; the engine's own surface stays unmapped meanwhile. It
  opens below the text cursor, above it when there is no room below, and moves left when it
  would leave the output on the right. The cursor comes from `SetCursorRect` for the Fcitx5
  modules, or from the XIM spot location, where the application draws text; omimed assumes
  a line from 16 pixels above to 6 pixels below the baseline. X11 coordinates are mapped to
  the compositor's by matching XWayland's RandR monitors with the compositor's outputs by
  name (xdg-output), which also handles Hyprland's `xwayland:force_zero_scaling`.
  Wayland applications using the Fcitx5 modules (`display` is `wayland:`) have no input
  popup either, and their text cursor is relative to their window: omimed asks the
  compositor's own IPC where the focused window is (Hyprland's `j/activewindow`, sway's
  `GET_TREE`) when the field gets focus, and again at most once a second as the cursor
  moves, and shows the window on a mirror at the window's position plus the cursor
  (`SetCursorRectV2` divided by its scale).
  Incoming Fcitx rectangles and XIM cursor rectangles are clamped to coordinates within
  ±(1 << 24) and dimensions from 0 through (1 << 16). A non-finite, zero or negative
  cursor scale is treated as 1. Placement uses 64-bit integer arithmetic and saturates
  results back to 32-bit coordinates.

## Engine protocol (v1)

Engines connect to a private peer-to-peer D-Bus socket whose address is in
`OMIME_ENGINE_ADDRESS`.

Each installed engine owns its listening sockets. Uninstall closes both listeners,
unlinks their paths, and cancels pending native connections; re-adding the engine
creates fresh listeners. Native peers have five seconds from acceptance to complete
authentication and registration, with at most 16 pending peers per engine. Excess
connections close immediately. Authentication alone does not release the quota.
A silent handshake uses up to three daemon descriptors on Linux: its transport,
a cancellation duplicate, and zbus's peer-process descriptor during authentication.
Successful registration releases the cancellation descriptor and transfers the
connection to the core.
Silent peers can occupy every permit until the deadline; these private, same-user
sockets are not a security boundary between processes of the same user. Repeated
accept failures back off from 100 ms to one second, with at most one warning per
listener every five seconds. Hosted engines' inherited socket pairs are unaffected.

For shutdown, restart, settings reload and uninstall, the native supervisor sends
`Shutdown` and waits up to two seconds, then sends SIGTERM and waits another two
seconds before SIGKILL. Sending Shutdown shares the first deadline. Watchdog and
quarantine kills remain immediate. The daemon waits at most five seconds for its
engines and keeps a healthy UI bridge connected during their shutdown. Its systemd
unit uses `KillMode=mixed` and a six-second stop timeout, letting the daemon stop its
children first. Native children use `PR_SET_PDEATHSIG=SIGKILL` for an abrupt daemon
death; the SDK treats direct SIGTERM as an orderly stop.

- `io.github.ign1x.OhMyIme.Engine1` is exported by the engine at
  `/io/github/ign1x/OhMyIme/Engine`. It has `Activate`, `Deactivate`, `UpdateContext`,
  `ProcessKey`, `Reset` and `Shutdown`. `ProcessKey` returns `(handled, actions)`, where
  `actions` is an ordered list of commit, preedit, delete-surrounding and forward-key
  operations.
- `io.github.ign1x.OhMyIme.EngineHost1` is exported by the daemon at
  `/io/github/ign1x/OhMyIme`. It has `Register`, out-of-band actions (for example committing
  after a mouse click on a candidate), `SetPopupVisible`, `SetStatus` and `RequestSwitch`.
  Everything except `Register` is fire-and-forget.
- Field activation allows at least one second (or `key_timeout_ms`, if longer) to
  load a schema or dictionary, with incoming keys queued in order. Key events still
  time out after 200 ms by default. On timeout the key is forwarded to the
  application and later keys bypass that context until the request completes and a
  queued `Reset` finishes. A call that still has not completed after two further
  key-timeout intervals causes its engine to be restarted; this also bounds a stuck
  activation. Replies abandoned by switching,
  deactivation or process exit are discarded without modifying the current context.
- Composition belongs to a `ContextId`. Multiple contexts can coexist; the bundled
  engines store separate state for each. Engine selection and popup placement remain
  global, so the supported UI workflow is single-seat.
- Engines must not hide their toolkit windows. They ask the platform with `SetPopupVisible`.
- Version 1.1 lets an engine have input methods of its own, such as the schemas of a
  Rime engine: it announces them with `SetInputMethods` and the one in use with
  `SetCurrentInputMethod`, users choose them as `engine/id` like the input methods
  inside hosted frameworks, and the daemon asks for one with `SelectInputMethod`. The
  core treats them like a framework's: the choice is kept across restarts and given
  back to the engine when it registers.
- Version 1.1 also gives engines a menu: `SetProperties` announces actions, switches and
  choices (Chinese or English punctuation, full or half width) as a flat list of
  entries, each naming the submenu it is in, and the daemon calls `ActivateProperty`
  when one is clicked. The menu of the engine in use is what the control API offers.
- The protocol is versioned by interface name (major) and negotiated at `Register` (minor).
  Extensible arguments use `a{sv}`, and unknown keys are ignored. The XML in `protocol/` is the
  source of truth.

## Session setup and application diagnostics

The CLI's setup journal covers the compositor include, engine choice, systemd's
`environment.d/90-oh-my-ime.conf` and a marked block in UWSM's `env-hyprland`.
`--start` updates D-Bus activation and systemd's user-manager environment only after
the daemon has acquired its input seat. It connects through the selected session
bus and rejects a manager naming another Wayland display. This affects future
launches; no running application's environment can be changed this way. Undo
restores the managed files, with the restored environment taking effect at login.

`doctor --app PID` reads only the current user's selected process. Its output
contains a fixed allowlist of input-related environment variables and names of
recognized input/display plugins found in `/proc/PID/maps`. It never reports the
process command line, window title, document text or other environment values.
These are configuration observations, not evidence that a field sends keys. The
daemon's reported engines supplement local discovery because a service may have
different manifest paths from the terminal running `doctor`.

## Rime settings

`omime-rime --settings` is a separate Slint process on the real compositor, opened
through the engine manifest's settings command. It uses librime's `levers` module
as a deployer without input sessions. Schema selection, page size and Shift behavior
are patches in the engine's `default.custom.yaml`; appearance is in
`$XDG_CONFIG_HOME/oh-my-ime/rime.toml`. Each file is staged before replacement and
save errors stay visible in the window. The manifest requests a restart when the
settings process exits, so the engine reads the appearance and deploys changed Rime
configuration. No engine-protocol changes are needed.

## Hosted input methods

An unmodified Wayland input method, such as Fcitx 5, runs as an engine when its manifest
says `protocol = "input-method-v2"`. It has no engine-protocol socket: it gets its bridge
connection ready-made, as `WAYLAND_SOCKET` (one end of a socket pair whose other end the
bridge takes), and its registry shows two globals more, a virtual
`zwp_input_method_manager_v2` and a virtual `zwp_virtual_keyboard_manager_v1`, both
answered by the bridge. `WAYLAND_DISPLAY` names the real compositor: Wayland libraries
take `WAYLAND_SOCKET` and remove it from the environment, so the programs the input
method starts itself (Fcitx 5's configuration tool, IBus's emoji picker, Qingjian's
settings) reach the desktop as ordinary windows. `omime-ibus` hands the socket on to
IBus's panel only, so that ibus-daemon and its engines do not hold it.

- **Activation.** The core activates the virtual input method for a text field as a
  compositor would: `activate`, the field's surrounding text and content type, `done`.
  Every key goes to the input method's keyboard grab with the modifiers it was typed
  with, so a batch of keys arrives in order. The grab gets the keymap and repeat rate of
  the seat (no repeats for X11 fields; the X server repeats their keys). What the input
  method does to the field (`commit_string`, `set_preedit_string`,
  `delete_surrounding_text`, then `commit`) comes back as engine actions; a commit whose
  serial is from before the field was activated is dropped.
- **Keys it does not want.** An input method forwards them through its virtual keyboard.
  The bridge turns them into `ForwardKey` actions, so omimed's own virtual keyboard
  stays the only one the compositor sees, and keeps track of what the application holds.
  Keys wait up to a second for a hosted engine that is still starting, or has not grabbed
  the keyboard yet, then pass through.
- **Hung input methods.** A key handed to an input method is its own, so one that
  hangs would swallow every key. After handing it keys, the core looks every 250 ms at
  how much of what the bridge sent it is still unread on its socket (`SIOCOUTQ`); an
  input method that reads nothing for three seconds is killed and restarted, and the
  keys it held are lost. This works for any Wayland input method, with or without
  D-Bus.
- **X11 fields.** Fields of XIM and Fcitx5-module applications use the input method of
  any seat. XIM passes only presses, so each delivered key is released right away, and
  modifier keys stay with the application: an input method would take a lone Shift for
  a tap.
- **Windows.** The input method's popup surfaces are engine windows, shown as input
  popups or on a mirror, and the text cursor rectangle reaches it through its popup
  role. Toplevels on its bridge connection cannot be shown; the programs it starts are
  on the desktop instead (see above).
- **Process.** omimed starts the input method on a private session bus, a `dbus-daemon`
  without service activation in the engine's runtime directory, so that it cannot take
  `org.fcitx.Fcitx5` on the user's bus from omimed's Fcitx5 frontend. It gets the
  `WAYLAND_DISPLAY` of its bridge socket and no `DISPLAY`. Hosted engines get SIGTERM when
  omimed exits, even when omimed is killed (`PR_SET_PDEATHSIG`), and hosted input methods
  up to two seconds to save their state. One input method per seat: a second
  `get_input_method` gets `unavailable`.
- **IBus.** IBus is several processes, and none of them runs it as an input method on
  its own: `ibus-daemon` starts a panel but no Wayland client, and the panel in input
  method mode (`ibus-ui-gtk3 --enable-wayland-im`, IBus 1.5.30 or later) exits when no
  daemon answers. `omime-ibus` starts the daemon without a panel, waits for its address
  file, starts the panel as the input method, and stops both together. It writes the
  address file IBus tools in the session look for, named after the session's display
  (`OMIME_HOST_WAYLAND_DISPLAY`), so `ibus engine` and `ibus-setup` reach the hosted
  daemon. dconf's service is not on the private bus: the hosted IBus reads the settings
  dconf holds, but cannot save any.
- **Input methods inside.** Users choose input methods, not frameworks: Fcitx 5's 双拼
  and 拼音 and IBus's libpinyin are choices of their own, with ids `fcitx5/shuangpin`
  and `ibus/libpinyin` wherever an engine id goes (`Switch`, per-application engines,
  `Ctrl+Shift+Space`, the tray menu, the `engines` list of `config.toml`, where an
  entry `fcitx5` stands for everything in Fcitx 5's group and the engines to run are
  those the entries name). Fcitx 5 switches only to input methods of its current
  group, so selecting one that is not in it adds it there first
  (`SetInputMethodGroupInfo`, which Fcitx 5 saves in its profile). An adapter task runs beside each launch of a
  hosted engine: it finds Fcitx 5 by its name on the private bus, or IBus by the address
  file `omime-ibus` names in `$OMIME_ENGINE_RUNTIME_DIR`, and reports the input methods
  (Fcitx 5's current group; IBus's `preload-engines` from the user's dconf database,
  read as a file) and the one in use (`CurrentInputMethod`, `GlobalEngine`), following
  `InputMethodGroupChanged`, `GlobalEngineChanged` and the framework's tray item.
  Selecting is `SetCurrentIM` or `SetGlobalEngine`, which work without a focused field.
  Keyboard layouts are left out: direct input is omimed's. The lists and the input
  method in use are kept across restarts, so they can be chosen before the framework
  starts, and the framework gets the input method in use back when it starts.
- **Tray items.** Fcitx 5 and IBus publish a tray item (`org.kde.StatusNotifierItem`)
  with their current input method. On the private bus no status bar takes it, so omimed
  plays `org.kde.StatusNotifierWatcher` there, follows the item the input method
  registers, and makes the engine's status from it: the tooltip's title, the
  `XAyatanaLabel` and the icon. omimed's own tray item shows the icon, with the input
  method's `share/icons` as the theme path when the item names none.
- **Their menus.** The menu of that tray item (`com.canonical.dbusmenu`) becomes the
  engine's menu, which the control API and omimed's tray offer as they offer a native
  engine's: what the input method has to switch, without its input methods and its own
  entries. Of Fcitx 5's menu these are the status actions (ids from 300 on: Chinese
  punctuation, full width, simplified or traditional Chinese); of IBus's, the engine's
  properties before the first separator. omimed reads the menu after `AboutToShow`
  whenever the item changes and again after `LayoutUpdated`, and passes clicks on as
  `Event(id, "clicked")`; the menu's ids are the entries' ids. IBus exports its menu
  anew only when an engine registers its properties, not when one of them changes, so
  omimed follows the engines' `UpdateProperty` on IBus's bus and has the panel build
  the menu again shortly after: a middle click (`SecondaryActivate`) flips it to the
  panel's own menu and a second one back, each building it afresh.

## Control API

`io.github.ign1x.OhMyIme.Daemon1` on the session bus is how everything else talks to
omimed: the `omime` command line tool, status bars, and settings programs. The XML in
[`protocol/`](../protocol/io.github.ign1x.OhMyIme.Daemon1.xml) describes it.

- **Lists.** `Engines` and `InputMethods` return dictionaries meant for a settings
  program: every installed engine with its description, languages, kind and state, and
  every input method there is to choose from, enabled or not. What a framework has
  beyond what it is set up with (Fcitx 5's `AvailableInputMethods`, IBus's `Engines`,
  without keyboard layouts) is kept in `$XDG_CACHE_HOME/oh-my-ime/catalog.toml`, so it
  is listed before the framework runs again. Of a framework that never ran, the files
  it installs say what it has: Fcitx 5's `share/fcitx5/inputmethod/*.conf` and the
  engines of IBus's `share/ibus/component/*.xml`, running the programs that list them
  as IBus does, in the installation prefixes of the programs its manifest runs and
  requires. `InputMethodsChanged` says when to read them again.
- **Configuration.** omimed writes `config.toml` for `Configure`, which sets
  options by dotted name (`apps.remember`, `engines`), checks that the file stays
  valid, keeps comments and everything else, writes it through a temporary file and
  reloads. `omime enable`, `disable` and `config` use it; only `omime install` and
  `uninstall` and `setup`, which work without omimed, write the file themselves.
  `setup` also manages Hyprland includes and startup lines, recording original and
  installed contents for conflict-aware undo. `initial_input_method` supplies a
  first-login choice; saved state takes precedence on later logins.
  `ConfigChanged` follows every reload.
- **File writes.** Configuration, daemon state and the input-method catalog use an
  exclusively created, uniquely named sibling file, then rename it over the destination.
  They keep the destination's mode and create new files with `0600`; failures preserve
  the previous file and remove the temporary. Configuration writes sync the file before
  renaming. State, catalog and Sparkle history writes skip fsync to avoid adding disk
  flush delays to input handling; atomic replacement does not guarantee persistence
  through power loss. Configure and CLI engine edits replace a symlinked `config.toml`,
  preserving its target's mode without changing the target; setup instead refuses
  symlinks.
- **Applications.** `Apps`, `SetAppInputMethod` and `ForgetApp` read and edit what
  `[apps] remember` keeps for each application.
- **Menus.** `Properties` returns the menu of the engine in use, `ActivateProperty`
  activates one of its entries and `PropertiesChanged` says when it changed;
  `omime properties` and `omime activate` use them.

## Tray item

omimed publishes one tray item on the session bus (`org.kde.StatusNotifierItem`, with a
`com.canonical.dbusmenu` menu) and registers it with the status bar's
`org.kde.StatusNotifierWatcher`, again whenever a watcher appears. The item keeps no
state: every property is read from the daemon loop, and the loop tells the item when the
current engine, its status or the engines change, which sends `NewIcon`, `NewToolTip` and
`LayoutUpdated`. The icon is the hosted input method's, else the engine's status or
manifest icon, else `input-keyboard`; the tooltip is "engine · status". A click toggles
direct input, a middle click switches to the next input method, and the menu holds the
input methods and direct input as radio items with their icons; for an input method inside a framework
the tooltip is "input method (framework)". Below them come the current engine's own
menu, with its switches as check items and its submenus, "Configure …" for an
engine with settings, and "Settings…", which opens `omime-settings`. An entry keeps its id while the menu changes, so a click on a menu
shown before a change does what the entry said. `[tray] enabled = false` turns it off.

## Switcher

`omime-panel` shows the input methods for a moment, the one now in use highlighted,
when the user switches with omimed's hotkeys or the control API's `Switch`, `Next` and
`Toggle`. It is an ordinary client of the compositor: omimed starts it with
`[panel] enabled`, restarts it when it stops, and stops it with itself. It learns of
switches from the control API's `Switched(input_method, reason)` and shows itself for
the reasons "hotkey" and "command"; switches from the tray ("menu"), from engines
("engine") and those that follow the focus between applications are left alone.

It draws with Slint through `omime-overlay`, a small library that makes a layer surface
(`zwlr_layer_shell_v1`, smithay-client-toolkit) Slint's platform: Slint's software
renderer draws into shared memory, the surface takes no input, and the application
shows and hides it with the size it needs. Engines' floating panels use it too, taking
the pointer (`Pointer::Take`): Sparkle's recent-emoji panel, a process of its own on the
compositor's display, which draws color emoji as images (the software renderer draws a
glyph in one color only).
Slint stays out of omimed, the command line tool, the protocol crate and the SDK.

## Settings

`omime-settings` is oh-my-ime's settings window: the input methods and their order,
with a list of what can be added grouped by language; the hotkeys, recorded by pressing
them; the input method of each application; the options of `config.toml`; and what
`Diagnose` reports. It is an ordinary Wayland application (Slint with winit, which
follows the desktop's light or dark preference through the settings portal) and keeps
nothing of its own: it reads everything from the control API, again after
`InputMethodsChanged`, `ConfigChanged` and `StatusChanged`, follows omimed's bus name
as omimed stops and starts, and changes things with `Configure`, `OpenSettings`,
`SetAppInputMethod` and `ForgetApp`, so omimed remains the only writer of
`config.toml`. Its texts are English in the user interface and translated from the
gettext catalogs in `crates/omime-settings/po`, which are built in. `omime settings`
and "Settings…" in the tray menu open it, and the desktop entry
`io.github.ign1x.OhMyIme.Settings` (also its application id) puts it in launchers.

## Notifications

omimed tells the user about trouble that would otherwise only be in its log, with
desktop notifications (`org.freedesktop.Notifications`, connected to once there is
something to tell): an engine that stopped and stays stopped until the user switches to
it again (it kept crashing, did not register in time, could not be launched, or was
quarantined after a compositor protocol error), another input method holding a seat
(`unavailable` from the compositor), a mistake in config.toml, at startup (omimed
uses the defaults) or on a reload (it keeps what it had), and a desktop missing the
required input protocols. For an unsupported desktop omimed exits right after
telling, so the click action has nothing left to do. The core reports engine and
seat trouble to the daemon with a listener, as it reports state and lists. Each trouble
is told once, again only after it was resolved (the engine registered, the seat's input
method was activated, the configuration loaded) or an hour later, and a resolved
trouble's notification is closed. While omimed runs, the default action opens `omime-settings` on its
About page. Texts are in Chinese under a Chinese locale. `[notifications] enabled =
false` turns them off.

## Compatibility work after the current milestones

Per-engine IBus hosting is deferred. The current integration hosts complete Fcitx 5
and IBus frameworks; a separate Fcitx5 addon host is outside the plan.

## Security notes

- Wayland fields marked sensitive are not grabbed; Fcitx5 fields marked sensitive
  bypass engines. XIM does not report field purpose, so it cannot provide this bypass.
- Key contents are never logged by default.
- Sockets live in a `0700` directory and peers are checked with `SO_PEERCRED`.
- Rime's user-data directory is created or restricted to `0700` before librime starts;
  if this fails, Rime leaves keys to applications. Its existing user database files
  are protected by this directory. Sparkle saves its recent emoji atomically as `0600`,
  restricting an older, more permissive history file on the next save, and creates or
  restricts its parent state directory to `0700`. Both directory paths may be
  symlinks: the links stay in place and their target directories receive `0700`
  before writes.
- Processes running as the same user are not a security boundary. Sandboxing engines (for
  example with bubblewrap) is future work.
