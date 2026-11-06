#!/usr/bin/env python3
"""Probe a private desktop session and optional Firefox/Rime text-input workflow.

No session services, user settings, browser profiles, or installed packages are changed.
A report distinguishes a successful probe from a supported desktop/application.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import http.server
import json
import os
from pathlib import Path
import platform
import shutil
import shlex
import signal
import socket
import struct
import subprocess
import sys
import threading
import time
from xml.sax.saxutils import escape

ROOT = Path(__file__).resolve().parent.parent
REQUIRED = ("zwp_input_method_manager_v2", "zwp_virtual_keyboard_manager_v1")
SCENARIOS = ("preedit", "candidate_keyboard", "candidate_pointer", "commit", "focus_cleanup", "shortcuts",
             "popup_position", "mixed_scale", "restart")
APPLICATIONS = {
    # The demos are real toolkit clients; their presence is recorded separately from
    # input acceptance. They are intentionally not launched by the default probe.
    "gtk3": ("GTK 3 application", ("gtk3-demo",)),
    "gtk4": ("GTK 4 application", ("gtk4-demo",)),
    "qt5": ("Qt 5 application", ("qtdiag",)),
    "qt6": ("Qt 6 application", ("qtdiag6",)),
    "firefox": ("Firefox", ("firefox",)), "chromium": ("Chromium", ("chromium",)),
    "vscode": ("VS Code", ("code",)), "foot": ("foot", ("foot",)),
    "xterm": ("xterm", ("xterm",)), "wechat": ("WeChat", ("wechat",)),
    "wps": ("WPS Office", ("wps",)),
}


def result(status, detail, **extra):
    return {"status": status, "detail": detail, **extra}


def wait_for(description, callback, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = callback()
        if value:
            return value
        time.sleep(0.05)
    raise TimeoutError(f"timed out waiting for {description}")


def private_environment(directory):
    env = os.environ.copy()
    shared_rime_data = env.get("OMIME_RIME_SHARED_DATA")
    for key in list(env):
        if key.startswith(("DBUS_", "IBUS_", "OMIME_", "HYPRLAND_", "WLR_")) or key in (
            "DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "SWAYSOCK", "SESSION_MANAGER",
            "GTK_IM_MODULE", "QT_IM_MODULE", "QT_IM_MODULES", "XMODIFIERS", "LANGUAGE",
            "GDK_BACKEND", "QT_QPA_PLATFORM", "XAUTHORITY", "KDE_FULL_SESSION",
        ):
            env.pop(key, None)
    # This is the read-only dictionary path from the development shell, not an
    # engine socket or user configuration. Installed Rime wrappers set it themselves.
    if shared_rime_data:
        env["OMIME_RIME_SHARED_DATA"] = shared_rime_data
    for key, subdir in (
        ("XDG_RUNTIME_DIR", "run"), ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"), ("XDG_CONFIG_DIRS", "system-config"),
    ):
        path = directory / subdir
        path.mkdir(mode=0o700)
        env[key] = str(path)
    env.update(LC_ALL="C.UTF-8", XDG_SESSION_TYPE="wayland",
               DBUS_SESSION_BUS_ADDRESS=f"unix:path={directory / 'run/bus'}",
               IBUS_ADDRESS_FILE=str(directory / "run/ibus-address"),
               GSETTINGS_BACKEND="memory", GTK_A11Y="none", NO_AT_BRIDGE="1")
    # Keep only two representative fonts in this private session. A fresh font cache
    # must not rescan thousands of user fonts on every compositor/browser launch.
    font_dir = directory / "fonts"
    font_dir.mkdir()
    for index, pattern in enumerate(("sans", "sans:lang=zh")):
        if not shutil.which("fc-match"):
            break
        found = subprocess.run(["fc-match", "-f", "%{file}", pattern], capture_output=True,
                               text=True, timeout=10)
        source = Path(found.stdout)
        if found.returncode == 0 and source.is_file():
            (font_dir / f"font-{index}{source.suffix}").symlink_to(source)
    if any(font_dir.iterdir()):
        config = directory / "fonts.conf"
        config.write_text(f"<fontconfig><dir>{escape(str(font_dir))}</dir>"
                          f"<cachedir>{escape(str(directory / 'cache/fontconfig'))}</cachedir></fontconfig>")
        env["FONTCONFIG_FILE"] = str(config)
    return env


def receive_globals(connection):
    """Perform one registry roundtrip without binding input methods or any seats."""
    connection.sendall(struct.pack("=III", 1, (12 << 16) | 1, 2)
                       + struct.pack("=III", 1, 12 << 16, 3))
    pending = b""
    globals_ = {}
    while True:
        chunk = connection.recv(65536)
        if not chunk:
            raise RuntimeError("compositor closed the connection during registry roundtrip")
        pending += chunk
        while len(pending) >= 8:
            object_id, header = struct.unpack_from("=II", pending)
            size, opcode = header >> 16, header & 0xFFFF
            if size < 8 or size % 4:
                raise RuntimeError("invalid Wayland message size")
            if len(pending) < size:
                break
            body, pending = pending[8:size], pending[size:]
            if object_id == 1 and opcode == 0:
                raise RuntimeError("Wayland display returned a protocol error")
            if object_id == 2 and opcode == 0:
                _, length = struct.unpack_from("=II", body)
                name = body[8:8 + length - 1].decode("ascii")
                version = struct.unpack_from("=I", body, 8 + ((length + 3) // 4) * 4)[0]
                globals_[name] = max(version, globals_.get(name, 0))
            if object_id == 3 and opcode == 0:
                return dict(sorted(globals_.items()))


def registry(env, timeout):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(timeout)
        connection.connect(str(Path(env["XDG_RUNTIME_DIR"]) / env["WAYLAND_DISPLAY"]))
        return receive_globals(connection)


class Session:
    def __init__(self, output, env):
        self.output, self.env, self.children = output, env, []

    def start(self, name, command, **extra_env):
        log = (self.output / f"{name}.log").open("w")
        try:
            process = subprocess.Popen([str(arg) for arg in command], env=self.env | extra_env,
                                       stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                       start_new_session=True)
        finally:
            log.close()
        self.children.append(process)
        return process

    def run(self, command, timeout=15, input_text=None, **extra_env):
        return subprocess.run([str(arg) for arg in command], env=self.env | extra_env,
                              stdin=subprocess.DEVNULL if input_text is None else None, input=input_text,
                              capture_output=True, text=True, timeout=timeout)

    def close(self):
        # Every child has its own process group, including its descendants. Never
        # pkill by name: a desktop or browser may also be running in the real session.
        for process in reversed(self.children):
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        for process in reversed(self.children):
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()


def version(command):
    try:
        completed = subprocess.run(command, capture_output=True, text=True, timeout=10)
        return (completed.stdout or completed.stderr).strip().splitlines()[0]
    except (OSError, subprocess.SubprocessError, IndexError):
        return "unknown"


def application_matrix():
    rows = []
    for app_id, (name, candidates) in APPLICATIONS.items():
        path = next((shutil.which(candidate) for candidate in candidates if shutil.which(candidate)), None)
        rows.append({"id": app_id, "application": name, "availability": "available" if path else "unavailable",
                     "executable": path, "version": None,
                     "installation": str(Path(path).resolve()) if path else None,
                     "display": None, "route": None, "engine": None, "launch_options": [],
                     "scenarios": {scenario: result("not-tested", "not run") for scenario in SCENARIOS}})
    return rows


class BrowserPage:
    def __init__(self):
        self.events = []
        self.focus_target = None
        page = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                if self.path == "/focus":
                    content = json.dumps({"target": page.focus_target}).encode()
                    page.focus_target = None
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(content)))
                    self.end_headers()
                    self.wfile.write(content)
                    return
                if self.path != "/":
                    self.send_error(404)
                    return
                content = (ROOT / "tests/desktop/input.html").read_bytes()
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(content)))
                self.end_headers()
                self.wfile.write(content)

            def do_POST(self):
                length = int(self.headers.get("Content-Length", "0"))
                if self.path != "/event" or length > 16384:
                    self.send_error(400)
                    return
                page.events.append(json.loads(self.rfile.read(length)))
                self.send_response(204)
                self.end_headers()

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    def saw(self, kind, target=None, value=None):
        return any(event["type"] == kind and (target is None or event["target"] == target)
                   and (value is None or event["value"] == value) for event in self.events)


def firefox_probe(session, args, report):
    row = next(row for row in report["applications"] if row["id"] == "firefox")
    firefox = shutil.which(args.firefox)
    if not firefox:
        row["scenarios"]["preedit"] = result("blocked", f"{args.firefox} is unavailable")
        return
    row.update(executable=firefox, version=version([firefox, "--version"]),
               installation=str(Path(firefox).resolve()), display="Wayland", route="text-input-v3",
               engine="rime/luna_pinyin",
               launch_options=["--no-remote", "--profile <private directory>", "MOZ_ENABLE_WAYLAND=1",
                               "GTK_IM_MODULE unset"])
    page = BrowserPage()
    stage = "preedit"
    try:
        profile = session.output / "firefox-profile"
        profile.mkdir()
        (profile / "user.js").write_text('user_pref("browser.shell.checkDefaultBrowser", false);\n'
                                        'user_pref("browser.startup.homepage_override.mstone", "ignore");\n'
                                        'user_pref("datareporting.policy.dataSubmissionEnabled", false);\n'
                                        'user_pref("browser.aboutwelcome.enabled", false);\n')
        session.start("firefox", [firefox, "--no-remote", "--profile", profile,
                                  f"http://127.0.0.1:{page.server.server_port}/"],
                      MOZ_ENABLE_WAYLAND="1", GDK_BACKEND="wayland")
        wait_for("Firefox test field focus", lambda: page.saw("focus", "first"), args.timeout)
        def key(*values):
            output = session.run(["wtype", *values])
            if output.returncode:
                raise RuntimeError(output.stderr.strip())
        key("-d", "30", "zhongguo")
        wait_for("Firefox preedit", lambda: page.saw("compositionupdate", "first"), args.timeout)
        row["scenarios"][stage] = result("passed", "Firefox emitted compositionupdate for the first field")
        stage = "candidate_keyboard"
        key("-k", "space")
        wait_for("Rime's first candidate in Firefox", lambda: page.saw("compositionend", "first", "中国")
                 and page.saw("input", "first", "中国"), args.timeout)
        row["scenarios"][stage] = result("passed", "Space selected the first Rime pinyin candidate")
        row["scenarios"]["commit"] = result("passed", "DOM compositionend and input value equal 中国")
        stage = "focus_cleanup"
        key("-d", "30", "ni")
        wait_for("second composition", lambda: sum(e["type"] == "compositionstart" for e in page.events) >= 2,
                 args.timeout)
        page.focus_target = "second"
        wait_for("second field focus", lambda: page.saw("focus", "second", ""), args.timeout)
        key("-d", "30", "hao")
        key("-k", "space")
        wait_for("second field independent composition", lambda: page.saw("compositionend", "second", "好")
                 and page.saw("input", "second", "好"), args.timeout)
        row["scenarios"][stage] = result("passed", "DOM focus moved during composition; second field committed only 好")
    except (OSError, RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
        row["scenarios"][stage] = result("failed", str(error))
    finally:
        page.close()
        (session.output / "firefox-events.json").write_text(json.dumps(page.events, ensure_ascii=False, indent=2) + "\n")


def terminal_probe(session, args, report, app_id):
    row = next(row for row in report["applications"] if row["id"] == app_id)
    terminal = shutil.which(getattr(args, app_id))
    if not terminal:
        row["scenarios"]["commit"] = result("blocked", f"{getattr(args, app_id)} is unavailable")
        return
    is_xterm = app_id == "xterm"
    version_flag = "-version" if is_xterm else "--version"
    window_id = f"omime-{app_id}-probe"
    row.update(executable=terminal, version=version([terminal, version_flag]),
               availability="available", installation=str(Path(terminal).resolve()),
               display="XWayland" if is_xterm else "Wayland", route="XIM" if is_xterm else "text-input-v3",
               engine="rime/luna_pinyin", launch_options=[
                   "-name omime-xterm-probe, XMODIFIERS=@im=omime" if is_xterm
                   else "--config /dev/null --app-id omime-foot-probe", "synthetic terminal reader"])
    output = session.output / f"{app_id}-input.txt"
    try:
        options = (["-name", window_id, "-xrm", "*locale: true", "-e"] if is_xterm
                   else ["--config", "/dev/null", "--app-id", window_id])
        extra_env = {"XMODIFIERS": "@im=omime"} if is_xterm else {}
        session.start(app_id, [terminal, *options, sys.executable,
                              ROOT / "tests/desktop/terminal-reader.py", output], **extra_env)
        def has_window():
            tree = session.run(["swaymsg", "-t", "get_tree", "-r"])
            return tree.returncode == 0 and window_id in tree.stdout
        wait_for(f"{app_id} window", has_window, args.timeout)
        field = "instance" if is_xterm else "app_id"
        focused = session.run(["swaymsg", f'[{field}="{window_id}"] focus'])
        if focused.returncode:
            raise RuntimeError(focused.stderr.strip())
        wait_for(f"{app_id} reader", lambda: output.with_suffix(".ready").exists(), args.timeout)
        if is_xterm:
            # Keep the normal US keymap: wtype installs a synthetic keymap, whereas
            # XIM resolves keycodes through the X server's core keyboard mapping.
            keys = [*"zhongguo", "space", "return"]
            script = "".join(f"tap {key}\nsleep 30\n" for key in keys)
            typed = session.run([args.tools_dir / "omime-keyboard", "--script"], input_text=script)
        else:
            typed = session.run(["wtype", "-d", "30", "zhongguo ", "-k", "Return"])
        if typed.returncode:
            raise RuntimeError(typed.stderr.strip())
        wait_for(f"{app_id}'s committed line", output.exists, args.timeout)
        value = output.read_text()
        if value != "中国\n":
            raise RuntimeError(f"terminal received {value!r}, expected '中国\\n'")
        row["scenarios"]["candidate_keyboard"] = result("passed", "Space selected the first Rime pinyin candidate")
        row["scenarios"]["commit"] = result("passed", "terminal stdin received exactly 中国 followed by newline")
    except (OSError, RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
        row["scenarios"]["commit"] = result("failed", str(error))


def fingerprint(path):
    try:
        digest = hashlib.sha256()
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()
    except (OSError, AttributeError):
        return None


def worktree_dirty():
    try:
        status = subprocess.run(["git", "-C", str(ROOT), "status", "--porcelain", "--untracked-files=all"],
                                capture_output=True, text=True, timeout=10)
        return bool(status.stdout.strip())
    except (OSError, subprocess.SubprocessError):
        return None


def probe(args):
    args.out.mkdir(parents=True, exist_ok=False)
    args.out.chmod(0o700)
    report = {"schema_version": 1, "date": datetime.now(timezone.utc).isoformat(),
              "revision": version(["git", "-C", str(ROOT), "rev-parse", "HEAD"]),
              "worktree_dirty": worktree_dirty(),
              "probe_script_sha256": fingerprint(Path(__file__).resolve()),
              "system": platform.system(), "architecture": platform.machine(),
              "desktop": args.desktop, "session": "private headless", "checks": {},
              "applications": application_matrix(), "limitations": [
                  "Headless probes do not validate real GPUs, desktop launchers, fullscreen or mixed DPI.",
                  "A passed protocol/startup probe is not application acceptance.",
              ]}
    env = private_environment(args.out)
    # Build-time Rime data is needed only for source binaries; preserve this one
    # explicit input instead of inheriting daemon/session control environment.
    if os.environ.get("OMIME_RIME_SHARED_DATA"):
        env["OMIME_RIME_SHARED_DATA"] = os.environ["OMIME_RIME_SHARED_DATA"]
    env["PATH"] = str(args.bin_dir) + os.pathsep + env.get("PATH", "")
    env["XDG_CURRENT_DESKTOP"] = {"sway": "sway", "kwin": "KDE", "gnome": "GNOME"}[args.desktop]
    session = Session(args.out, env)
    stage = "compositor"
    try:
        executable = args.compositor or {"sway": "sway", "kwin": "kwin_wayland", "gnome": "gnome-shell"}[args.desktop]
        program = shutil.which(executable)
        if not program:
            report["checks"][stage] = result("blocked", f"{executable} is unavailable; no desktop was started")
            return report
        report["compositor"] = {"executable": program, "version": version([program, "--version"])}
        bus_config = args.out / "bus.conf"
        bus_config.write_text(f'<busconfig><type>session</type><listen>{escape(env["DBUS_SESSION_BUS_ADDRESS"])}</listen>'
                              '<auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/>'
                              '<allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        session.start("dbus", ["dbus-daemon", "--nofork", f"--config-file={bus_config}"])
        wait_for("private D-Bus socket", lambda: (args.out / "run/bus").exists(), args.timeout)
        if args.desktop == "sway":
            config = args.out / "sway.conf"
            xwayland = "enable" if args.test_xterm else "disable"
            config.write_text("output HEADLESS-1 resolution 1280x720\ndefault_border none\n"
                              f"xwayland {xwayland}\ninput type:keyboard repeat_delay 3000\n")
            session.start("compositor", [program, "-c", config], WLR_BACKENDS="headless", WLR_RENDERER="pixman",
                          WLR_LIBINPUT_NO_DEVICES="1", WLR_HEADLESS_OUTPUTS="1")
            socket_name = wait_for("sway socket", lambda: next((p.name for p in (args.out / "run").glob("wayland-*")
                                                                if p.is_socket()), None), args.timeout)
            env["WAYLAND_DISPLAY"] = socket_name
            env["SWAYSOCK"] = str(wait_for("sway IPC socket", lambda: next(
                (p for p in (args.out / "run").glob("sway-ipc.*.sock") if p.is_socket()), None), args.timeout))
            if args.test_xterm:
                display_file = args.out / "xwayland-display"
                def display_ready():
                    command = shlex.join([sys.executable, str(ROOT / "tests/desktop/record-display.py"), str(display_file)])
                    session.run(["swaymsg", "exec", command])
                    return display_file.read_text().strip() if display_file.exists() else None
                env["DISPLAY"] = wait_for("private XWayland display", display_ready, args.timeout)
        elif args.desktop == "kwin":
            env["WAYLAND_DISPLAY"] = "omime-probe-wayland"
            session.start("compositor", [program, "--virtual", "--no-lockscreen", "--no-global-shortcuts",
                                          "--no-kactivities", "--socket", env["WAYLAND_DISPLAY"],
                                          "--inputmethod", ROOT / "tests/desktop/wayland-registry.py"],
                          QT_QPA_PLATFORM="offscreen", KWIN_COMPOSE="Q",
                          OMIME_DESKTOP_REGISTRY_REPORT=str(args.out / "kwin-inputmethod-registry.json"))
        else:
            help_text = session.run([program, "--help"]).stdout
            if not all(option in help_text for option in ("--headless", "--wayland-display")):
                report["checks"][stage] = result("blocked", "gnome-shell lacks required headless/display options")
                return report
            env["WAYLAND_DISPLAY"] = "omime-probe-wayland"
            session.start("compositor", [program, "--wayland", "--headless", "--no-x11",
                                          "--wayland-display", env["WAYLAND_DISPLAY"]])
        wait_for("Wayland socket", lambda: (Path(env["XDG_RUNTIME_DIR"]) / env["WAYLAND_DISPLAY"]).exists(), args.timeout)
        stage = "registry"
        globals_ = registry(env, args.timeout)
        report["globals"] = globals_
        if args.desktop == "kwin":
            privileged = args.out / "kwin-inputmethod-registry.json"
            try:
                wait_for("KWin inputmethod registry", privileged.exists, args.timeout)
                report["privileged_inputmethod"] = json.loads(privileged.read_text())
            except (TimeoutError, ValueError) as error:
                report["privileged_inputmethod"] = result("blocked", str(error))
        report["checks"]["compositor"] = result("passed", "private compositor answered a registry roundtrip")
        missing = [name for name in REQUIRED if name not in globals_]
        report["checks"][stage] = result("blocked" if missing else "passed",
                                        "missing current daemon requirements: " + ", ".join(missing) if missing
                                        else "current daemon's Wayland globals are present")
        if args.desktop == "kwin":
            report["limitations"].append("KWin restricts IM v1 to its launched input method; public registry absence does not prove IM v1 is absent.")
        stage = "daemon"
        daemon = args.bin_dir / "omimed"
        if not daemon.is_file():
            report["checks"][stage] = result("blocked", f"{daemon} is unavailable")
            return report
        report["platform"] = {"executable": str(daemon), "version": version([str(daemon), "--version"]),
                              "sha256": fingerprint(daemon)}
        rime_binary = args.bin_dir / "omime-rime"
        if rime_binary.is_file():
            report["platform"]["rime_executable"] = str(rime_binary)
            report["platform"]["rime_sha256"] = fingerprint(rime_binary)
        config_dir = args.out / "config/oh-my-ime"
        config_dir.mkdir()
        (config_dir / "config.toml").write_text('engines = ["rime"]\n[panel]\nenabled = false\n[tray]\nenabled = false\n')
        manifest_dir = args.out / "engines/rime"
        manifest_dir.mkdir(parents=True)
        (manifest_dir / "engine.toml").write_text('[engine]\nid = "rime"\nname = "Rime"\napi = "1.0"\n'
                                                 f'exec = [{json.dumps(str(args.bin_dir / "omime-rime"))}]\n')
        env["OMIME_ENGINE_PATH"] = str(args.out / "engines")
        process = session.start("omimed", [daemon, "--engine", "rime"], OMIME_LOG="debug")
        if missing:
            try:
                code = process.wait(timeout=args.timeout)
                report["checks"][stage] = result("blocked", "current daemon cannot start on this desktop", exit_code=code)
            except subprocess.TimeoutExpired:
                report["checks"][stage] = result("blocked", "no supported frontend; daemon did not exit before timeout")
            return report
        def ready():
            if process.poll() is not None:
                raise RuntimeError(f"omimed exited with {process.returncode}; see omimed.log")
            response = session.run([args.bin_dir / "omime", "list"], timeout=5)
            return response.returncode == 0 and "rime/luna_pinyin" in response.stdout
        wait_for("Rime pinyin schema", ready, max(args.timeout, 120))
        response = session.run([args.bin_dir / "omime", "switch", "rime/luna_pinyin"])
        if response.returncode:
            raise RuntimeError(response.stderr.strip())
        report["checks"][stage] = result("passed", "Rime deployed and luna_pinyin was selected")
        if args.test_firefox or args.test_foot or args.test_xterm:
            keyboard = args.tools_dir / "omime-keyboard"
            if not keyboard.is_file() or not shutil.which("wtype"):
                report["checks"]["application_tools"] = result("blocked", "build omime-keyboard and install wtype")
            else:
                session.start("keyboard", [keyboard])
                if args.test_firefox:
                    firefox_probe(session, args, report)
                if args.test_foot:
                    terminal_probe(session, args, report, "foot")
                if args.test_xterm:
                    terminal_probe(session, args, report, "xterm")
        return report
    except (OSError, RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
        report["checks"][stage] = result("blocked" if stage in ("compositor", "registry") else "failed", str(error))
        return report
    finally:
        session.close()
        (args.out / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--desktop", choices=("sway", "kwin", "gnome"), required=True)
    parser.add_argument("--out", type=Path, required=True, help="new directory for the report and private session")
    parser.add_argument("--bin-dir", type=Path, default=ROOT / "target/debug")
    parser.add_argument("--tools-dir", type=Path, default=ROOT / "target/debug")
    parser.add_argument("--compositor", help="explicit compositor executable")
    parser.add_argument("--test-firefox", action="store_true", help="exercise real Firefox DOM composition and Rime")
    parser.add_argument("--firefox", default="firefox")
    parser.add_argument("--test-foot", action="store_true", help="exercise real foot terminal stdin with Rime")
    parser.add_argument("--foot", default="foot")
    parser.add_argument("--test-xterm", action="store_true", help="exercise real xterm/XIM terminal input")
    parser.add_argument("--xterm", default="xterm")
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--require-compatible", action="store_true", help="exit 3 on any blocked/failed executed check")
    args = parser.parse_args()
    args.out, args.bin_dir, args.tools_dir = args.out.resolve(), args.bin_dir.resolve(), args.tools_dir.resolve()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    if (args.test_firefox or args.test_foot or args.test_xterm) and args.desktop != "sway":
        parser.error("automated application input currently requires sway's virtual keyboard")
    if args.out.exists():
        parser.error("--out must not already exist; every probe uses a fresh private session")
    report = probe(args)
    print(f"Report: {args.out / 'report.json'}")
    checks = list(report["checks"].values())
    for row in report["applications"]:
        checks.extend(row["scenarios"].values())
    for row in report["applications"]:
        for scenario, check in row["scenarios"].items():
            if check["status"] != "not-tested":
                print(f"{row['id']}/{scenario}: {check['status']}: {check['detail']}")
    for name, check in report["checks"].items():
        print(f"{name}: {check['status']}: {check['detail']}")
    return 3 if args.require_compatible and any(check["status"] in ("blocked", "failed") for check in checks) else 0


if __name__ == "__main__":
    sys.exit(main())
