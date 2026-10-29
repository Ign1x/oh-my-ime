# oh-my-ime architecture

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
  the daemon releases every key it still holds on its virtual keyboard. A new grab is created
  only after the old one is destroyed.
- **Keys.** Grab events are decoded with xkbcommon (keysym, UTF-8, modifiers). Hotkeys are
  handled first. The remaining keys go to the active engine, whose reply decides whether the key
  is consumed or forwarded through the virtual keyboard. Releases of keys whose press was not
  consumed are always forwarded, including keys pressed before the grab started.
- **Repeat.** Applications repeat forwarded keys themselves. The daemon synthesises repeats,
  using `repeat_info`, only for keys an engine consumed, and turns that off if the compositor
  sends its own repeat events.
- **Commit discipline.** After every `done`, the daemon sends a commit that carries the current
  preedit, whether or not anything changed.

## The UI Bridge

The bridge is a multiplexing Wayland proxy built on [`wl-proxy`](https://crates.io/crates/wl-proxy).
All engine connections share the daemon's upstream connection, so their surfaces are, from the
compositor's point of view, the input method's own surfaces.

- **Per-engine sockets.** Each engine gets `$XDG_RUNTIME_DIR/oh-my-ime/<engine-id>/wayland`
  (directory mode `0700`). `WAYLAND_DISPLAY` is set to that relative name, and `DISPLAY` is
  removed so toolkits cannot silently fall back to X11. The daemon accepts connections itself,
  so it always knows which engine a client belongs to.
- **Global filtering.** Engines see a virtual registry. It contains the globals a UI needs
  (compositor, subcompositor, shm, seat without keyboard capability, outputs, linux-dmabuf,
  viewporter, fractional scale, cursor shape, single-pixel buffer, presentation time) and hides
  everything else (data devices, text input, input method, virtual keyboard, layer shell,
  capture protocols).
- **Virtual `xdg_shell`.** Engines create an ordinary frameless `xdg_toplevel`. The bridge never
  forwards xdg objects. It answers the initial commit with `configure(0, 0, [])` and dismisses
  `xdg_popup`s immediately.
- **Popup lifecycle.** Each engine surface receives the input-popup role once, right before the
  commit that should map it, and keeps that role for life. To hide it, the bridge injects
  `attach(NULL)` + `commit`. While hidden, it answers frame callbacks and releases buffers on
  the compositor's behalf, so toolkits never stall. To show it again, the bridge sends a
  configure and forwards the engine's next commit. Only the active engine's popup is visible.
- **Input routing.** Pointer `enter`, `motion`, `button`, `axis` and `frame` go only to the
  engine whose surface the pointer entered. `wl_surface.enter(output)` and similar events are
  filtered by owner.
- **Cleanup.** When an engine disconnects or dies, its compositor objects are destroyed in
  dependency order: popup role, then surface extensions, subsurfaces, surfaces, buffers and
  pools, and finally seat and output objects. If an engine destroys a surface itself, the popup
  role is destroyed first.
- **Failure handling.** A protocol error on a virtual object only disconnects that engine. A
  protocol error on the shared connection is fatal to it; the daemon then rebuilds the
  connection, binds the input method again and restarts the engines.

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

## Compatibility layers (planned)

- **X11 / XWayland:** an XIM bridge (`omime-xim`), plus a server for the Fcitx5 IM-module
  D-Bus protocol so that existing `fcitx5-gtk`/`fcitx5-qt` modules work unchanged. Popups for X11
  contexts are placed with layer-shell using compositor-specific coordinate mapping.
- **Virtual `input-method-v2`:** the bridge can offer a virtual input method to a hosted client.
  This lets unmodified Fcitx5, IBus (≥ 1.5.32) and other input-method-v2 programs run as one
  oh-my-ime engine with their own UI.
- **Per-engine hosts:** an IBus engine host (private bus, component discovery, Factory/Engine
  translation) and a Fcitx5 addon host (C++, libFcitx5Core).

## Security notes

- Sensitive fields bypass the daemon entirely (no grab).
- Key contents are never logged by default.
- Sockets live in a `0700` directory and peers are checked with `SO_PEERCRED`.
- Processes running as the same user are not a security boundary. Sandboxing engines (for
  example with bubblewrap) is future work.
