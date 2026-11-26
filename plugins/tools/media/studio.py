"""Additional bounded editing, delivery, and analysis recipes."""

from __future__ import annotations

from array import array
from collections import Counter
import json
import math
from pathlib import Path
import re
import sys

from contract import MediaError, Registry
from documents import render_captions
from engine import Engine, INPUT_OPTIONS, run_command
from recipes import EVEN, STEREO, Recipe, duration_ms, mp4, picture, seconds, sound, stream

ROTATION = {"clockwise": "transpose=clock", "counterclockwise": "transpose=cclock", "half": "hflip,vflip"}
VIDEO = ["-map", "0:V:0", "-map", "0:a:0?"]
IMAGE = ["-map", "0:V:0"]
AUDIO = ["-map", "0:a:0"]
SHOTS = re.compile(rb"pts_time:\s*([0-9]+(?:\.[0-9]+)?)")
CHROMA = {"green": "0x00ff00", "blue": "0x0000ff", "white": "0xffffff", "black": "0x000000"}


def frame(width: int, height: int) -> str:
    return (
        f"scale={width}:{height}:force_original_aspect_ratio=decrease:force_divisible_by=2,"
        f"pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1"
    )


def fades(media: dict, length: int) -> tuple[str, str]:
    first, second = (seconds(duration_ms(media[slot])) for slot in ("first", "second"))
    if length >= min(duration_ms(source) for source in media.values()):
        raise MediaError("crossfade must be shorter than both sources")
    graph = []
    for index, slot in enumerate(("first", "second")):
        source = media[slot]
        audio = f"[{index}:a:0]{STEREO},apad" if stream(source, "audio") else "anullsrc=r=48000:cl=stereo"
        graph.append(f"{audio},atrim=duration={(first, second)[index]},asetpts=PTS-STARTPTS[a{index}]")
    graph.append(f"[a0][a1]acrossfade=d={seconds(length)}:c1=tri:c2=tri[a]")
    return ";".join(graph), seconds(duration_ms(media["first"]) - length)


def video_recipe(name: str, media: dict, p: dict) -> Recipe:
    if name == "letterbox":
        return mp4(VIDEO + ["-vf", frame(p["width"], p["height"])])
    if name == "rotate":
        return mp4(VIDEO + ["-vf", ROTATION[p["turn"]] + "," + EVEN])
    if name == "frame_rate":
        return mp4(VIDEO + ["-vf", f"fps={p['fps']}," + EVEN])
    if name == "overlay":
        base = stream(media["video"], "video")
        overlay = stream(media["overlay"], "video")
        if p["x"] + overlay["width"] > base["width"] or p["y"] + overlay["height"] > base["height"]:
            raise MediaError("overlay exceeds frame bounds")
        graph = (
            f"[1:V:0]format=rgba,colorchannelmixer=aa={p['opacity']}[overlay];"
            f"[0:V:0][overlay]overlay={p['x']}:{p['y']}:eof_action=repeat[composed];"
            f"[composed]{EVEN}[out]"
        )
        return mp4(["-filter_complex", graph, "-map", "[out]", "-map", "0:a:0?"])
    if name == "contact_sheet":
        count = p["columns"] * p["rows"]
        length = duration_ms(media["video"])
        tile = p["tile_size"]
        return picture(IMAGE + ["-vf", (
            f"fps={count * 1000}/{length},{frame(tile, tile)},"
            f"tile={p['columns']}x{p['rows']}:nb_frames={count}:padding=2:margin=2"
        )])
    if name == "crossfade":
        source = stream(media["first"], "video")
        width = (source["width"] + 1) // 2 * 2
        height = (source["height"] + 1) // 2 * 2
        audio, offset = fades(media, p["duration_ms"])
        normalize = f"{frame(width, height)},fps=24,format=yuv444p,settb=AVTB,setpts=PTS-STARTPTS"
        transition = p["transition"]
        graph = (
            f"[0:V:0]{normalize}[first];[1:V:0]{normalize}[second];"
            f"[first][second]xfade=transition={transition}:duration={seconds(p['duration_ms'])}:offset={offset}[v];{audio}"
        )
        return mp4(["-filter_complex", graph, "-map", "[v]", "-map", "[a]"])
    if name == "to_webm":
        return Recipe(VIDEO + [
            "-vf", EVEN, "-c:v", "libvpx-vp9", "-crf", str(p["quality"]), "-b:v", "0",
            "-deadline", "good", "-cpu-used", "4", "-pix_fmt", "yuv420p",
            "-c:a", "libopus", "-b:a", "96k", "-f", "webm",
        ], "video/webm")
    raise MediaError("unsupported studio video operation")


def image_recipe(name: str, media: dict, p: dict) -> Recipe:
    if name == "letterbox":
        return picture(IMAGE + ["-vf", frame(p["width"], p["height"])])
    if name == "rotate":
        return picture(IMAGE + ["-vf", ROTATION[p["turn"]]])
    if name == "flip":
        return picture(IMAGE + ["-vf", "hflip" if p["axis"] == "horizontal" else "vflip"])
    if name == "monochrome":
        return picture(IMAGE + ["-vf", "format=gray"])
    if name == "chroma_key":
        return picture(IMAGE + ["-vf", f"format=rgba,colorkey={CHROMA[p['color']]}:{p['similarity']}:{p['blend']}"])
    if name == "alpha_mask":
        image, mask = (stream(media[key], "video") for key in ("image", "mask"))
        if (image["width"], image["height"]) != (mask["width"], mask["height"]):
            raise MediaError("alpha mask must have the same dimensions as the image")
        graph = "[0:V:0]format=rgba[image];[1:V:0]format=gray[mask];[image][mask]alphamerge[out]"
        return picture(["-filter_complex", graph, "-map", "[out]"])
    raise MediaError("unsupported studio image operation")


def audio_recipe(name: str, media: dict, p: dict) -> Recipe:
    if name == "change_speed":
        return sound(AUDIO + ["-af", f"atempo={p['factor']}"])
    if name == "gain":
        return sound(AUDIO + ["-af", f"volume={p['db']}dB"])
    if name == "denoise":
        return sound(AUDIO + ["-af", f"afftdn=nr={p['amount_db']}"])
    if name == "resample":
        return sound(AUDIO, sample_rate=p["rate"])
    if name == "mono":
        return sound(AUDIO + ["-ac", "1"])
    if name == "crossfade":
        graph, _ = fades(media, p["duration_ms"])
        return sound(["-filter_complex", graph, "-map", "[a]"])
    raise MediaError("unsupported studio audio operation")


STUDIO_RECIPES = {"video": video_recipe, "image": image_recipe, "audio": audio_recipe}


def pcm_samples(data: bytes) -> array:
    if not data or len(data) % 2:
        raise MediaError("audio did not produce usable samples")
    samples = array("h")
    samples.frombytes(data)
    if sys.byteorder != "little":
        samples.byteswap()
    return samples


def waveform(data: bytes, count: int) -> dict:
    samples = pcm_samples(data)
    rate = 8000
    count = min(count, len(samples))
    points = []
    for index in range(count):
        window = samples[index * len(samples) // count:(index + 1) * len(samples) // count]
        peak = max(abs(value) for value in window) / 32768
        rms = math.sqrt(sum(value * value for value in window) / len(window)) / 32768
        points.append({"peak": round(peak, 5), "rms": round(rms, 5)})
    return {"duration_ms": round(len(samples) * 1000 / rate), "points": points}


def silence(data: bytes, dbfs: int, minimum_ms: int) -> dict:
    samples = pcm_samples(data)
    threshold = 32768 * 10 ** (dbfs / 20)
    minimum = 8 * minimum_ms
    start = None
    intervals = []
    for index, value in enumerate(samples):
        if abs(value) <= threshold:
            if start is None:
                start = index
        elif start is not None:
            if index - start >= minimum:
                intervals.append({"start_ms": round(start / 8), "end_ms": round(index / 8)})
            start = None
    if start is not None and len(samples) - start >= minimum:
        intervals.append({"start_ms": round(start / 8), "end_ms": round(len(samples) / 8)})
    return {"duration_ms": round(len(samples) / 8), "quiet": intervals}


def palette(data: bytes, count: int) -> dict:
    if len(data) != 64 * 64 * 3:
        raise MediaError("image did not produce usable RGB samples")
    histogram = Counter((r >> 4, g >> 4, b >> 4) for r, g, b in zip(data[::3], data[1::3], data[2::3]))
    return {"colors": [
        {"hex": "#" + "".join(f"{channel * 16 + 8:02x}" for channel in color), "samples": samples}
        for color, samples in histogram.most_common(count)
    ]}


class Studio(Engine):
    registry = Registry(Path(__file__).with_name("studio.manifest.json"))

    def recipe(self, tool_id: str, media: dict, parameters: dict) -> Recipe:
        family, name = tool_id.split(".", 1)
        return STUDIO_RECIPES[family](name, media, parameters)

    def render(self, tool: dict, sources: dict[str, Path], parameters: dict, staging: Path) -> str:
        if tool["id"] == "video.caption_track":
            video = sources["video"]
            info = self.inspect(video)
            if info["kind"] != "video":
                raise MediaError("captioning requires a video")
            captions = staging.with_suffix(".vtt")
            render_captions("normalize", sources["captions"], {}, captions)
            recipe = mp4([
                *VIDEO, "-map", "1:s:0", "-vf", EVEN, "-c:s", "mov_text",
                "-t", seconds(duration_ms(info)),
            ])
            return self.encode("video", recipe, {"video": video}, staging, (
                "-protocol_whitelist", "file,pipe", "-f", "webvtt", "-i", str(captions),
            ))
        if tool["id"].startswith("analysis."):
            return self.analyze(tool["id"], sources, parameters, staging)
        return super().render(tool, sources, parameters, staging)

    def analyze(self, name: str, sources: dict[str, Path], parameters: dict, staging: Path) -> str:
        if name == "analysis.shots":
            expected = "video"
        elif name == "analysis.palette":
            expected = "image"
        else:
            expected = "audio"
        source = sources[expected]
        if self.inspect(source)["kind"] != expected:
            raise MediaError(f"analysis requires {expected} input")
        source_options = [*self.command(), *INPUT_OPTIONS, "-threads", "1", "-noautorotate", "-i", str(source)]
        if expected == "video":
            score = parameters["sensitivity"]
            raw = run_command([
                *self.command(), "-t", "600", *INPUT_OPTIONS, "-threads", "1", "-noautorotate",
                "-i", str(source), *IMAGE, "-an",
                "-vf", f"scale=320:-2,select=gt(scene\\,{score}),metadata=mode=print:file=-",
                "-f", "null", "-",
            ], self.limits.timeout_seconds, read_output=True)
            shot_times = [round(float(match) * 1000) for match in SHOTS.findall(raw)]
            result = {"cuts_ms": shot_times[:1000], "truncated": len(shot_times) > 1000}
            staging.write_text(json.dumps(result, sort_keys=True) + "\n", encoding="utf-8")
            return "application/json"
        raw = staging.with_suffix(".raw")
        if expected == "audio":
            options = [*AUDIO, "-t", "600", "-ar", "8000", "-ac", "1", "-c:a", "pcm_s16le", "-f", "s16le"]
        else:
            options = [*IMAGE, "-vf", "scale=64:64", "-frames:v", "1", "-pix_fmt", "rgb24", "-f", "rawvideo"]
        run_command([
            *source_options, *options, "-threads", "1", str(raw),
        ], self.limits.timeout_seconds)
        data = raw.read_bytes()
        if name == "analysis.waveform":
            record = waveform(data, parameters["points"])
        elif name == "analysis.silence":
            record = silence(data, parameters["threshold_dbfs"], parameters["minimum_ms"])
        else:
            record = palette(data, parameters["colors"])
        staging.write_text(json.dumps(record, sort_keys=True) + "\n", encoding="utf-8")
        return "application/json"
