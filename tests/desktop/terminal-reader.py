#!/usr/bin/env python3
"""Capture one synthetic input line through a real terminal emulator's stdin."""
from pathlib import Path
import signal
import sys

output = Path(sys.argv[1])
output.with_suffix(".ready").touch()
output.write_text(sys.stdin.readline())
signal.pause()
