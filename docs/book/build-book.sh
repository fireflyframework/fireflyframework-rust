#!/usr/bin/env bash
#
# build-book.sh — render the Firefly-for-Rust mdBook chapters into a polished
# PDF and EPUB using pandoc + tectonic.
#
# Pipeline:
#   1. Read SUMMARY.md to recover chapter order.
#   2. Preprocess each chapter (strip mdBook-only syntax pandoc cannot parse,
#      normalize code-fence info strings, rewrite intra-book ./*.md links into
#      in-document anchors).
#   3. Concatenate everything behind a YAML metadata block (title, subtitle,
#      author, rights, date) so pandoc emits a real title page + TOC.
#   4. Run pandoc twice: once with --pdf-engine=tectonic for the PDF, once for
#      the EPUB.
#
# Usage:
#   docs/book/build-book.sh            # build both PDF and EPUB
#   docs/book/build-book.sh --pdf      # PDF only
#   docs/book/build-book.sh --epub     # EPUB only
#
# Requires: pandoc >= 3, tectonic (a TeX engine), both on PATH. Homebrew's
# /opt/homebrew/bin is prepended automatically.
set -euo pipefail

# --- locate ourselves -------------------------------------------------------
BOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_DIR="${BOOK_DIR}/src"
DIST_DIR="${BOOK_DIR}/dist"
SUMMARY="${SRC_DIR}/SUMMARY.md"

# Homebrew installs pandoc/tectonic here; make them discoverable.
export PATH="/opt/homebrew/bin:${PATH}"

# --- metadata ---------------------------------------------------------------
TITLE="Firefly for Rust by Example"
SUBTITLE="Reactive, Event-Driven, Resilient Microservices on Rust with the Firefly Framework"
AUTHOR="Firefly Software Foundation"
RIGHTS="Copyright (c) 2026 Firefly Software Foundation. Licensed under Apache-2.0."
DATE="$(date +%Y-%m-%d)"
PDF_OUT="${DIST_DIR}/firefly-rust-by-example.pdf"
EPUB_OUT="${DIST_DIR}/firefly-rust-by-example.epub"

# --- argument parsing -------------------------------------------------------
BUILD_PDF=1
BUILD_EPUB=1
case "${1:-}" in
  --pdf)  BUILD_EPUB=0 ;;
  --epub) BUILD_PDF=0 ;;
  "")     ;;
  *) echo "usage: build-book.sh [--pdf|--epub]" >&2; exit 2 ;;
esac

command -v pandoc >/dev/null   || { echo "error: pandoc not found on PATH" >&2; exit 1; }
if [ "${BUILD_PDF}" -eq 1 ]; then
  command -v tectonic >/dev/null || { echo "error: tectonic not found on PATH" >&2; exit 1; }
fi

mkdir -p "${DIST_DIR}"

# ---------------------------------------------------------------------------
# slugify <text> — reproduce pandoc's GitHub-style auto identifier so that
# rewritten intra-book links land on the correct heading anchor.
#   * lowercase
#   * drop everything that is not alphanumeric, space, hyphen or underscore
#   * collapse whitespace runs to a single hyphen
# ---------------------------------------------------------------------------
slugify() {
  printf '%s' "$1" \
    | tr '[:upper:]' '[:lower:]' \
    | LC_ALL=C sed -E 's/[^a-z0-9 _-]+//g' \
    | sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//' \
    | sed -E 's/[[:space:]]+/-/g'
}

# Recover the first-level (#) heading of a chapter file.
first_h1() {
  grep -m1 '^# ' "$1" | sed -E 's/^#[[:space:]]+//'
}

# ---------------------------------------------------------------------------
# Recover chapter order from SUMMARY.md. We accept both the bracketed prefix
# entry ([title](./file.md)) and the list entries (- [title](./file.md)).
# ---------------------------------------------------------------------------
CHAPTERS=()
while IFS= read -r line; do
  [ -n "${line}" ] && CHAPTERS+=("${line}")
done < <(
  grep -oE '\]\(\./[^)]+\.md\)' "${SUMMARY}" \
    | sed -E 's/^\]\(\.\///; s/\)$//'
)
if [ "${#CHAPTERS[@]}" -eq 0 ]; then
  echo "error: no chapters discovered in ${SUMMARY}" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Build a sed program that rewrites every intra-book "./file.md" link target
# into "#<anchor-of-that-file's-H1>". Targets carrying their own fragment are
# left for pandoc to resolve; here all SUMMARY links point at whole files.
# ---------------------------------------------------------------------------
LINK_SED="$(mktemp)"
trap 'rm -f "${LINK_SED}"' EXIT
for f in "${CHAPTERS[@]}"; do
  path="${SRC_DIR}/${f}"
  [ -f "${path}" ] || { echo "error: missing chapter ${path}" >&2; exit 1; }
  h1="$(first_h1 "${path}")"
  anchor="$(slugify "${h1}")"
  # Escape sed-special characters in the filename (hyphens/dots are literal in
  # a bracketed/escaped context, but be safe with '.').
  esc_f="$(printf '%s' "${f}" | sed -E 's/[.[\*^$/]/\\&/g')"
  # (./file.md) and (./file.md#frag) -> (#anchor) / (#anchor-frag-preserved)
  printf 's@\](\\./%s)@](#%s)@g\n' "${esc_f}" "${anchor}" >> "${LINK_SED}"
  printf 's@\](\\./%s#@](#@g\n'     "${esc_f}"             >> "${LINK_SED}"
done

# ---------------------------------------------------------------------------
# Assemble the manuscript: YAML metadata block, then each preprocessed chapter
# separated by blank lines. Preprocessing per chapter:
#   * normalize mdBook fence attributes: ```rust,ignore / ```rust,no_run /
#     ```rust,should_panic ... -> ```rust  (pandoc treats the whole info
#     string as one class, which kills highlighting; keep only the language).
#   * drop mdBook {{#include ...}}, {{#playground ...}}, {{#rustdoc_include ...}}
#     directives (none present today, but strip defensively so future edits
#     don't break the build).
#   * rewrite intra-book ./*.md links into in-document anchors.
# ---------------------------------------------------------------------------
COMBINED="$(mktemp)"
trap 'rm -f "${LINK_SED}" "${COMBINED}"' EXIT

{
  printf -- '---\n'
  printf 'title: "%s"\n'     "${TITLE}"
  printf 'subtitle: "%s"\n'  "${SUBTITLE}"
  printf 'author: "%s"\n'    "${AUTHOR}"
  printf 'date: "%s"\n'      "${DATE}"
  printf 'rights: "%s"\n'    "${RIGHTS}"
  printf 'lang: "en"\n'
  printf 'subject: "Rust, microservices, reactive, event-driven"\n'
  printf 'description: "%s"\n' "${SUBTITLE}"
  printf 'titlepage: true\n'
  printf 'toc-title: "Contents"\n'
  printf -- '---\n\n'
} > "${COMBINED}"

normalize_chapter() {
  # $1 = path to chapter
  sed -E \
    -e 's/^(```)[[:space:]]*rust,[A-Za-z0-9_,]+/\1rust/' \
    -e '/\{\{#(include|playground|rustdoc_include)[^}]*\}\}/d' \
    "$1" \
  | sed -f "${LINK_SED}"
}

for f in "${CHAPTERS[@]}"; do
  normalize_chapter "${SRC_DIR}/${f}" >> "${COMBINED}"
  printf '\n\n' >> "${COMBINED}"
done

# ---------------------------------------------------------------------------
# Common pandoc options shared by both targets.
# ---------------------------------------------------------------------------
COMMON_OPTS=(
  --from=gfm+yaml_metadata_block+smart
  --standalone
  --toc
  --toc-depth=2
  --number-sections
  --syntax-highlighting=tango
  --lua-filter="${BOOK_DIR}/tablewidths.lua"
)

# ---------------------------------------------------------------------------
# PDF — via tectonic. --top-level-division=chapter turns each "#" heading into
# a numbered chapter; the YAML metadata becomes the title page.
# ---------------------------------------------------------------------------
# Diagnostic hook: when FIREFLY_BOOK_DEBUG_TEX names a path, also emit the
# intermediate LaTeX so typesetting issues can be traced to a source line.
if [ -n "${FIREFLY_BOOK_DEBUG_TEX:-}" ]; then
  pandoc "${COMBINED}" "${COMMON_OPTS[@]}" --top-level-division=chapter \
    -H "${BOOK_DIR}/header.tex" -V documentclass=report -V papersize=letter \
    -V geometry:margin=1in -t latex -o "${FIREFLY_BOOK_DEBUG_TEX}"
fi

if [ "${BUILD_PDF}" -eq 1 ]; then
  echo "==> building PDF -> ${PDF_OUT}"
  pandoc "${COMBINED}" \
    "${COMMON_OPTS[@]}" \
    --top-level-division=chapter \
    --pdf-engine=tectonic \
    -H "${BOOK_DIR}/header.tex" \
    -V documentclass=report \
    -V papersize=letter \
    -V geometry:margin=1in \
    -V linkcolor=RoyalBlue \
    -V urlcolor=RoyalBlue \
    -V toccolor=black \
    -V colorlinks=true \
    -V fontsize=10pt \
    -M date="${DATE}" \
    -o "${PDF_OUT}"
fi

# ---------------------------------------------------------------------------
# EPUB.
# ---------------------------------------------------------------------------
if [ "${BUILD_EPUB}" -eq 1 ]; then
  echo "==> building EPUB -> ${EPUB_OUT}"
  pandoc "${COMBINED}" \
    "${COMMON_OPTS[@]}" \
    --top-level-division=chapter \
    --epub-title-page=true \
    --split-level=1 \
    -o "${EPUB_OUT}"
fi

# ---------------------------------------------------------------------------
# Report. (Written so the script always exits 0 on success — note that a bare
# `[ x -eq y ] && cmd` as the final statement would propagate a false test as a
# non-zero script exit under `set -e`.)
# ---------------------------------------------------------------------------
echo
echo "==> done"
if [ "${BUILD_PDF}" -eq 1 ]; then
  ls -l "${PDF_OUT}"
fi
if [ "${BUILD_EPUB}" -eq 1 ]; then
  ls -l "${EPUB_OUT}"
fi
exit 0
