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
- **Popup lifecycle.** Each engine surface receives the input-popup role once, right
  before the commit that should map it, and keeps it for life. Windows are hidden until
  their engine calls `SetPopupVisible(true)`, and only the active engine's windows are
  shown. To hide a window, the bridge injects `attach(NULL)` + `commit` and swallows the
  compositor's release of the last buffer, holding it on the compositor's behalf; showing
  the window again re-attaches that buffer instantly, without waiting for the toolkit to
  draw. Buffers committed while hidden replace the held one. While an X11 application has
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
  check (shm pools and buffers, bind versions, virtual objects) are validated, and a
  broken engine is disconnected on its own. Anything else that makes the compositor
  close the shared connection makes the daemon reconnect, bind the input method again
  and restart the engines; the end-to-end tests cover this.

## X11 applications

X11 applications never speak `text-input`, so the Wayland frontend cannot reach them. Two
X11 frontends do, and both feed the same core and the same engines; an engine does not
know which frontend a key came from.

- **XIM.** Built on the [`xim`](https://crates.io/crates/xim) and
  [`x11rb`](https://crates.io/crates/x11rb) crates. When `DISPLAY` is set at startup,
  omimed becomes the XIM server `omime` on that display; applications find it with
  `XMODIFIERS=@im=omime`. While the X server is not up, omimed retries with a growing
  delay (2 s, doubling up to 60 s), and it reconnects when the server goes away. Only key
  presses pass through XIM; releases reach the application directly. libX11 puts a commit
  or a key sent back at the front of the application's event queue, so two in a row would
  arrive in the wrong order: omimed sends them one at a time as synchronous requests and
  waits for the application's `XIM_SYNC_REPLY` (300 ms at most) before the next. Keysyms
  come from the X server's core keyboard mapping.
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

## Engine protocol (v1)

Engines connect to a private peer-to-peer D-Bus socket whose address is in
`OMIME_ENGINE_ADDRESS`.

- `io.github.ign1x.OhMyIme.Engine1` is exported by the engine at
  `/io/github/ign1x/OhMyIme/Engine`. It has `Activate`, `Deactivate`, `UpdateContext`,
  `ProcessKey`, `Reset` and `Shutdown`. `ProcessKey` returns `(handled, actions)`, where
  `actions` is an ordered list of commit, preedit, delete-surrounding and forward-key
  operations.
- `io.github.ign1x.OhMyIme.EngineHost1` is exported by the daemon at
  `/io/github/ign1x/OhMyIme`. It has `Register`, out-of-band actions (for example committing
  after a mouse click on a candidate), `SetPopupVisible`, `SetStatus` and `RequestSwitch`.
  Everything except `Register` is fire-and-forget.
- Key events time out after 200 ms by default. On timeout the key is forwarded to the
  application, and a late reply is discarded and followed by `Reset`.
- Engines must not hide their toolkit windows. They ask the platform with `SetPopupVisible`.
- The protocol is versioned by interface name (major) and negotiated at `Register` (minor).
  Extensible arguments use `a{sv}`, and unknown keys are ignored. The XML in `protocol/` is the
  source of truth.

## Hosted input methods

An unmodified Wayland input method, such as Fcitx 5, runs as an engine when its manifest
says `protocol = "input-method-v2"`. It has no engine-protocol socket: it connects to its
bridge socket like the UI of any engine, and its registry shows two globals more, a
virtual `zwp_input_method_manager_v2` and a virtual `zwp_virtual_keyboard_manager_v1`,
both answered by the bridge.

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
- **X11 fields.** Fields of XIM and Fcitx5-module applications use the input method of
  any seat. XIM passes only presses, so each delivered key is released right away, and
  modifier keys stay with the application: an input method would take a lone Shift for
  a tap.
- **Windows.** The input method's popup surfaces are engine windows, shown as input
  popups or on a mirror, and the text cursor rectangle reaches it through its popup
  role. Other windows of the input method (toplevels) cannot be shown.
- **Process.** omimed starts the input method on a private session bus, a `dbus-daemon`
  without service activation in the engine's runtime directory, so that it cannot take
  `org.fcitx.Fcitx5` on the user's bus from omimed's Fcitx5 frontend. It gets the
  `WAYLAND_DISPLAY` of its bridge socket and no `DISPLAY`. Engines get SIGTERM when
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
  `Ctrl+Shift+Space`, the tray menu). An adapter task runs beside each launch of a
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

## Tray item

omimed publishes one tray item on the session bus (`org.kde.StatusNotifierItem`, with a
`com.canonical.dbusmenu` menu) and registers it with the status bar's
`org.kde.StatusNotifierWatcher`, again whenever a watcher appears. The item keeps no
state: every property is read from the daemon loop, and the loop tells the item when the
current engine, its status or the engines change, which sends `NewIcon`, `NewToolTip` and
`LayoutUpdated`. The icon is the hosted input method's, else the engine's status or
manifest icon, else `input-keyboard`; the tooltip is "engine · status". A click toggles
direct input, a middle click switches to the next input method, and the menu holds the
input methods and direct input as radio items; for an input method inside a framework
the tooltip is "input method (framework)". `[tray] enabled = false` turns it off.

## Compatibility layers (planned)

- **Per-engine hosts:** an IBus engine host (private bus, component discovery, Factory/Engine
  translation) and a Fcitx5 addon host (C++, libFcitx5Core).

## Security notes

- Sensitive fields bypass the daemon entirely (no grab).
- Key contents are never logged by default.
- Sockets live in a `0700` directory and peers are checked with `SO_PEERCRED`.
- Processes running as the same user are not a security boundary. Sandboxing engines (for
  example with bubblewrap) is future work.
