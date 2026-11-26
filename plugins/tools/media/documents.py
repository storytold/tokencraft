"""Opt-in, bounded PDF and caption operations."""

from __future__ import annotations

from dataclasses import dataclass
import html
import json
from pathlib import Path
import re

from contract import MediaError, Registry, produce_artifact, validate_request
from engine import Limits, resolved_executable, run_command

MAX_CAPTION_BYTES = 1024 * 1024
TIMESTAMP = re.compile(r"(?:(\d{1,3}):)?([0-5]\d):([0-5]\d)[.,](\d{3})")


@dataclass(frozen=True)
class Cue:
    start: int
    end: int
    text: str


def cue_time(time: str) -> int:
    match = TIMESTAMP.fullmatch(time)
    if not match:
        raise MediaError("invalid caption timestamp")
    hours, minutes, seconds, millis = (int(value or 0) for value in match.groups())
    return ((hours * 60 + minutes) * 60 + seconds) * 1000 + millis


def captions(path: Path) -> list[Cue]:
    if path.stat().st_size > MAX_CAPTION_BYTES:
        raise MediaError("caption file exceeds its byte limit")
    try:
        text = path.read_text(encoding="utf-8-sig")
    except UnicodeError as error:
        raise MediaError("subtitle file must be UTF-8") from error
    cues = []
    for block in re.split(r"\n[ \t]*\n", text.strip()):
        lines = block.splitlines()
        if lines and (lines[0].startswith(("WEBVTT", "NOTE ")) or lines[0] == "NOTE"):
            continue
        if lines and "-->" not in lines[0]:
            lines = lines[1:]
        if len(lines) < 2 or "-->" not in lines[0]:
            raise MediaError("expected timed SRT or WebVTT cues")
        start, end = lines[0].split("-->", 1)
        end_fields = end.split()
        if not end_fields:
            raise MediaError("caption end time is missing")
        first, last = cue_time(start.strip()), cue_time(end_fields[0])
        content = "\n".join(lines[1:]).strip()
        if last <= first or not content or "\0" in content:
            raise MediaError("empty or invalid caption cue")
        cues.append(Cue(first, last, content))
    if not cues:
        raise MediaError("caption file has no timed cues")
    return sorted(cues, key=lambda cue: (cue.start, cue.end))


def timestamp(value: int) -> str:
    seconds, millis = divmod(value, 1000)
    minutes, seconds = divmod(seconds, 60)
    hours, minutes = divmod(minutes, 60)
    return f"{hours:02}:{minutes:02}:{seconds:02}.{millis:03}"


def render_captions(name: str, source: Path, parameters: dict, target: Path) -> str:
    cues = captions(source)
    if name == "extract_text":
        target.write_text("\n\n".join(html.unescape(cue.text) for cue in cues) + "\n", encoding="utf-8")
        return "text/plain"
    offset = parameters.get("offset_ms", 0)
    blocks = ["WEBVTT"]
    for cue in cues:
        if cue.end + offset > 0:
            start = timestamp(max(0, cue.start + offset))
            end = timestamp(cue.end + offset)
            text = html.escape(html.unescape(cue.text), quote=False)
            blocks.append(f"{start} --> {end}\n{text}")
    if len(blocks) == 1:
        raise MediaError("caption offset removes every cue")
    target.write_text("\n\n".join(blocks) + "\n", encoding="utf-8")
    return "text/vtt"


class Documents:
    registry = Registry(Path(__file__).with_name("documents.manifest.json"))

    def __init__(self, limits: Limits):
        self.limits = limits
        self.pdfinfo = resolved_executable("pdfinfo")
        self.pdftoppm = resolved_executable("pdftoppm")
        self.pdftotext = resolved_executable("pdftotext")

    def execute(self, tool_id: str, revision: int, inputs: dict[str, Path], parameters: dict, output: Path) -> str:
        tool = validate_request(tool_id, revision, inputs, parameters, self.registry)
        return produce_artifact(
            tool, inputs, parameters, output, self.limits.input_bytes, self.limits.output_bytes, self.render,
        )

    def render(self, tool: dict, sources: dict[str, Path], parameters: dict, target: Path) -> str:
        family, operation = tool["id"].split(".", 1)
        if family == "captions":
            return render_captions(operation, sources["captions"], parameters, target)
        source = sources["pdf"]
        with source.open("rb") as uploaded:
            if uploaded.read(5) != b"%PDF-":
                raise MediaError("source must be a PDF")

        info = run_command([self.pdfinfo, str(source)], min(15, self.limits.timeout_seconds), read_output=True)
        entries = dict(line.split(":", 1) for line in info.decode("utf-8", "replace").splitlines() if ":" in line)
        pages = entries.get("Pages", "").strip()
        if not pages.isascii() or not pages.isdecimal() or not 0 < int(pages) <= 1000:
            raise MediaError("PDF exceeds the page limit or could not be read")
        if entries.get("Encrypted", "").strip().startswith("yes"):
            raise MediaError("encrypted PDFs are unsupported")
        total = int(pages)

        if operation == "inspect":
            record = {
                "pages": total, "pdf_version": entries.get("PDF version", "").strip(),
                "size_bytes": source.stat().st_size,
            }
            target.write_text(json.dumps(record, sort_keys=True) + "\n", encoding="utf-8")
            return "application/json"
        if operation == "render_page":
            page = parameters["page"]
            if page > total:
                raise MediaError("requested page exceeds the document")
            run_command([
                self.pdftoppm, "-q", "-f", str(page), "-l", str(page), "-singlefile",
                "-scale-to", str(parameters["long_edge"]), "-png", str(source), str(target),
            ], self.limits.timeout_seconds)
            raster = target.with_suffix(".png")
            if not raster.is_file():
                raise MediaError("PDF page rendering returned no image")
            raster.replace(target)
            return "image/png"

        first, last = parameters["first_page"], parameters["last_page"]
        if not first <= last <= total or last - first >= 32:
            raise MediaError("choose an existing range of up to 32 PDF pages")
        layout = ["-layout"] if parameters["order"] == "layout" else []
        run_command([
            self.pdftotext, "-q", "-f", str(first), "-l", str(last), "-enc", "UTF-8", "-nopgbrk",
            *layout, str(source), str(target),
        ], self.limits.timeout_seconds)
        return "text/plain"
