#!/usr/bin/env python3
"""Exercise real KWin's privileged IM v1 connection in an isolated virtual session."""
import argparse
import json
import os
from pathlib import Path
import runpy
import socket
import struct
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
shared = runpy.run_path(str(ROOT / "scripts/desktop-probe.py"))


def string(value):
    data = value.encode() + b"\0"
    return struct.pack("=I", len(data)) + data + b"\0" * (-len(data) % 4)


class Keyboard:
    """KWin's test-only fake-input protocol; used only on our own compositor."""
    def __init__(self, env):
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(5)
        self.socket.connect(str(Path(env["XDG_RUNTIME_DIR"]) / env["WAYLAND_DISPLAY"]))
        self.send(1, 1, struct.pack("=I", 2))
        self.send(1, 0, struct.pack("=I", 3))
        pending, global_id = b"", None
        done = False
        while not done:
            pending += self.socket.recv(65536)
            while len(pending) >= 8:
                obj, header = struct.unpack_from("=II", pending)
                size, opcode = header >> 16, header & 65535
                if size < 8 or size % 4:
                    raise RuntimeError("invalid Wayland frame")
                if len(pending) < size:
                    break
                body, pending = pending[8:size], pending[size:]
                if obj == 2 and opcode == 0:
                    name, length = struct.unpack_from("=II", body)
                    interface = body[8:8+length-1].decode()
                    version = struct.unpack_from("=I", body, 8 + ((length+3)//4)*4)[0]
                    if interface == "org_kde_kwin_fake_input" and version >= 4:
                        global_id = name
                if obj == 3:
                    done = True
        if global_id is None:
            raise RuntimeError("KWin does not expose its test input protocol")
        self.send(2, 0, struct.pack("=I", global_id) + string("org_kde_kwin_fake_input") + struct.pack("=II", 4, 4))
        self.send(4, 0, string("oh-my-ime isolated test") + string("Native input regression"))

    def send(self, obj, opcode, body):
        self.socket.sendall(struct.pack("=II", obj, ((len(body)+8)<<16)|opcode) + body)

    def key(self, code):
        self.send(4, 10, struct.pack("=II", code, 1))
        time.sleep(.03)
        self.send(4, 10, struct.pack("=II", code, 0))
        time.sleep(.03)

    def type(self, text):
        keys = dict(zip("qwertyuiop", range(16,26))) | dict(zip("asdfghjkl",range(30,39))) | dict(zip("zxcvbnm",range(44,51))) | {" ":57}
        for ch in text:
            self.key(keys[ch])

    def close(self):
        self.socket.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--kwin", default="kwin_wayland")
    parser.add_argument("--bin-dir", type=Path, default=ROOT/"target/debug")
    parser.add_argument("--screenshot-tool", help="Spectacle executable for a candidate panel screenshot")
    parser.add_argument("--capture-panel", action="store_true", help="Capture panel with KWin Screenshot2 (requires Gio)")
    parser.add_argument("--wire-log", action="store_true", help="Record synthetic test input and Wayland protocol messages")
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    env = shared["private_environment"](args.out)
    env.update(PATH=str(args.bin_dir.resolve())+os.pathsep+env["PATH"],
               XDG_CURRENT_DESKTOP="KDE", XDG_SESSION_DESKTOP="KDE", WAYLAND_DISPLAY="omime-kwin-test",
               OMIME_ENGINE_PATH=str(ROOT/"engines"), SLINT_BACKEND="winit-software",
               QT_QPA_PLATFORM="offscreen", KWIN_COMPOSE="Q",
               OMIME_LOG="omimed=debug,wl_proxy=error")
    if args.wire_log:
        env["OMIME_WL_DEBUG"] = "1"
    session = shared["Session"](args.out, env)
    report = {"desktop":"KWin", "checks":{}, "scope":"isolated virtual output, real IM v1"}
    keyboard = None
    try:
        bus = args.out/"bus.conf"
        bus.write_text('<busconfig><type>session</type><listen>'+env["DBUS_SESSION_BUS_ADDRESS"]+'</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        session.start("dbus", ["dbus-daemon","--nofork",f"--config-file={bus}"])
        shared["wait_for"]("private bus",lambda:(args.out/"run/bus").exists())
        config = Path(env["XDG_CONFIG_HOME"])/"oh-my-ime"
        config.mkdir()
        (config/"config.toml").write_text('engines = ["echo", "rime"]\n[panel]\nenabled = false\n[tray]\nenabled = false\n')
        wrapper = args.out/"input-method"
        # shlex quotes only paths controlled by this test, never session values.
        import shlex
        wrapper.write_text("#!/bin/sh\nexec "+shlex.quote(str(args.bin_dir.resolve()/"omimed"))+" --engine echo >"+shlex.quote(str(args.out/"omimed.log"))+" 2>&1\n")
        wrapper.chmod(0o700)
        compositor = session.start("kwin",[args.kwin,"--virtual","--no-lockscreen","--no-global-shortcuts","--no-kactivities","--socket",env["WAYLAND_DISPLAY"],"--inputmethod",wrapper])
        shared["wait_for"]("KWin display",lambda:(Path(env["XDG_RUNTIME_DIR"])/env["WAYLAND_DISPLAY"]).exists())
        shared["wait_for"]("daemon control",lambda: session.run(["omime","status"]).returncode == 0)
        report["checks"]["startup"] = "passed"
        editor = session.start("editor",[args.bin_dir/"omime-ti-client"])
        log = args.out/"editor.log"
        shared["wait_for"]("editor focus", lambda: log.exists() and "ready" in log.read_text())
        daemon_log = args.out/"omimed.log"
        grab_marker = "input-method-v1 keyboard grabbed"
        shared["wait_for"]("input-method activation",lambda:grab_marker in daemon_log.read_text())
        keyboard = Keyboard(env)
        keyboard.type("hello ")
        shared["wait_for"]("Echo commit",lambda:'text "HELLO "' in log.read_text())
        report["checks"]["echo_preedit_commit"] = "passed"
        selected = session.run(["omime","switch","rime/luna_pinyin"])
        if selected.returncode:
            raise RuntimeError(selected.stderr)
        # The first launch deploys dictionaries before the schema becomes selectable.
        shared["wait_for"]("Rime deployment", lambda: "rime/luna_pinyin" in session.run(["omime","list","--all"]).stdout, 60)
        session.run(["omime","switch","rime/luna_pinyin"])
        shared["wait_for"]("Rime schema", lambda: session.run(["omime","status"]).stdout.startswith("rime/luna_pinyin"), 20)
        time.sleep(.2)
        keyboard.type("zhongguo")
        shared["wait_for"]("Rime preedit",lambda:'preedit "zhong guo"' in log.read_text(),30)
        if args.wire_log:
            shared["wait_for"]("candidate panel role",lambda:".set_overlay_panel()" in daemon_log.read_text())
            report["checks"]["candidate_panel_role"] = "passed"
        if args.capture_panel:
            # Every bus and output belongs to this temporary compositor. This
            # never requests a screenshot of the user's desktop.
            from gi.repository import Gio, GLib
            connection = Gio.DBusConnection.new_for_address_sync(
                env["DBUS_SESSION_BUS_ADDRESS"],
                Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
                None, None)
            raw_path = args.out/"rime-candidates.raw"
            with raw_path.open("wb") as output:
                descriptors = Gio.UnixFDList.new()
                index = descriptors.append(output.fileno())
                result, _ = connection.call_with_unix_fd_list_sync(
                    "org.kde.KWin.ScreenShot2", "/org/kde/KWin/ScreenShot2", "org.kde.KWin.ScreenShot2",
                    "CaptureActiveScreen", GLib.Variant("(a{sv}h)", ({"include-cursor": GLib.Variant("b", False)}, index)),
                    GLib.VariantType.new("(a{sv})"), Gio.DBusCallFlags.NONE, 10000, descriptors, None)
                metadata = result.unpack()[0]
                (args.out/"rime-candidates.json").write_text(json.dumps(metadata, indent=2)+"\n")
                shared["wait_for"]("candidate frame capture",lambda:raw_path.stat().st_size == metadata["stride"] * metadata["height"])
            report["checks"]["candidate_panel_capture"] = str(raw_path)
        if args.screenshot_tool:
            screenshot = args.out/"rime-candidates.png"
            capture = session.run([args.screenshot_tool, "--background", "--nonotify", "--fullscreen", "--output", screenshot], timeout=20, QT_QPA_PLATFORM="wayland")
            if capture.returncode:
                raise RuntimeError("candidate screenshot: "+capture.stderr)
            shared["wait_for"]("candidate screenshot",lambda:screenshot.exists() and screenshot.stat().st_size > 1024)
            report["checks"]["candidate_panel_capture"] = str(screenshot)
        keyboard.type(" ")
        shared["wait_for"]("Rime Chinese commit",lambda:'中国' in log.read_text(),30)
        report["checks"]["rime_candidates_commit"] = "passed"

        # New windows take keyboard focus. Password input must reach the client
        # literally, with no engine composition or surrounding-text retention.
        session.run(["omime", "switch", "echo"])
        password = session.start("password", [args.bin_dir/"omime-ti-client", "--purpose", "8", "--app-id", "omime-password-test"])
        password_log = args.out/"password.log"
        shared["wait_for"]("password focus",lambda:password_log.exists() and "ready" in password_log.read_text())
        keyboard.type("secret")
        shared["wait_for"]("password bypass",lambda:'text "secret"' in password_log.read_text())
        if any(line.startswith("commit ") or (line.startswith("preedit ") and not line.startswith('preedit ""')) for line in password_log.read_text().splitlines()):
            raise RuntimeError("password field received engine composition")
        report["checks"]["password_bypass"] = "passed"
        before = log.read_text().count("ready")
        before_grabs = daemon_log.read_text().count(grab_marker)
        password.terminate()
        password.wait(timeout=5)
        shared["wait_for"]("original editor regains focus",lambda:log.read_text().count("ready") > before)
        shared["wait_for"]("input-method reactivation",lambda:daemon_log.read_text().count(grab_marker) > before_grabs)
        keyboard.type("again ")
        shared["wait_for"]("input after password focus",lambda:'text "HELLO 中国AGAIN "' in log.read_text())
        report["checks"]["focus_reactivation"] = "passed"
        if compositor.poll() is not None or editor.poll() is not None:
            raise RuntimeError("compositor or editor exited")
        report["status"] = "passed"
    except Exception as exc:
        report.update(status="failed", detail=str(exc))
    finally:
        if keyboard:
            keyboard.close()
        session.close()
        (args.out/"report.json").write_text(json.dumps(report,indent=2,ensure_ascii=False)+"\n")
    print(json.dumps(report,ensure_ascii=False))
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
