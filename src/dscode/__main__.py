"""`python -m dscode` entry point.

On ASCII-only terminals, forcefully reconfigures stdout/stderr to UTF-8
before Textual initializes, preventing UnicodeEncodeError.
"""
from __future__ import annotations

import os
import sys

# ------------------------------------------------------------
# Force UTF-8: must happen BEFORE any Textual/Rich import
# ------------------------------------------------------------
if sys.stdout.encoding and "utf" not in sys.stdout.encoding.lower():
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
if sys.stderr.encoding and "utf" not in sys.stderr.encoding.lower():
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")

os.environ.setdefault("PYTHONIOENCODING", "utf-8")
os.environ.setdefault("PYTHONUTF8", "1")

if __name__ == "__main__":  # pragma: no cover
    from dscode.cli import app
    app()
