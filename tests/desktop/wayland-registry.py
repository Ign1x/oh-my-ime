#!/usr/bin/env python3
"""Read the registry on the privileged connection supplied by KWin --inputmethod."""
import json
import os
from pathlib import Path
import runpy
import signal
import socket

probe = runpy.run_path(str(Path(__file__).resolve().parents[2] / "scripts/desktop-probe.py"))
output = Path(os.environ["OMIME_DESKTOP_REGISTRY_REPORT"])
try:
    with socket.socket(fileno=int(os.environ["WAYLAND_SOCKET"])) as connection:
        connection.settimeout(15)
        globals_ = probe["receive_globals"](connection)
        output.write_text(json.dumps({"status": "passed", "globals": globals_}, indent=2) + "\n")
        # Keep the input method alive until the private compositor is stopped.
        signal.pause()
except (OSError, ValueError, KeyError, RuntimeError) as error:
    output.write_text(json.dumps({"status": "blocked", "detail": str(error)}, indent=2) + "\n")
