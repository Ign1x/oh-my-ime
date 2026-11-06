"""Protocol parsing and failure reporting for the private desktop probes."""

import argparse
import importlib.util
import json
import hashlib
import os
from pathlib import Path
import socket
import struct
import tempfile
import threading
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("desktop_probe", ROOT / "scripts/desktop-probe.py")
PROBE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROBE)


def message(object_id, opcode, body):
    return struct.pack("=II", object_id, ((len(body) + 8) << 16) | opcode) + body


def global_event(name, version):
    encoded = name.encode() + b"\0"
    body = struct.pack("=II", 7, len(encoded)) + encoded
    body += b"\0" * (-len(encoded) % 4)
    return message(2, 0, body + struct.pack("=I", version))


class DesktopProbeTests(unittest.TestCase):
    def test_registry_accepts_fragmented_messages_and_keeps_highest_version(self):
        client, server = socket.socketpair()
        client.settimeout(2)
        payload = (global_event("zwp_input_method_v1", 1)
                   + global_event("wl_compositor", 4)
                   + global_event("wl_compositor", 6)
                   + message(3, 0, struct.pack("=I", 10)))

        def send():
            with server:
                received = b""
                while len(received) < 24:
                    received += server.recv(24 - len(received))
                for index in range(0, len(payload), 5):
                    server.sendall(payload[index:index + 5])

        thread = threading.Thread(target=send)
        thread.start()
        try:
            with client:
                self.assertEqual(PROBE.receive_globals(client),
                                 {"wl_compositor": 6, "zwp_input_method_v1": 1})
        finally:
            thread.join()

    def test_disconnected_compositor_is_not_an_empty_successful_registry(self):
        client, server = socket.socketpair()
        server.shutdown(socket.SHUT_WR)
        try:
            with client, self.assertRaisesRegex(RuntimeError, "closed the connection"):
                PROBE.receive_globals(client)
        finally:
            server.close()

    def test_session_environment_does_not_use_the_running_desktop(self):
        foreign = {"DBUS_SESSION_BUS_ADDRESS": "unix:path=/run/user/1000/bus",
                   "WAYLAND_SOCKET": "3", "WAYLAND_DISPLAY": "wayland-1", "DISPLAY": ":0",
                   "SWAYSOCK": "/real/sway.sock", "HYPRLAND_INSTANCE_SIGNATURE": "real",
                   "IBUS_ADDRESS": "real", "GTK_IM_MODULE": "ibus", "QT_IM_MODULES": "wayland;fcitx",
                   "OMIME_ENGINE_PATH": "/real/engines"}
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, foreign), \
             patch.object(PROBE.shutil, "which", return_value=None):
            env = PROBE.private_environment(Path(directory))
            for key in foreign:
                if key == "DBUS_SESSION_BUS_ADDRESS":
                    self.assertEqual(env[key], f"unix:path={directory}/run/bus")
                else:
                    self.assertNotIn(key, env)
            self.assertEqual(env["XDG_CONFIG_HOME"], f"{directory}/config")
            self.assertEqual(env["GSETTINGS_BACKEND"], "memory")
            self.assertEqual(Path(env["XDG_RUNTIME_DIR"]).stat().st_mode & 0o777, 0o700)

    def test_development_dictionary_path_is_preserved_without_engine_connections(self):
        with tempfile.TemporaryDirectory() as directory, \
             patch.dict(os.environ, {"OMIME_RIME_SHARED_DATA": "/read-only/rime-data"}), \
             patch.object(PROBE.shutil, "which", return_value=None):
            env = PROBE.private_environment(Path(directory))
            self.assertEqual(env["OMIME_RIME_SHARED_DATA"], "/read-only/rime-data")

    def test_missing_desktop_is_blocked_and_no_application_is_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "probe"
            args = argparse.Namespace(out=output, desktop="gnome", compositor=None,
                                      bin_dir=Path(directory), tools_dir=Path(directory),
                                      timeout=1, test_firefox=False)
            with patch.object(PROBE.shutil, "which", return_value=None):
                report = PROBE.probe(args)
            self.assertEqual(report["checks"]["compositor"]["status"], "blocked")
            self.assertTrue(all(check["status"] == "not-tested" for app in report["applications"]
                                for check in app["scenarios"].values()))
            self.assertEqual(json.loads((output / "report.json").read_text()), report)

    def test_binary_fingerprint_distinguishes_builds_with_the_same_version(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "omimed"
            binary.write_bytes(b"build one")
            first = PROBE.fingerprint(binary)
            self.assertEqual(first, hashlib.sha256(b"build one").hexdigest())
            binary.write_bytes(b"build two")
            self.assertNotEqual(first, PROBE.fingerprint(binary))
            self.assertIsNone(PROBE.fingerprint(Path(directory) / "absent"))

    def test_report_is_written_and_children_cleaned_when_compositor_cannot_start(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "probe"
            args = argparse.Namespace(out=output, desktop="gnome", compositor="gnome-shell",
                                      bin_dir=Path(directory), tools_dir=Path(directory),
                                      timeout=1, test_firefox=False, test_foot=False)
            with patch.object(PROBE, "private_environment", return_value={"DBUS_SESSION_BUS_ADDRESS": "unix:path=/private/bus"}), \
                 patch.object(PROBE.shutil, "which", return_value="/missing/gnome-shell"), \
                 patch.object(PROBE.Session, "start", side_effect=OSError("no dbus-daemon")), \
                 patch.object(PROBE.Session, "close") as close:
                report = PROBE.probe(args)
            close.assert_called_once()
            self.assertEqual(report["checks"]["compositor"]["status"], "blocked")
            self.assertIn("no dbus-daemon", report["checks"]["compositor"]["detail"])
            self.assertEqual(json.loads((output / "report.json").read_text()), report)
            self.assertIsInstance(report["worktree_dirty"], bool)
            self.assertEqual(len(report["probe_script_sha256"]), 64)

    def test_existing_output_is_never_reused(self):
        with tempfile.TemporaryDirectory() as directory:
            args = argparse.Namespace(out=Path(directory))
            marker = Path(directory) / "keep"
            marker.write_text("existing result")
            with self.assertRaises(FileExistsError):
                PROBE.probe(args)
            self.assertEqual(marker.read_text(), "existing result")


if __name__ == "__main__":
    unittest.main()
