# Building the branded book editions

Both editions use the collection title **rsfly by example**. The English subtitle
is “Composing typed services with Tokio, Axum and SQLx”; the Spanish subtitle is
“Servicios tipados con Tokio, Axum y SQLx”. The product name is rsfly; existing
crate names, imports, technical examples and repository URLs remain unchanged.

The English and Spanish editions share the original chapters and technical examples.
Each edition has a localized front and back cover from the Firefly Framework Brand
Kit, `11-Books/rust/`. The cover source of truth is that kit; this repository stores
the exported artwork used to build the books. `art/brand-assets.json` records the
imported files and their SHA-256 digests.

The PDF uses outlined SVG artwork at the existing 7.5 × 9.25 inch trim size.
CairoSVG renders the two cover pages as vectors, preserving translucent gradients;
they replace the reserved cover pages without changing interior pagination. Cover
pages have no running header or page number. The EPUB uses the corresponding PNG
artwork for reader compatibility, with a front-cover image declaration, localized
alternative text and separate front/back documents at the beginning/end of its
reading order. The back cover is not a table-of-contents chapter.

## Toolchain

Create an isolated Python 3.12 environment under `docs/book/.venv` and install
`docs/book/requirements.txt`. The requirements pin the renderer and its Python
dependencies, including the PDF inspection library used by the tests. WeasyPrint
also needs Pango/Cairo/GObject: on macOS use the Homebrew libraries; on Linux the
CI workflow lists the system packages. Poppler supplies `pdftoppm` for
rendered gradient regression tests. `build-book.sh` configures the macOS loader
path. Pandoc, LaTeX and mdBook are not part of the PDF/EPUB build.

```sh
python3.12 -m venv docs/book/.venv
docs/book/.venv/bin/pip install -r docs/book/requirements.txt
docs/book/build-book.sh --test
BOOK_OUTPUT_DIR=/absolute/path/to/book-editions docs/book/build-book.sh
BOOK_CONFIG=book-es.yaml BOOK_OUTPUT_DIR=/absolute/path/to/book-editions docs/book/build-book.sh
```

The output directory contains `firefly-rust-by-example.{pdf,epub}` and
`firefly-rust-by-example-es.{pdf,epub}`. Use `BOOK_OUTPUT_DIR` for a new edition:
the default `docs/book/dist/` contains tracked historical release artifacts and
must not be replaced incidentally. `--pdf` and `--epub` select one format.

The existing body typography uses system-font fallbacks; the outlined cover SVGs
do not depend on locally installed cover fonts. Inspect both complete PDFs after
building on the selected release host, including front/back pages, chapter
openings, page boundaries and the English/Spanish accented characters. Also open
each EPUB in a reader and check its cover, final page and navigation.

## Updating cover artwork

Copy the approved `cover`, `cover-es`, `back-cover` and `back-cover-es` SVG/PNG
pairs from the canonical kit into `docs/book/art/`, then refresh the provenance
digests. `book.yaml` and `book-es.yaml` select each edition's artwork, title labels
and alternative text. The build fails if a configured cover asset is missing.
`gen_openers.py` regenerates chapter openers only and does not own the cover files.

## Book-only publication

Book publication is separate from the framework version. The existing release
workflow is triggered by `v*.*.*` tags and publishes the historical committed
distribution; it is not the publisher for this design edition. A reviewed
`books-2026.10.08` release can attach the four newly built artifacts with
`latest=false`, preserving framework tags, package versions and existing assets.
Include the exact source commit, artwork provenance and artifact SHA-256 digests
in its release notes. No book build command publishes or uploads files.

## Interior identity and diagram sources

The approved rsfly gradient-y lockups in `art/rsfly-light.svg` and
`art/rsfly-dark.svg` come from Brand Kit `12-Frameworks/rust/`.
`build/brand.py` embeds their existing outlines; it never redraws the mark.
Chapter openers and part dividers use this same identity. Diagram footers reserve
space below the existing drawing, leaving technical coordinates and labels intact.
The inline figures in the English and Spanish manuscripts are maintained in the
manuscripts; `build/gen_diagrams.py` owns the standalone diagram previews.

`theme/tokens.css` uses the kit's charcoal, paper, amber, gold, ink and neutral
colors. Success, warning and failure indicators retain distinct accessible
semantic treatments. Syntax highlighting keeps its language-token distinctions.
Root README and book landing headers use the matching canonical family assets
from `assets/brand/`; technical identifiers and example code remain unchanged.

For PDF interiors, the renderer uses the canonical PNG version of each small
rsfly lockup to preserve the approved alpha gradient. Source SVG diagrams and
EPUB interiors retain the original vector lockups. This avoids WeasyPrint
flattening that gradient without changing the technical drawings.
