#!/usr/bin/env python3
"""build-book.py — entry point for the designed *Firefly for Rust by Example*
PDF + EPUB pipeline.

This is a thin launcher: it ensures Homebrew's gobject/pango/cairo are on the
dynamic-loader path (WeasyPrint needs them on macOS), then delegates to
build/build.py which reads book.yaml and renders the PDF + EPUB.

Usage:
    docs/book/build-book.py             # build both PDF and EPUB
    docs/book/build-book.py --pdf       # PDF only
    docs/book/build-book.py --epub      # EPUB only

Prefer running it through build-book.sh, which selects the project venv and
exports DYLD_FALLBACK_LIBRARY_PATH for you.
"""
from __future__ import annotations
import os
import runpy
import sys
from pathlib import Path

BOOK = Path(__file__).resolve().parent

# Make Homebrew's native libs discoverable for WeasyPrint when run directly.
if sys.platform == "darwin":
    brew_lib = "/opt/homebrew/lib"
    cur = os.environ.get("DYLD_FALLBACK_LIBRARY_PATH", "")
    if brew_lib not in cur.split(":"):
        os.environ["DYLD_FALLBACK_LIBRARY_PATH"] = (
            f"{brew_lib}:/usr/local/lib" + (f":{cur}" if cur else ""))

sys.argv = [str(BOOK / "build" / "build.py"), *sys.argv[1:]]
runpy.run_path(str(BOOK / "build" / "build.py"), run_name="__main__")
