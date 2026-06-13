#!/usr/bin/env bash
#
# build-book.sh — render the designed *Firefly for Rust by Example* PDF + EPUB.
#
# The pipeline is a WeasyPrint build (build/build.py) driven by book.yaml:
#   1. Read book.yaml -> front matter, parts, chapters (each with an opener SVG).
#   2. Render every src/*.md chapter to HTML with the book theme (Pygments
#      highlighting, callout boxes, listing tabs, chapter heads + openers).
#   3. Assemble cover + front matter + Contents + part dividers + chapters and
#      render to dist/firefly-rust-by-example.{pdf,epub}.
#
# Usage:
#   docs/book/build-book.sh            # build both PDF and EPUB
#   docs/book/build-book.sh --pdf      # PDF only
#   docs/book/build-book.sh --epub     # EPUB only
#
# Requires the project venv at docs/book/.venv (weasyprint, markdown, pygments,
# pyyaml) and Homebrew's pango/cairo/gobject under /opt/homebrew/lib.
set -euo pipefail

BOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENV_PY="${BOOK_DIR}/.venv/bin/python"

# WeasyPrint loads libgobject/pango/cairo at import time; point the macOS
# dynamic loader at Homebrew's lib dir (the task environment installs them there).
BREW_PREFIX="$(brew --prefix 2>/dev/null || echo /opt/homebrew)"
export DYLD_FALLBACK_LIBRARY_PATH="${BREW_PREFIX}/lib:/usr/local/lib:${DYLD_FALLBACK_LIBRARY_PATH:-}"

if [ ! -x "${VENV_PY}" ]; then
  echo "error: book venv not found at ${VENV_PY}" >&2
  echo "       create it with: python3 -m venv ${BOOK_DIR}/.venv && \\" >&2
  echo "       ${BOOK_DIR}/.venv/bin/pip install weasyprint markdown pygments pyyaml" >&2
  exit 1
fi

case "${1:-}" in
  --pdf|--epub|"") ;;
  *) echo "usage: build-book.sh [--pdf|--epub]" >&2; exit 2 ;;
esac

# (Re)generate the cover + chapter-opener art so it always matches the manifest.
"${VENV_PY}" "${BOOK_DIR}/build/gen_openers.py"

exec "${VENV_PY}" "${BOOK_DIR}/build/build.py" "$@"
