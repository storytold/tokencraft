"""Deterministic local delivery bundles and asset checksums."""

from __future__ import annotations

from hashlib import file_digest
import json
from pathlib import Path
import shutil
from zipfile import ZipFile, ZipInfo, ZIP_DEFLATED

from contract import MediaError, Registry, produce_artifact, validate_request
from engine import Limits

BLOCK_SIZE = 64 * 1024


def identify(path: Path, prefix: bytes) -> tuple[str, str]:
    if prefix.startswith(b"\x89PNG\r\n\x1a\n"):
        return "image/png", "png"
    if prefix.startswith(b"\xff\xd8\xff"):
        return "image/jpeg", "jpg"
    if prefix.startswith((b"GIF87a", b"GIF89a")):
        return "image/gif", "gif"
    if prefix.startswith(b"RIFF"):
        return {"WEBP": ("image/webp", "webp"), "WAVE": ("audio/wav", "wav")}.get(
            prefix[8:12].decode("ascii", "ignore"), ("application/octet-stream", "bin"),
        )
    if prefix[4:8] == b"ftyp":
        return ("video/quicktime", "mov") if prefix[8:12] == b"qt  " else ("video/mp4", "mp4")
    if prefix.startswith(b"\x1a\x45\xdf\xa3"):
        return "video/webm", "webm"
    if prefix.startswith(b"fLaC"):
        return "audio/flac", "flac"
    if prefix.startswith(b"OggS"):
        return "audio/ogg", "ogg"
    if prefix.startswith(b"ID3") or len(prefix) >= 2 and prefix[0] == 255 and prefix[1] & 224 == 224:
        return "audio/mpeg", "mp3"
    if prefix.startswith(b"%PDF-"):
        return "application/pdf", "pdf"
    if prefix.startswith(b"glTF"):
        return "model/gltf-binary", "glb"
    try:
        with path.open("r", encoding="utf-8-sig") as data:
            start = data.read(1024)
    except UnicodeError:
        return "application/octet-stream", "bin"
    if start.startswith("WEBVTT"):
        return "text/vtt", "vtt"
    if start.lstrip().startswith("{") and path.stat().st_size <= 4 * 1024 * 1024:
        try:
            record = json.loads(path.read_text(encoding="utf-8-sig"))
        except (ValueError, RecursionError):
            record = None
        asset = record.get("asset") if isinstance(record, dict) else None
        if isinstance(asset, dict) and asset.get("version") == "2.0":
            return "model/gltf+json", "gltf"
    if "\0" in start:
        return "application/octet-stream", "bin"
    return "text/plain", "txt"


def describe(path: Path) -> tuple[dict, str]:
    with path.open("rb") as data:
        prefix = data.read(16)
        data.seek(0)
        digest = file_digest(data, "sha256").hexdigest()
    mime, extension = identify(path, prefix)
    return {"content_type": mime, "size_bytes": path.stat().st_size, "sha256": digest}, extension


def zip_entry(name: str) -> ZipInfo:
    info = ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
    info.compress_type = ZIP_DEFLATED
    info.external_attr = 0o100644 << 16
    return info


class Delivery:
    registry = Registry(Path(__file__).with_name("delivery.manifest.json"))

    def __init__(self, limits: Limits):
        self.limits = limits

    def execute(self, tool_id: str, revision: int, inputs: dict[str, Path], parameters: dict, output: Path) -> str:
        tool = validate_request(tool_id, revision, inputs, parameters, self.registry)
        return produce_artifact(
            tool, inputs, parameters, output, self.limits.input_bytes, self.limits.output_bytes, self.render,
        )

    def render(self, tool: dict, sources: dict[str, Path], _parameters: dict, target: Path) -> str:
        if tool["id"] == "delivery.fingerprint":
            record, _ = describe(sources["source"])
            target.write_text(json.dumps(record, sort_keys=True) + "\n", encoding="utf-8")
            return "application/json"

        entries = []
        with ZipFile(target, "w") as bundle:
            slots = (slot for slot in ("first", "second", "third", "fourth") if slot in sources)
            for index, slot in enumerate(slots, 1):
                source = sources[slot]
                record, extension = describe(source)
                name = f"{index:02}-{slot}.{extension}"
                info = zip_entry(name)
                info.file_size = record["size_bytes"]
                with source.open("rb") as data, bundle.open(info, "w") as entry:
                    shutil.copyfileobj(data, entry, BLOCK_SIZE)
                if target.stat().st_size >= self.limits.output_bytes:
                    raise MediaError("bundle reached its byte limit")
                entries.append({"path": name, "slot": slot, **record})
            bundle.writestr(zip_entry("manifest.json"), json.dumps({"version": 1, "assets": entries}, sort_keys=True) + "\n")
        return "application/zip"
