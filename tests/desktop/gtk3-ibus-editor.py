#!/usr/bin/env python3
"""Tiny GTK3 entry used by the isolated GNOME/IBus acceptance probe."""

import json
import os
import sys

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import GLib, Gtk  # noqa: E402


OUTPUT = os.environ["OMIME_GTK_LOG"]


def record(kind, value):
    with open(OUTPUT, "a", encoding="utf-8") as stream:
        stream.write(f"{kind} {json.dumps(value, ensure_ascii=False)}\n")
        stream.flush()


class App(Gtk.Application):
    def __init__(self):
        super().__init__(application_id="io.github.ign1x.OhMyIme.Gtk3Probe")

    def do_activate(self):
        window = Gtk.ApplicationWindow(application=self)
        window.set_default_size(640, 120)
        entry = Gtk.Entry()
        entry.set_placeholder_text("oh-my-ime GTK3 native probe")

        def changed(widget):
            value = widget.get_text()
            record("text", value)
            if value == "中国":
                GLib.idle_add(self.quit)

        entry.connect("changed", changed)
        window.add(entry)
        window.show_all()
        entry.grab_focus()
        record("ready", True)


if __name__ == "__main__":
    sys.exit(App().run([]))
