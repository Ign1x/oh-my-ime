#!/usr/bin/env python3
"""Shout — an oh-my-ime engine written in Python with PyGObject and GTK 4.

It speaks the engine protocol (io.github.ign1x.OhMyIme.Engine1) directly over the
peer-to-peer D-Bus connection oh-my-ime gives it, without the Rust SDK. The toolkit
window is a Gtk.Window: the platform turns it into a popup next to the text cursor.

Typed a-z letters compose a word. The field gets the word as a lowercase preedit, the
window shows it in UPPERCASE. Space, Return or any other key commits what is there and
then reaches the application; Backspace edits the word and Escape cancels it. A click
on the window commits too.
"""

import os
import sys
import traceback

import gi

gi.require_version("Gdk", "4.0")
gi.require_version("Gtk", "4.0")
from gi.repository import Gdk, Gio, GLib, Gtk  # noqa: E402  (after require_version)

# Names and paths of the protocol; they are fixed by omime-proto.
ENGINE_PATH = "/io/github/ign1x/OhMyIme/Engine"
HOST_PATH = "/io/github/ign1x/OhMyIme"
ENGINE_INTERFACE = "io.github.ign1x.OhMyIme.Engine1"
HOST_INTERFACE = "io.github.ign1x.OhMyIme.EngineHost1"

# Keysyms we act on (see xkbcommon-keysyms.h).
BACKSPACE = 0xFF08
ESCAPE = 0xFF1B

# Modifier bits of the protocol's key event. Shift and CapsLock stay out: Shift+A is
# still a letter, and we hand the layout's text back unchanged.
SHORTCUT_MODIFIERS = 4 | 8 | 64  # Control, Alt, Super

# Action kinds; unknown kinds would be ignored by the daemon.
ACTION_COMMIT = 0
ACTION_PREEDIT = 1

# The window's colors; tests look for this green.
BACKGROUND = "#2e7d32"

# Only the methods this engine needs. It registers with minor version 0, so the daemon
# never calls the 1.1 additions (input methods of its own, a menu).
ENGINE_XML = f"""
<node>
  <interface name="{ENGINE_INTERFACE}">
    <method name="Activate">
      <arg name="context" type="t" direction="in"/>
      <arg name="info" type="a{{sv}}" direction="in"/>
      <arg name="actions" type="a(uv)" direction="out"/>
    </method>
    <method name="Deactivate">
      <arg name="context" type="t" direction="in"/>
    </method>
    <method name="UpdateContext">
      <arg name="context" type="t" direction="in"/>
      <arg name="changes" type="a{{sv}}" direction="in"/>
      <arg name="actions" type="a(uv)" direction="out"/>
    </method>
    <method name="ProcessKey">
      <arg name="context" type="t" direction="in"/>
      <arg name="key" type="(uuuusbb)" direction="in"/>
      <arg name="extras" type="a{{sv}}" direction="in"/>
      <arg name="handled" type="b" direction="out"/>
      <arg name="actions" type="a(uv)" direction="out"/>
    </method>
    <method name="Reset">
      <arg name="context" type="t" direction="in"/>
      <arg name="actions" type="a(uv)" direction="out"/>
    </method>
    <method name="Shutdown"/>
  </interface>
</node>
"""


def preedit(text: str):
    """A preedit action with the cursor at the end of `text` (offsets are bytes)."""
    end = len(text.encode("utf-8"))
    return (ACTION_PREEDIT, GLib.Variant("(sii)", (text, end, end)))


def commit(text: str):
    """A commit action; inserting text also replaces the preedit in the field."""
    return (ACTION_COMMIT, GLib.Variant("s", text))


class Shout:
    def __init__(self, connection: Gio.DBusConnection):
        self.connection = connection
        # One word per text field: several fields can be focused at once.
        self.words: dict[int, str] = {}
        # The field whose word the single window is displaying, and whether we asked
        # the platform to show the window.
        self.shown: int | None = None
        self.visible = False
        self.loop = GLib.MainLoop()

        self.window = Gtk.Window()
        self.window.set_decorated(False)
        self.window.set_resizable(False)
        self.window.set_default_size(240, 48)
        # Green everywhere, not only behind the label: CSS on the toplevel paints the
        # whole surface.
        provider = Gtk.CssProvider()
        provider.load_from_data(
            f"window {{ background-color: {BACKGROUND}; }}"
            "label { color: #ffffff; font-size: 30px; font-weight: bold; }".encode()
        )
        Gtk.StyleContext.add_provider_for_display(
            Gdk.Display.get_default(),
            provider,
            Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION,
        )
        self.label = Gtk.Label()
        self.label.set_hexpand(True)
        self.label.set_vexpand(True)
        self.window.set_child(self.label)

        # A click anywhere on the window commits, like a candidate in other engines.
        click = Gtk.GestureClick.new()
        click.connect("pressed", self._on_click)
        self.window.add_controller(click)

        # The window is always mapped for the toolkit; SetPopupVisible decides whether
        # the user sees it. Never hide or close it ourselves.
        self.window.present()

    # ---- Talking to the platform ------------------------------------------------

    def _call_host(self, method: str, parameters: GLib.Variant):
        """Sends a host call without waiting: callbacks must not block the daemon."""
        self.connection.call(
            None,  # peer-to-peer: there is no bus name to address
            HOST_PATH,
            HOST_INTERFACE,
            method,
            parameters,
            None,
            Gio.DBusCallFlags.NONE,
            -1,
            None,
            None,
            None,
        )

    def _set_visible(self, visible: bool):
        if visible == self.visible:
            return
        self.visible = visible
        self._call_host("SetPopupVisible", GLib.Variant("(b)", (visible,)))

    def _show_word(self, context: int):
        """Shows the word of `context`, or hides the window if it just became empty."""
        word = self.words.get(context, "")
        if word:
            self.shown = context
            self.label.set_text(word.upper())
            self._set_visible(True)
        elif self.shown == context:
            self.shown = None
            self.label.set_text("")
            self._set_visible(False)

    def _forget(self, context: int):
        self.words.pop(context, None)
        if self.shown == context:
            self.shown = None
            self.label.set_text("")
            self._set_visible(False)

    # ---- Engine methods ---------------------------------------------------------

    def activate(self, context: int) -> list:
        self._forget(context)
        return []

    def deactivate(self, context: int):
        self._forget(context)

    def update_context(self, context: int) -> list:
        # The window follows the field on its own: the platform places it at the text
        # cursor and sends the cursor rectangle in `info`.
        return []

    def reset(self, context: int) -> list:
        # The application changed the text, so the composition is meaningless.
        self._forget(context)
        return []

    def process_key(self, context: int, key) -> tuple[bool, list]:
        keysym, _keycode, modifiers, _time, text, released, _repeat = key

        # Releases are routed like their presses; they never change anything.
        if released:
            return False, []

        word = self.words.get(context, "")
        shortcut = modifiers & SHORTCUT_MODIFIERS

        if shortcut == 0 and len(text) == 1 and "a" <= text <= "z":
            word += text
            self.words[context] = word
            self._show_word(context)
            return True, [preedit(word)]

        if keysym == BACKSPACE and word:
            word = word[:-1]
            self.words[context] = word
            self._show_word(context)
            return True, [preedit(word)]

        if keysym == ESCAPE and word:
            self._forget(context)
            return True, [preedit("")]

        # Anything else (Space and Return included, like the echo engine): commit what
        # we have, then let the application handle the key too.
        if word:
            self.words.pop(context, None)
            self._show_word(context)
            return False, [commit(word.upper())]
        return False, []

    def _on_click(self, _gesture, _n_press, _x, _y):
        if self.shown is None:
            return
        word = self.words.get(self.shown, "")
        if not word:
            return
        # A click arrives outside of a key callback, so it goes through the host.
        context = self.shown
        self._call_host("Commit", GLib.Variant("(ts)", (context, word.upper())))
        self.words.pop(context, None)
        self._show_word(context)

    # ---- D-Bus plumbing ---------------------------------------------------------

    def on_method_call(
        self, _connection, _sender, _path, _interface, method, parameters, invocation
    ):
        try:
            args = parameters.unpack() if parameters is not None else ()
            match method:
                case "Activate":
                    actions = self.activate(args[0])
                    invocation.return_value(GLib.Variant("(a(uv))", (actions,)))
                case "UpdateContext":
                    actions = self.update_context(args[0])
                    invocation.return_value(GLib.Variant("(a(uv))", (actions,)))
                case "ProcessKey":
                    handled, actions = self.process_key(args[0], args[1])
                    invocation.return_value(
                        GLib.Variant("(ba(uv))", (handled, actions))
                    )
                case "Reset":
                    actions = self.reset(args[0])
                    invocation.return_value(GLib.Variant("(a(uv))", (actions,)))
                case "Deactivate":
                    self.deactivate(args[0])
                    invocation.return_value(None)
                case "Shutdown":
                    invocation.return_value(None)
                    self.loop.quit()
                case _:
                    invocation.return_error_literal(
                        Gio.DBusError,
                        Gio.DBusError.UNKNOWN_METHOD,
                        f"unknown method {method}",
                    )
        except Exception as error:  # keep the engine alive and say what went wrong
            traceback.print_exc()
            invocation.return_error_literal(
                Gio.DBusError, Gio.DBusError.FAILED, str(error)
            )


def main() -> int:
    try:
        address = os.environ["OMIME_ENGINE_ADDRESS"]
        engine_id = os.environ["OMIME_ENGINE_ID"]
    except KeyError as missing:
        print(f"shout: missing {missing}", file=sys.stderr)
        return 1

    Gtk.init()
    connection = Gio.DBusConnection.new_for_address_sync(
        address,
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT,
        None,
        None,
    )
    if connection is None:
        print("shout: cannot connect to the engine socket", file=sys.stderr)
        return 1

    shout = Shout(connection)
    node = Gio.DBusNodeInfo.new_for_xml(ENGINE_XML)
    # GLib 2.84 renamed the call that takes Python callables; older ones only have the
    # first name, which newer PyGObject marks deprecated.
    register = getattr(connection, "register_object_with_closures2", None)
    if register is None:
        register = connection.register_object
    register(ENGINE_PATH, node.interfaces[0], shout.on_method_call, None, None)
    # omimed closed the connection: there is nobody left to serve.
    connection.connect("closed", lambda _connection: shout.loop.quit())

    # The object must exist before Register: the daemon may call it right away.
    try:
        connection.call_sync(
            None,
            HOST_PATH,
            HOST_INTERFACE,
            "Register",
            GLib.Variant("(sqqa{sv})", (engine_id, 1, 0, {})),
            GLib.VariantType.new("(a{sv})"),
            Gio.DBusCallFlags.NONE,
            -1,
            None,
        )
    except GLib.Error as error:
        print(f"shout: registration failed: {error.message}", file=sys.stderr)
        connection.close_sync(None)
        return 1

    try:
        shout.loop.run()
    finally:
        connection.close_sync(None)
    return 0


if __name__ == "__main__":
    sys.exit(main())
