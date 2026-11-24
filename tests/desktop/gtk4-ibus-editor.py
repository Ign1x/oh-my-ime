#!/usr/bin/env python3
"""Tiny GTK4 entry used by the isolated GNOME/IBus acceptance probe."""

import json
import os
import sys

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402


OUTPUT = os.environ["OMIME_GTK_LOG"]


def record(kind, value):
    with open(OUTPUT, "a", encoding="utf-8") as stream:
        stream.write(f"{kind} {json.dumps(value, ensure_ascii=False)}\n")
        stream.flush()


class App(Gtk.Application):
    def __init__(self):
        super().__init__(application_id="io.github.ign1x.OhMyIme.Gtk4Probe")
        self.entry = None

    def do_activate(self):
        window = Gtk.ApplicationWindow(application=self)
        window.set_default_size(640, 120)
        self.entry = Gtk.Entry()
        self.entry.set_placeholder_text("oh-my-ime GTK4 native probe")
        def changed(widget):
            value = widget.get_text()
            record("text", value)
            # The acceptance probe only needs one committed phrase. Exit after it
            # reaches the real GTK4 model so the probe can focus its next native
            # text field without compositor-specific window activation behavior.
            if value == "中国":
                GLib.idle_add(self.quit)

        self.entry.connect("changed", changed)
        window.set_child(self.entry)
        window.present()
        self.entry.grab_focus()
        record("ready", True)


if __name__ == "__main__":
    sys.exit(App().run([]))
