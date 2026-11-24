#!/usr/bin/env python3
"""Exercise omimed and headless Rime through a real, isolated IBus daemon.

Run in the e2e development shell after building the workspace:
  python3 tests/desktop/native-ibus.py --out /tmp/omime-ibus-acceptance

This is an IBus protocol acceptance test. It does not start GNOME Shell or establish
application, launcher, accessibility, or physical-display acceptance.
"""
from __future__ import annotations

import argparse
from collections import deque
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
import traceback
from xml.sax.saxutils import escape


def load_ibus():
    # Nix's ibus-with-plugins wrapper has the typelib beside its executable, but it
    # need not be on GI_TYPELIB_PATH. Resolve the typelib symlink to the base output.
    executable = shutil.which("ibus-daemon")
    if executable:
        root = Path(executable).resolve().parent.parent
        typelib = root / "lib/girepository-1.0/IBus-1.0.typelib"
        if typelib.exists():
            search = str(typelib.resolve().parent)
            os.environ["GI_TYPELIB_PATH"] = search + os.pathsep + os.environ.get(
                "GI_TYPELIB_PATH", ""
            )
    import gi

    gi.require_version("IBus", "1.0")
    from gi.repository import GLib, IBus

    IBus.init()
    return GLib, IBus


class Probe:
    def __init__(self, args):
        self.args = args
        self.out = args.out.resolve()
        self.out.mkdir(parents=True, exist_ok=False)
        self.children = []
        self.logs = []
        self.events = []
        self.steps = []
        self.GLib = None
        self.IBus = None
        self.bus = None
        self.ic = None
        self.panel = None
        self.commits = []
        # libibus inspects incoming floating objects again after emitting its
        # signal. Keep their GI wrappers alive past callback return so PyGObject
        # cannot finalize them before that C-side check; bound retained history.
        self.gi_values = deque(maxlen=32)
        self.preedit = ""
        self.lookup = {"visible": False, "candidates": [], "cursor": 0}
        self.environment = os.environ.copy()
        for name in (
            "DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "IBUS_ADDRESS",
            "DBUS_SESSION_BUS_ADDRESS", "DBUS_STARTER_ADDRESS", "DBUS_STARTER_BUS_TYPE",
            "GTK_IM_MODULE", "QT_IM_MODULE", "XMODIFIERS", "OMIME_PRESENTATION",
        ):
            self.environment.pop(name, None)
        for name, relative in (
            ("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"), ("XDG_STATE_HOME", "state"),
            ("XDG_RUNTIME_DIR", "run"),
        ):
            path = self.out / relative
            path.mkdir(mode=0o700)
            self.environment[name] = str(path)
        (self.out / "components").mkdir()
        self.environment.update(
            GSETTINGS_BACKEND="memory",
            IBUS_COMPONENT_PATH=str(self.out / "components"),
            OMIME_ENGINE_PATH=str(self.args.project / "engines"),
            PATH=str(self.args.bin_dir) + os.pathsep + self.environment.get("PATH", ""),
            OMIME_LOG=self.environment.get("OMIME_LOG", "omimed=debug"),
        )
        config = self.out / "config/oh-my-ime"
        config.mkdir(mode=0o700)
        (config / "config.toml").write_text(
            '[panel]\nenabled = false\n'
        )
        self.started = time.monotonic()

    def event(self, kind, **data):
        self.events.append({"at_seconds": round(time.monotonic() - self.started, 3),
                            "kind": kind, **data})

    def spawn(self, name, command):
        log = (self.out / f"{name}.log").open("wb")
        self.logs.append(log)
        process = subprocess.Popen(
            command, env=self.environment, stdout=log, stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        self.children.append((name, process))
        self.event("process-start", name=name, pid=process.pid, command=command)
        return process

    def pump(self):
        if self.GLib:
            context = self.GLib.MainContext.default()
            # A producer must not keep the loop spinning forever.
            for _ in range(512):
                if not context.pending():
                    break
                context.iteration(False)
        for name, process in self.children:
            if process.poll() is not None:
                raise RuntimeError(f"{name} exited with status {process.returncode}; see {name}.log")

    def wait(self, description, predicate, timeout=None):
        deadline = time.monotonic() + (timeout or self.args.timeout)
        while time.monotonic() < deadline:
            self.pump()
            result = predicate()
            if result:
                return result
            time.sleep(0.01)
        raise TimeoutError(f"timed out waiting for {description}")

    def settle(self, seconds=0.15):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.pump()
            time.sleep(0.005)

    def check(self, name, action):
        self.event("check-start", name=name)
        started = time.monotonic()
        action()
        self.steps.append({"name": name, "status": "passed",
                           "seconds": round(time.monotonic() - started, 3)})
        self.event("check-pass", name=name)

    def start(self):
        runtime = self.out / "run"
        config = self.out / "session-bus.conf"
        # No servicedir or included configuration: activation cannot launch any
        # service from the user's desktop. This bus belongs only to this probe.
        config.write_text(
            '<busconfig><type>session</type><listen>unix:path='
            + escape(str(runtime / "session.sock"))
            + '</listen><auth>EXTERNAL</auth><policy context="default">'
            '<allow send_destination="*"/><allow receive_sender="*"/>'
            '<allow own="*"/></policy></busconfig>\n'
        )
        self.spawn("session-bus", ["dbus-daemon", "--nofork", "--config-file", str(config)])
        self.wait("private session bus", lambda: (runtime / "session.sock").exists(), 10)
        self.environment["DBUS_SESSION_BUS_ADDRESS"] = "unix:path=" + str(runtime / "session.sock")
        self.environment["IBUS_ADDRESS"] = "unix:path=" + str(runtime / "ibus.sock")
        self.spawn("ibus", ["ibus-daemon", "--single", "--panel", "disable",
                            "--config", "disable", "--cache", "none", "--address",
                            self.environment["IBUS_ADDRESS"]])
        self.wait("private IBus socket", lambda: (runtime / "ibus.sock").exists(), 10)
        # GI reads these only in this process; no launch environment is changed.
        os.environ.clear()
        os.environ.update(self.environment)
        self.GLib, self.IBus = load_ibus()
        self.bus = self.IBus.Bus.new()
        self.wait("private IBus connection", self.bus.is_connected, 10)
        self.bus.get_connection().set_exit_on_close(False)
        if self.args.monitor:
            self.spawn("ibus-wire", ["dbus-monitor", "--address", self.environment["IBUS_ADDRESS"]])
        self.install_panel()
        self.spawn("omimed", [str(self.args.bin_dir / "omimed"), "--ibus", "--engine", self.args.engine])
        self.wait("oh-my-ime IBus registration", lambda: any(
            engine.get_name() == "oh-my-ime" for engine in self.bus.list_active_engines()
        ))
        self.ic = self.bus.create_input_context("oh-my-ime-native-acceptance")
        if self.ic is None:
            raise RuntimeError("IBus failed to create an input context")
        self.ic.connect("commit-text", self.on_commit)
        self.ic.connect("update-preedit-text", self.on_preedit)
        self.ic.connect("hide-preedit-text", lambda *_: self.clear_preedit())
        # The client draws preedit; the panel draws candidates so real panel
        # CandidateClicked/PageUp/PageDown signals traverse IBus's normal path.
        self.ic.set_capabilities(int(self.IBus.Capabilite.PREEDIT_TEXT
                                     | self.IBus.Capabilite.FOCUS
                                     | self.IBus.Capabilite.SURROUNDING_TEXT))
        self.ic.focus_in()
        self.ic.set_engine("oh-my-ime")
        if not self.bus.set_global_engine("oh-my-ime"):
            raise RuntimeError("IBus rejected oh-my-ime as the global engine")
        self.wait("oh-my-ime selected", lambda: self.ic.get_engine()
                  and self.ic.get_engine().get_name() == "oh-my-ime", 15)
        self.ic.set_content_type(int(self.IBus.InputPurpose.FREE_FORM), 0)
        self.settle()
        if self.args.engine == "rime":
            self.wait("Rime deployment", lambda: "rime/luna_pinyin" in self.omime("list", "--all"))
            self.omime("switch", "rime/luna_pinyin")
            self.wait("Rime pinyin selection", lambda: self.omime("status").startswith("rime/luna_pinyin"))
        self.event("environment", headless=True,
                   ibus_version=".".join(str(getattr(self.IBus, name, "?")) for name in
                                         ("MAJOR_VERSION", "MINOR_VERSION", "MICRO_VERSION")),
                   input_context=self.ic.get_object_path())

    def install_panel(self):
        self.panel = self.IBus.PanelService.new(self.bus.get_connection())
        self.panel.connect("update-lookup-table", lambda _panel, table, visible:
                           self.on_lookup(table, visible))
        self.panel.connect("hide-lookup-table", lambda *_: self.hide_lookup())
        self.panel.connect("focus-in", lambda _panel, path:
                           self.event("panel-focus-in", path=path))
        self.panel.connect("focus-out", lambda _panel, path:
                           self.panel_focus_out(path))
        result = self.bus.request_name("org.freedesktop.IBus.Panel", 0)
        if result != 1:
            raise RuntimeError(f"private IBus panel name was not acquired ({result})")

    def hide_lookup(self):
        self.lookup["visible"] = False
        self.event("lookup-hidden")

    def panel_focus_out(self, path):
        self.lookup["visible"] = False
        self.event("panel-focus-out", path=path)

    def on_commit(self, _ic, text):
        self.gi_values.append(text)
        value = text.get_text()
        # The application replaces its displayed preedit with committed text.
        self.preedit = ""
        self.commits.append(value)
        self.event("commit", text=value)

    def on_preedit(self, _ic, text, cursor, visible):
        self.gi_values.append(text)
        self.preedit = text.get_text() if visible else ""
        self.event("preedit", text=text.get_text(), cursor=cursor, visible=bool(visible))

    def clear_preedit(self):
        self.preedit = ""
        self.event("preedit-hidden")

    def on_lookup(self, table, visible):
        self.gi_values.append(table)
        candidates = []
        for index in range(table.get_number_of_candidates()):
            label = table.get_label(index)
            candidates.append({"text": table.get_candidate(index).get_text(),
                               "label": label.get_text() if label else ""})
        self.lookup = {"visible": bool(visible), "candidates": candidates,
                       "cursor": table.get_cursor_pos()}
        self.event("lookup", **self.lookup)

    def omime(self, *args):
        result = subprocess.run([str(self.args.bin_dir / "omime"), *args], env=self.environment,
                                text=True, capture_output=True, timeout=5, check=True)
        return result.stdout.strip()

    def key(self, character, state=0):
        keysym = {" ": 0x20, "Escape": 0xFF1B, "BackSpace": 0xFF08}.get(character)
        if keysym is None:
            keysym = self.IBus.unicode_to_keyval(character)
        # IBus keycodes use evdev indices. These are standard US letter positions;
        # Rime primarily consumes keysyms, and zero is valid for an unknown keycode.
        keycodes = {"q": 16, "w": 17, "e": 18, "r": 19, "t": 20, "y": 21,
                    "u": 22, "i": 23, "o": 24, "p": 25, "a": 30, "s": 31,
                    "d": 32, "f": 33, "g": 34, "h": 35, "j": 36, "k": 37,
                    "l": 38, "z": 44, "x": 45, "c": 46, "v": 47, "b": 48,
                    "n": 49, "m": 50, " ": 57, "Escape": 1, "BackSpace": 14}
        start = time.monotonic()
        handled = self.ic.process_key_event(keysym, keycodes.get(character, 0), state)
        self.settle(0.02)
        self.event("key", key=character, state=state, handled=bool(handled),
                   milliseconds=round(1000 * (time.monotonic() - start), 2))
        self.ic.process_key_event(keysym, keycodes.get(character, 0), state | (1 << 30))
        self.settle(0.01)
        return bool(handled)

    def type(self, text):
        for character in text:
            self.key(character)

    def reset(self):
        self.ic.reset()
        self.settle()
        self.commits.clear()

    def composition(self):
        self.type("zhongguo" if self.args.engine == "rime" else "hello")
        self.wait("nonempty inline preedit", lambda: self.preedit, 5)
        if self.args.engine == "rime":
            self.wait("native lookup candidates", lambda: self.lookup["visible"]
                      and self.lookup["candidates"], 5)
            assert self.lookup["candidates"][0]["text"].startswith("中国"), self.lookup
        handled = self.key(" ")
        if self.args.engine == "echo":
            assert not handled, "Echo's boundary Space was consumed after committing its preedit"
        expected = "中国" if self.args.engine == "rime" else "HELLO"
        self.wait(f"commit {expected}", lambda: "".join(self.commits) == expected, 5)
        self.wait("cleared preedit", lambda: not self.preedit, 5)
        if self.args.engine == "rime":
            self.wait("hidden lookup after commit", lambda: not self.lookup["visible"], 5)
        assert len(self.commits) == 1, self.commits

    def pointer_and_pages(self):
        self.reset()
        self.type("shi")
        self.wait("candidate page", lambda: self.lookup["visible"]
                  and len(self.lookup["candidates"]) >= 2, 5)
        first = list(self.lookup["candidates"])
        lookup_events = sum(event["kind"] == "lookup" for event in self.events)
        self.panel.page_down()
        self.wait("following candidate page", lambda: self.lookup["candidates"] != first, 5)
        self.panel.page_up()
        self.wait("preceding candidate page", lambda: self.lookup["candidates"] == first, 5)
        assert sum(event["kind"] == "lookup" for event in self.events) >= lookup_events + 2
        selected = self.lookup["candidates"][1]["text"]
        # Comments may be displayed beside candidate text. For the schema used by
        # this probe they are empty; record the chosen label/text in the evidence.
        self.event("candidate-click", index=1, button=1, text=selected)
        self.panel.candidate_clicked(1, 1, 0)
        self.wait("pointer-selected commit", lambda: bool(self.commits), 5)
        assert "".join(self.commits) == selected, (self.commits, selected)
        self.wait("hidden lookup after click", lambda: not self.lookup["visible"], 5)

    def sensitive(self):
        fields = [(int(self.IBus.InputPurpose.PASSWORD), 0),
                  (int(self.IBus.InputPurpose.PIN), 0),
                  (int(self.IBus.InputPurpose.FREE_FORM), 1 << 11),
                  (int(self.IBus.InputPurpose.FREE_FORM), 1 << 12)]
        for purpose, hints in fields:
            self.reset()
            self.ic.set_content_type(purpose, hints)
            self.settle()
            mark = len(self.events)
            handled = [self.key(character) for character in "secret"]
            assert not any(handled), (purpose, hints, handled)
            assert not self.commits, self.commits
            assert not self.preedit and not self.lookup["visible"]
            assert not any(event["kind"] == "commit" or
                           (event["kind"] == "preedit" and event["visible"]) or
                           (event["kind"] == "lookup" and event["visible"])
                           for event in self.events[mark:])
        self.ic.set_content_type(int(self.IBus.InputPurpose.FREE_FORM), 0)
        self.settle()

    def unhandled_keys(self):
        self.reset()
        assert not self.key("c", 1 << 2), "Control+C was consumed without a composition"
        assert not self.key("Escape"), "Escape was consumed without a composition"
        assert not self.commits and not self.preedit and not self.lookup["visible"]

    def focus_cleanup(self):
        self.reset()
        self.type("ni" if self.args.engine == "rime" else "stale")
        self.wait("composition before focus out", lambda: self.preedit, 5)
        self.ic.focus_out()
        self.wait("preedit cleared after focus out", lambda: not self.preedit, 5)
        if self.args.engine == "rime":
            self.wait("lookup cleared after focus out", lambda: not self.lookup["visible"], 5)
        self.ic.focus_in()
        self.settle()
        self.key(" ")
        assert not self.commits, self.commits
        assert not self.preedit and not self.lookup["visible"]

    def run(self):
        result = {"route": "system-ibus-engine", "engine": self.args.engine,
                  "at_utc": datetime.now(timezone.utc).isoformat(),
                  "scope": "real IBus protocol, headless; GNOME Shell and graphical apps not tested"}
        def git(*args):
            return subprocess.run(["git", "-C", str(self.args.project), *args], text=True,
                                  capture_output=True, timeout=5).stdout.strip()
        result["source"] = {"revision": git("rev-parse", "HEAD"),
                            "dirty": bool(git("status", "--porcelain")),
                            "probe_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                            "binary_sha256": {name: hashlib.sha256(
                                (self.args.bin_dir / name).read_bytes()).hexdigest()
                                for name in ("omimed", "omime", "omime-" + self.args.engine)}}
        try:
            self.check("private IBus registration and headless Rime deployment", self.start)
            self.check("preedit, candidates and one ordered Unicode commit", self.composition)
            if self.args.engine == "rime":
                self.check("IBus panel candidate click and page callbacks", self.pointer_and_pages)
            self.check("password, PIN, private and hidden field bypass", self.sensitive)
            self.check("focus clears composition and candidates", self.focus_cleanup)
            self.check("unhandled shortcuts and cancellation keys fail open", self.unhandled_keys)
            result["status"] = "passed"
        except Exception as error:
            result.update(status="failed", error=str(error), traceback=traceback.format_exc())
            self.steps.append({"name": next((event["name"] for event in reversed(self.events)
                                              if event["kind"] == "check-start"), "startup"),
                               "status": "failed", "error": str(error)})
        finally:
            if self.ic:
                try:
                    self.ic.focus_out()
                except Exception:
                    pass
            for _name, process in reversed(self.children):
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait(timeout=5)
            for log in self.logs:
                log.close()
            result.update(checks=self.steps, events=self.events,
                          seconds=round(time.monotonic() - self.started, 3))
            (self.out / "result.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
            print(json.dumps({"status": result["status"], "result": str(self.out / "result.json"),
                              **({"error": result["error"]} if "error" in result else {})},
                             ensure_ascii=False))
        return 0 if result["status"] == "passed" else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True, help="new directory for private state and evidence")
    parser.add_argument("--project", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--bin-dir", type=Path, help="built omimed, omime and omime-rime directory")
    parser.add_argument("--engine", choices=("rime", "echo"), default="rime")
    parser.add_argument("--timeout", type=float, default=150, help="deployment/registration deadline in seconds")
    parser.add_argument("--monitor", action="store_true", help="record private IBus wire messages")
    args = parser.parse_args()
    args.project = args.project.resolve()
    args.bin_dir = (args.bin_dir or args.project / "target/debug").resolve()
    for executable in ("omimed", "omime", "omime-" + args.engine):
        if not (args.bin_dir / executable).is_file():
            parser.error(f"missing {args.bin_dir / executable}; build the workspace first")
    if len(str(args.out.resolve() / "run/session.sock")) > 100:
        parser.error("--out is too long for a Unix socket path; use a short /tmp directory")
    return Probe(args).run()


if __name__ == "__main__":
    sys.exit(main())
