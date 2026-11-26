"""Bounded local media processing for the optional TokenCraft tool plugin."""
from __future__ import annotations

from dataclasses import dataclass
import json
from pathlib import Path
import math
import shutil
import subprocess

from contract import DEFAULT_REGISTRY, MediaError, produce_artifact, validate_request
from recipes import RECIPE_FAMILIES


IMAGE_FORMATS = frozenset(("png_pipe", "jpeg_pipe", "webp_pipe", "gif", "image2"))
INPUT_FORMATS = ",".join(sorted(IMAGE_FORMATS | {
    "mov", "matroska", "webm", "wav", "mp3", "flac", "ogg",
}))
INPUT_OPTIONS = [
    "-protocol_whitelist", "file,pipe", "-format_whitelist", INPUT_FORMATS, "-max_streams", "16",
]
MAX_DIMENSION = 4096
MAX_DURATION_MS = 3_600_000


@dataclass(frozen=True)
class Limits:
    input_bytes: int = 64 * 1024 * 1024
    output_bytes: int = 64 * 1024 * 1024
    timeout_seconds: int = 90


def resolved_executable(name: str) -> str:
    path = shutil.which(name)
    if path is None:
        raise MediaError(f"{name} is required for the optional media plugin")
    return str(Path(path).resolve())


def run_command(args: list[str], timeout: int, *, read_output: bool = False) -> bytes:
    try:
        completed = subprocess.run(
            args, stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE if read_output else subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, timeout=timeout, check=False,
        )
    except subprocess.TimeoutExpired as error:
        raise MediaError("media operation exceeded its time limit") from error
    except OSError as error:
        raise MediaError("media executable could not start") from error
    if completed.returncode:
        raise MediaError("media executable rejected the operation")
    return completed.stdout if read_output else b""


def parse_duration(value: object) -> int | None:
    try:
        duration = float(value)
    except (TypeError, ValueError):
        return None
    if not math.isfinite(duration) or duration <= 0:
        return None
    if duration * 1000 > MAX_DURATION_MS:
        raise MediaError("media duration exceeds the configured processing envelope")
    return round(duration * 1000)


class Engine:
    registry = DEFAULT_REGISTRY

    def __init__(self, limits: Limits = Limits(), ffmpeg: str = "ffmpeg", ffprobe: str = "ffprobe"):
        self.limits = limits
        self.ffmpeg = resolved_executable(ffmpeg)
        self.ffprobe = resolved_executable(ffprobe)

    def inspect(self, source: Path, *, byte_limit: int | None = None) -> dict:
        if not source.is_file():
            raise MediaError("source must be a regular file")
        data_size = source.stat().st_size
        if byte_limit is None:
            byte_limit = self.limits.input_bytes
        if not 0 < data_size <= byte_limit:
            raise MediaError("source is empty or exceeds the byte limit")
        response = run_command([
            self.ffprobe, "-v", "error", *INPUT_OPTIONS, "-max_alloc", str(128 * 1024 * 1024),
            "-probesize", str(min(byte_limit, 8 * 1024 * 1024)), "-analyzeduration", "5000000",
            "-show_entries",
            "format=format_name,duration:stream=codec_type,codec_name,width,height,sample_rate,channels"
            ":stream_disposition=attached_pic",
            "-of", "json", "-i", str(source.resolve()),
        ], min(self.limits.timeout_seconds, 15), read_output=True)
        if len(response) > 256 * 1024:
            raise MediaError("media stream inventory is too large")
        data = json.loads(response)
        metadata = data.get("format", {})
        formats = metadata.get("format_name", "").split(",")
        source_duration = parse_duration(metadata.get("duration"))
        streams = []
        for item in data.get("streams", ()):
            kind = item["codec_type"]
            if kind not in ("video", "audio") or item.get("disposition", {}).get("attached_pic"):
                continue
            stream = {"type": kind, "codec": item.get("codec_name")}
            if kind == "video":
                width, height = item.get("width", 0), item.get("height", 0)
                if not 0 < width <= MAX_DIMENSION or not 0 < height <= MAX_DIMENSION:
                    raise MediaError("frame dimensions exceed the configured processing envelope")
                stream.update(width=width, height=height)
            else:
                rate, channels = int(item.get("sample_rate", 0)), item.get("channels", 0)
                if not 0 < rate <= 192_000 or not 0 < channels <= 16:
                    raise MediaError("audio stream exceeds the configured processing envelope")
                stream.update(sample_rate=rate, channels=channels)
            streams.append(stream)
        if not streams or len(streams) > 16:
            raise MediaError("the source needs between one and sixteen usable streams")
        if any(kind in IMAGE_FORMATS for kind in formats):
            kind = "image"
        elif any(item["type"] == "video" for item in streams):
            kind = "video"
        else:
            kind = "audio"
        return {
            "kind": kind, "formats": formats, "duration_ms": source_duration,
            "size_bytes": data_size, "streams": streams,
        }

    def execute(
        self, tool_id: str, revision: int, inputs: dict[str, Path], parameters: dict, output: Path,
    ) -> str:
        tool = validate_request(tool_id, revision, inputs, parameters, self.registry)
        return produce_artifact(
            tool, inputs, parameters, output, self.limits.input_bytes, self.limits.output_bytes, self.render,
        )

    def render(self, tool: dict, sources: dict[str, Path], parameters: dict, staging: Path) -> str:
        media = {slot: self.inspect(source) for slot, source in sources.items()}
        for slot, source in media.items():
            expected = tool["inputs"][slot]["kind"]
            if expected != "media" and source["kind"] != expected:
                raise MediaError(f"input slot {slot} requires {expected}")

        if tool["id"] == "media.inspect":
            staging.write_text(json.dumps(media["source"], sort_keys=True) + "\n", encoding="utf-8")
            return "application/json"
        family, name = tool["id"].split(".", 1)
        recipe = RECIPE_FAMILIES[family](name, media, parameters)
        arguments = [
            self.ffmpeg, "-nostdin", "-hide_banner", "-v", "error", "-xerror",
            "-filter_threads", "1", "-filter_complex_threads", "1",
            "-max_alloc", str(128 * 1024 * 1024),
        ]
        for source in sources.values():
            arguments += [*INPUT_OPTIONS, "-threads", "1", "-noautorotate", "-i", str(source)]
        arguments += [
            *recipe.arguments, "-map_metadata", "-1", "-map_chapters", "-1",
            "-threads", "1", "-fs", str(self.limits.output_bytes), str(staging),
        ]
        run_command(arguments, self.limits.timeout_seconds)
        if not staging.is_file() or staging.stat().st_size >= self.limits.output_bytes:
            raise MediaError("output was missing or reached its byte limit")
        result = self.inspect(staging, byte_limit=self.limits.output_bytes)
        if result["kind"] != tool["output"]["kind"]:
            raise MediaError("output does not match the declared media kind")
        return recipe.content_type
