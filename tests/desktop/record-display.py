#!/usr/bin/env python3
"""Called by the private sway to read its private XWayland display name."""
import os
from pathlib import Path
import sys

Path(sys.argv[1]).write_text(os.environ.get("DISPLAY", ""))
