from pathlib import Path
import json
import shutil
import struct
import tempfile
import unittest

from contract import MediaError
from documents import Documents
from engine import Limits
from queue_store import Store


def sample_pdf(path: Path):
    text = b"BT /F1 18 Tf 24 96 Td (TokenCraft source page) Tj ET"
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 240 120] "
        b"/Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        f"<< /Length {len(text)} >>\nstream\n".encode() + text + b"\nendstream",
    ]
    result = bytearray(b"%PDF-1.4\n")
    offsets = []
    for index, content in enumerate(objects, 1):
        offsets.append(len(result))
        result += f"{index} 0 obj\n".encode() + content + b"\nendobj\n"
    table = len(result)
    result += f"xref\n0 {len(objects) + 1}\n0000000000 65535 f \n".encode()
    for offset in offsets:
        result += f"{offset:010} 00000 n \n".encode()
    result += f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{table}\n%%EOF\n".encode()
    path.write_bytes(result)


@unittest.skipUnless(all(shutil.which(executable) for executable in ("pdfinfo", "pdftoppm", "pdftotext")),
                     "Poppler tools required")
class DocumentsTests(unittest.TestCase):
    def test_pdf_and_caption_operations(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pdf, srt, output = root / "source", root / "subtitles", root / "result"
            sample_pdf(pdf)
            srt.write_text("1\n00:00:00,800 --> 00:00:01,900\nA & B\n\n2\n00:00:02,100 --> 00:00:03,000\nNext\n")
            engine = Documents(Limits(timeout_seconds=10))
            operations = {
                "pdf.inspect": ("pdf", pdf, {}, "application/json"),
                "pdf.render_page": ("pdf", pdf, {"page": 1, "long_edge": 320}, "image/png"),
                "pdf.extract_text": ("pdf", pdf, {}, "text/plain"),
                "captions.normalize": ("captions", srt, {}, "text/vtt"),
                "captions.shift": ("captions", srt, {"offset_ms": -900}, "text/vtt"),
                "captions.extract_text": ("captions", srt, {}, "text/plain"),
            }
            self.assertEqual(set(operations), {name for name, _ in engine.registry.operations})
            for operation, (slot, source, settings, mime) in operations.items():
                with self.subTest(operation=operation):
                    self.assertEqual(engine.execute(operation, 1, {slot: source}, settings, output), mime)
                    artifact = output.read_bytes()
                    if operation == "pdf.inspect":
                        self.assertEqual(json.loads(artifact)["pages"], 1)
                    elif operation == "pdf.render_page":
                        self.assertEqual(struct.unpack(">II", artifact[16:24]), (320, 160))
                    elif operation == "pdf.extract_text":
                        self.assertIn(b"TokenCraft source page", artifact)
                    elif operation == "captions.normalize":
                        self.assertIn(b"00:00:00.800 --> 00:00:01.900\nA &amp; B", artifact)
                    elif operation == "captions.shift":
                        self.assertIn(b"00:00:00.000 --> 00:00:01.000", artifact)
                    else:
                        self.assertEqual(artifact, b"A & B\n\nNext\n")
            with self.assertRaises(MediaError):
                engine.execute("pdf.render_page", 1, {"pdf": pdf}, {"page": 2}, output)
            with self.assertRaises(MediaError):
                engine.execute("pdf.inspect", 1, {"pdf": srt}, {}, output)
            with self.assertRaises(MediaError):
                engine.execute("captions.shift", 1, {"captions": srt}, {"offset_ms": -5000}, output)


class PackStateTests(unittest.TestCase):
    def test_state_stays_with_the_original_pack(self):
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            store = Store(state, max_requests=8, retention_seconds=60, request_bytes=512, pack="tokencraft-documents")
            store.close()
            with self.assertRaisesRegex(RuntimeError, "belongs to another tool pack"):
                Store(state, max_requests=8, retention_seconds=60, request_bytes=512)
            reopened = Store(state, max_requests=8, retention_seconds=60, request_bytes=512, pack="tokencraft-documents")
            reopened.close()
