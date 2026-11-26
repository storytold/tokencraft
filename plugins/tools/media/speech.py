"""An opt-in speech pack using an operator-supplied whisper.cpp model."""

from __future__ import annotations

import html
import json
from pathlib import Path

from contract import MediaError, Registry, produce_artifact, validate_request
from documents import timestamp
from engine import Engine, INPUT_OPTIONS, Limits, resolved_executable, run_command
from recipes import duration_ms

MAX_DURATION_MS = 600_000
MAX_TRANSCRIPT_BYTES = 4 * 1024 * 1024


def read_segments(path: Path, source_duration: int) -> tuple[str | None, list[dict]]:
    if not path.is_file() or not 0 < path.stat().st_size <= MAX_TRANSCRIPT_BYTES:
        raise MediaError("speech worker returned no usable output")
    try:
        result = json.loads(path.read_text(encoding="utf-8"))
    except (ValueError, RecursionError) as error:
        raise MediaError("speech worker returned an invalid transcript") from error
    if not isinstance(result, dict) or not isinstance(result.get("transcription"), list):
        raise MediaError("speech worker omitted the segment list")

    locale = result.get("result")
    language = locale.get("language") if isinstance(locale, dict) else None
    if not (isinstance(language, str) and 1 <= len(language) <= 8
            and language.isascii() and language.isalpha()):
        language = None
    final = []
    previous = 0
    for segment in result["transcription"]:
        offsets = segment.get("offsets") if isinstance(segment, dict) else None
        text = segment.get("text") if isinstance(segment, dict) else None
        if not isinstance(offsets, dict) or not isinstance(text, str) or "\0" in text:
            raise MediaError("speech worker returned an invalid segment")
        first, last = offsets.get("from"), offsets.get("to")
        if (type(first) is not int or type(last) is not int
                or not previous <= first <= last <= source_duration + 1000):
            raise MediaError("speech segment is outside the audio timeline")
        previous = first
        text = " ".join(text.split())
        first, last = min(first, source_duration), min(last, source_duration)
        if text and first < last:
            final.append({"start_ms": first, "end_ms": last, "text": text})
    return language, final


class Speech:
    registry = Registry(Path(__file__).with_name("speech.manifest.json"))

    def __init__(
        self, limits: Limits, ffmpeg: str, ffprobe: str, executable: str, model: Path,
        *, allow_gpu: bool = False,
    ):
        self.audio = Engine(limits, ffmpeg, ffprobe)
        self.executable = resolved_executable(executable)
        self.model = model.resolve()
        if not self.model.is_file() or self.model.stat().st_size == 0:
            raise MediaError("provide an existing local whisper.cpp model")
        self.limits = limits
        self.device = [] if allow_gpu else ["--no-gpu"]

    def execute(self, tool_id: str, revision: int, inputs: dict[str, Path], parameters: dict, output: Path) -> str:
        tool = validate_request(tool_id, revision, inputs, parameters, self.registry)
        return produce_artifact(
            tool, inputs, parameters, output, self.limits.input_bytes, self.limits.output_bytes, self.render,
        )

    def render(self, tool: dict, sources: dict[str, Path], parameters: dict, target: Path) -> str:
        source = sources["audio"]
        info = self.audio.inspect(source)
        if info["kind"] != "audio":
            raise MediaError("speech tools require an audio file")
        duration = duration_ms(info)
        if duration > MAX_DURATION_MS:
            raise MediaError("speech audio must be ten minutes or shorter")
        wave = target.with_suffix(".wav")
        prefix = target.with_suffix(".speech")
        run_command([
            *self.audio.command(), *INPUT_OPTIONS, "-threads", "1", "-i", str(source),
            "-map", "0:a:0", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le", "-t", "600",
            "-map_metadata", "-1", "-f", "wav", str(wave),
        ], min(self.limits.timeout_seconds, 30))
        operation = tool["id"].removeprefix("speech.")
        translation = ["--translate"] if operation == "translate" else []
        run_command([
            self.executable, "--model", str(self.model), "--file", str(wave),
            "--output-json", "--output-file", str(prefix), "--language", parameters["language"],
            "--no-prints", "--no-fallback", "--threads", "4", *self.device, *translation,
        ], self.limits.timeout_seconds)
        language, segments = read_segments(Path(f"{prefix}.json"), duration)

        if operation == "segments":
            target.write_text(json.dumps({
                "language": language, "duration_ms": duration, "segments": segments,
            }, ensure_ascii=False) + "\n", encoding="utf-8")
            return "application/json"
        if operation == "captions":
            lines = ["WEBVTT"]
            for segment in segments:
                first, last = timestamp(segment["start_ms"]), timestamp(segment["end_ms"])
                text = html.escape(segment["text"], quote=False)
                lines.append(f"{first} --> {last}\n{text}")
            target.write_text("\n\n".join(lines) + "\n\n", encoding="utf-8")
            return "text/vtt"
        target.write_text("\n".join(segment["text"] for segment in segments) + "\n", encoding="utf-8")
        return "text/plain"
