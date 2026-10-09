"""Integration tests for localized covers in the real book renderer."""
from __future__ import annotations

from contextlib import ExitStack, redirect_stdout
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
import zipfile

from pypdf import PdfReader
from PIL import Image
import yaml

BOOK = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("book_build", BOOK / "build" / "build.py")
builder = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(builder)

OPF = {"opf": "http://www.idpf.org/2007/opf", "dc": "http://purl.org/dc/elements/1.1/"}
XHTML = {"x": "http://www.w3.org/1999/xhtml"}


class CoverIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "src").mkdir()
        (self.root / "art").mkdir()
        (self.root / "src" / "chapter.md").write_text("# Chapter\n\nBusiness content stays here.\n")
        for language in ("en", "es"):
            for side, color in (("front", "#ed312f"), ("back", "#125cc4")):
                (self.root / "art" / f"{side}-{language}.svg").write_text(
                    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1500 1850">'
                    f'<rect width="1500" height="1850" fill="{color}"/>'
                    f'<text x="100" y="200" font-size="60">{side.upper()}_{language.upper()}</text>'
                    '</svg>'
                )

    def manifest(self, language="en"):
        return {
            "title": "A test book", "author": "Test Publisher", "language": language,
            "identifier": "urn:uuid:9161672f-86cd-455d-a9a8-13b6c0fdfd89",
            "cover_svg": f"art/front-{language}.svg",
            "back_cover_svg": f"art/back-{language}.svg",
            "cover_alt": "Portada de prueba" if language == "es" else 'Test "front" cover & architecture',
            "back_cover_alt": "Contraportada de prueba" if language == "es" else "Test back cover",
            "labels": {"cover": "Portada", "back_cover": "Contraportada"} if language == "es" else {},
            "front": [],
            "parts": [{"title": "Part I — Test", "chapters": [
                {"id": "chapter", "file": "chapter.md", "num": 1, "title": "Chapter"}
            ]}],
        }

    def build(self, cfg, format="--epub", output=None):
        (self.root / "book.yaml").write_text(yaml.safe_dump(cfg))
        with ExitStack() as stack:
            stack.enter_context(patch.object(builder, "BOOK", self.root))
            stack.enter_context(patch.object(builder, "DIST", self.root / "dist"))
            stack.enter_context(patch.object(builder.sys, "argv", ["build.py", format]))
            stack.enter_context(patch.dict(os.environ, {"BOOK_CONFIG": "book.yaml"}, clear=False))
            if output is not None:
                stack.enter_context(patch.dict(os.environ, {"BOOK_OUTPUT_DIR": str(output)}))
            else:
                stack.enter_context(patch.dict(os.environ, {"BOOK_OUTPUT_DIR": str(self.root / "dist")}))
            stack.enter_context(redirect_stdout(io.StringIO()))
            self.assertEqual(builder.main(), 0)
        return output or self.root / "dist"

    def test_epub_localizes_both_cover_documents_and_ends_spine_with_back_cover(self):
        for language in ("en", "es"):
            with self.subTest(language=language):
                cfg = self.manifest(language)
                out = self.build(cfg)
                with zipfile.ZipFile(out / "firefly-rust-by-example.epub") as archive:
                    opf = ET.fromstring(archive.read("OEBPS/content.opf"))
                    spine = [item.attrib["idref"] for item in opf.findall("opf:spine/opf:itemref", OPF)]
                    self.assertEqual(spine[0], "cover")
                    self.assertEqual(spine[-1], "back-cover")
                    self.assertEqual(opf.find("opf:metadata/dc:language", OPF).text, language)
                    covers = [item for item in opf.findall("opf:manifest/opf:item", OPF)
                              if item.attrib.get("properties") == "cover-image"]
                    self.assertEqual(len(covers), 1)
                    self.assertEqual(archive.read("OEBPS/" + covers[0].attrib["href"]),
                                     (self.root / cfg["cover_svg"]).read_bytes())
                    for doc, alt in (("cover", cfg["cover_alt"]), ("back-cover", cfg["back_cover_alt"])):
                        page = ET.fromstring(archive.read(f"OEBPS/{doc}.xhtml"))
                        self.assertEqual(page.attrib["lang"], language)
                        img = page.find(".//x:img", XHTML)
                        self.assertEqual(img.attrib["alt"], alt)
                        self.assertIn("OEBPS/" + img.attrib["src"], archive.namelist())
                        section = page.find(".//x:section", XHTML)
                        self.assertEqual(section.attrib["{http://www.idpf.org/2007/ops}type"],
                                         "cover" if doc == "cover" else "backmatter")
                    nav = archive.read("OEBPS/nav.xhtml").decode()
                    self.assertNotIn('href="back-cover.xhtml"', nav)

    def test_epub_uses_configured_png_cover_images(self):
        cfg = self.manifest()
        for prefix, color in (("cover", "red"), ("back_cover", "blue")):
            path = self.root / "art" / f"{prefix}.png"
            Image.new("RGB", (30, 37), color).save(path)
            cfg[f"{prefix}_image"] = str(path)
        out = self.build(cfg)
        with zipfile.ZipFile(out / "firefly-rust-by-example.epub") as archive:
            opf = ET.fromstring(archive.read("OEBPS/content.opf"))
            for cid, prefix in (("cover", "cover"), ("back-cover", "back_cover")):
                item = opf.find(f"opf:manifest/opf:item[@id='{cid}-img']", OPF)
                self.assertEqual(item.attrib["media-type"], "image/png")
                self.assertEqual(archive.read("OEBPS/" + item.attrib["href"]),
                                 Path(cfg[f"{prefix}_image"]).read_bytes())

    def test_pdf_has_full_trim_front_and_back_pages_without_running_furniture(self):
        out = self.build(self.manifest("es"), format="--pdf")
        pdf = PdfReader(out / "firefly-rust-by-example.pdf")
        self.assertEqual(pdf.pages[0].extract_text().strip(), "FRONT_ES")
        self.assertEqual(pdf.pages[-1].extract_text().strip(), "BACK_ES")
        for page in (pdf.pages[0], pdf.pages[-1]):
            self.assertEqual(float(page.mediabox.width), 540)
            self.assertEqual(float(page.mediabox.height), 666)
        self.assertIn("Business content stays here.", "\n".join(p.extract_text() for p in pdf.pages))

    def test_output_directory_preserves_existing_distribution(self):
        legacy = self.root / "dist"
        legacy.mkdir()
        artifact = legacy / "firefly-rust-by-example.epub"
        artifact.write_bytes(b"historical release artifact")
        out = self.build(self.manifest(), output=self.root / "new-edition")
        self.assertTrue((out / artifact.name).is_file())
        self.assertEqual(artifact.read_bytes(), b"historical release artifact")

    def test_title_page_keeps_collection_name_lowercase_without_running_title(self):
        cfg = self.manifest()
        cfg["title"] = "rsfly by example"
        cfg["front"] = [
            {"id": "title", "file": "title.md", "nav": False},
            {"id": "copyright", "file": "copyright.md", "nav": False},
        ]
        (self.root / "src" / "title.md").write_text("# rsfly by example {.chtitle}\n")
        (self.root / "src" / "copyright.md").write_text("Copyright 2026.\n")
        out = self.build(cfg, format="--pdf")
        pdf = PdfReader(out / "firefly-rust-by-example.pdf")
        pages = [page.extract_text() for page in pdf.pages]
        self.assertIn("rsfly by example", "\n".join(pages))
        self.assertNotIn("RSFLY BY EXAMPLE", "\n".join(pages))

    def test_interior_generators_use_the_official_family_lockup(self):
        import gen_openers
        import gen_diagrams
        self.assertIn("rsfly", gen_openers.emblem())
        self.assertNotIn("<ellipse", gen_openers.emblem())
        for name, generate in gen_diagrams.DIAGRAMS.items():
            with self.subTest(diagram=name):
                svg = ET.fromstring(gen_diagrams._bare(generate()))
                self.assertTrue(any("rsfly" in (element.text or "")
                                    for element in svg.iter()))

    def test_pdf_preserves_the_canonical_cover_gradient(self):
        cfg = self.manifest()
        (self.root / cfg["cover_svg"]).write_bytes((BOOK / "art/cover.svg").read_bytes())
        out = self.build(cfg, format="--pdf")
        preview = self.root / "cover-preview"
        subprocess.run(["pdftoppm", "-f", "1", "-l", "1", "-scale-to-x", "1500",
                        "-scale-to-y", "1850", "-png", "-singlefile",
                        str(out / "firefly-rust-by-example.pdf"), str(preview)],
                       check=True, capture_output=True)
        with Image.open(preview.with_suffix(".png")) as actual, Image.open(BOOK / "art/cover.png") as expected:
            # Interior point on the translucent upper-left stroke of the approved y.
            observed = actual.convert("RGB").getpixel((349, 434))
            reference = expected.convert("RGB").getpixel((349, 434))
            self.assertLess(max(abs(a - b) for a, b in zip(observed, reference)), 12)

    def test_pdf_preserves_the_interior_lockup_gradient(self):
        from brand import lockup
        from pdf import render_pdf
        out = self.root / "lockup.pdf"
        render_pdf('<html><style>@page{size:335.52px 242px;margin:0}'
                   'body{margin:0;line-height:0;background:#10110f}</style>'
                   + lockup(0, 0, 335.52, 242, theme="dark") + '</html>',
                   BOOK, [], out)
        preview = self.root / "lockup-preview"
        subprocess.run(["pdftoppm", "-scale-to-x", "1000", "-scale-to-y", "721",
                        "-png", "-singlefile", str(out), str(preview)],
                       check=True, capture_output=True)
        with Image.open(preview.with_suffix(".png")) as actual, Image.open(BOOK / "art/rsfly-dark.png") as logo:
            expected = Image.new("RGBA", logo.size, "#10110f")
            expected.alpha_composite(logo.convert("RGBA"))
            observed = actual.convert("RGB").getpixel((726, 188))
            reference = expected.convert("RGB").getpixel((726, 188))
            self.assertLess(max(abs(a - b) for a, b in zip(observed, reference)), 12)

    def test_missing_configured_back_cover_stops_build(self):
        cfg = self.manifest()
        cfg["back_cover_svg"] = "art/missing.svg"
        with self.assertRaises(FileNotFoundError):
            self.build(cfg)


if __name__ == "__main__":
    unittest.main()
