"""Build fixed FFmpeg argument lists from validated tool parameters."""
from __future__ import annotations

from dataclasses import dataclass

from contract import MediaError


EVEN = "scale=ceil(iw/2)*2:ceil(ih/2)*2,setsar=1"
STEREO = "aresample=48000,aformat=channel_layouts=stereo"
MP4_AUDIO = ["-c:a", "aac", "-b:a", "128k"]
AUDIO_FORMATS = {
    "wav": ("audio/wav", ["-c:a", "pcm_s16le", "-f", "wav"]),
    "mp3": ("audio/mpeg", ["-c:a", "libmp3lame", "-b:a", "192k", "-f", "mp3"]),
    "flac": ("audio/flac", ["-c:a", "flac", "-f", "flac"]),
}
IMAGE_FORMATS = {
    "png": ("image/png", ["-c:v", "png", "-f", "image2", "-update", "1"]),
    "jpeg": ("image/jpeg", ["-c:v", "mjpeg", "-q:v", "3", "-f", "image2", "-update", "1"]),
    "webp": ("image/webp", ["-c:v", "libwebp", "-f", "webp"]),
}


@dataclass(frozen=True)
class Recipe:
    arguments: list[str]
    content_type: str


def seconds(milliseconds: int) -> str:
    return f"{milliseconds / 1000:.3f}"


def mp4(arguments: list[str], *, crf: int = 23) -> Recipe:
    return Recipe(arguments + [
        "-c:v", "libx264", "-preset", "veryfast", "-crf", str(crf),
        "-pix_fmt", "yuv420p", *MP4_AUDIO, "-movflags", "+faststart", "-f", "mp4",
    ], "video/mp4")


def sound(arguments: list[str], format_name: str = "wav") -> Recipe:
    content_type, options = AUDIO_FORMATS[format_name]
    return Recipe(arguments + ["-vn", "-ar", "48000", *options], content_type)


def picture(arguments: list[str], format_name: str = "png") -> Recipe:
    content_type, options = IMAGE_FORMATS[format_name]
    return Recipe(arguments + ["-an", "-frames:v", "1", *options], content_type)


def stream(info: dict, kind: str) -> dict:
    return next((entry for entry in info["streams"] if entry["type"] == kind), {})


def duration_ms(info: dict) -> int:
    value = info["duration_ms"]
    if value is None or value <= 0:
        raise MediaError("source duration is unavailable")
    return value


def check_range(source: dict, start_ms: int, length_ms: int) -> None:
    if start_ms + length_ms > duration_ms(source):
        raise MediaError("requested time range exceeds the source")


def trim(source: dict, parameters: dict) -> list[str]:
    start, length = parameters["start_ms"], parameters["duration_ms"]
    check_range(source, start, length)
    return ["-ss", seconds(start), "-t", seconds(length)]


def crop(source: dict, parameters: dict) -> str:
    frame = stream(source, "video")
    width, height, x, y = (parameters[key] for key in ("width", "height", "x", "y"))
    if width + x > frame["width"] or height + y > frame["height"]:
        raise MediaError("crop exceeds source bounds")
    return f"crop={width}:{height}:{x}:{y}:exact=1"


def join_video(first: dict, second: dict) -> Recipe:
    frame = stream(first, "video")
    width = (frame["width"] + 1) // 2 * 2
    height = (frame["height"] + 1) // 2 * 2
    segments = []
    for index, media in enumerate((first, second)):
        length = seconds(duration_ms(media))
        segments.append(
            f"[{index}:V:0]scale={width}:{height}:force_original_aspect_ratio=decrease,"
            f"pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=24,"
            f"trim=duration={length},setpts=PTS-STARTPTS[v{index}]"
        )
        audio = f"[{index}:a:0]{STEREO},apad" if stream(media, "audio") else "anullsrc=r=48000:cl=stereo"
        segments.append(f"{audio},atrim=duration={length},asetpts=PTS-STARTPTS[a{index}]")
    segments.append("[v0][a0][v1][a1]concat=n=2:v=1:a=1[v][a]")
    return mp4(["-filter_complex", ";".join(segments), "-map", "[v]", "-map", "[a]"])


def video(name: str, media: dict, p: dict) -> Recipe:
    source = next(iter(media.values()))
    mappings = ["-map", "0:V:0", "-map", "0:a:0?"]
    if name == "concat":
        return join_video(media["first"], media["second"])
    if name == "trim":
        return mp4(trim(source, p) + mappings + ["-vf", EVEN])
    if name == "attach_audio":
        length = seconds(duration_ms(source))
        return mp4(["-map", "0:V:0", "-map", "1:a:0", "-vf", EVEN, "-af", "apad", "-t", length])
    if name == "extract_audio":
        if not stream(source, "audio"):
            raise MediaError("the video has no audio track")
        return sound(["-map", "0:a:0"])
    if name == "mute":
        return mp4(["-map", "0:V:0", "-vf", EVEN, "-an"])
    if name == "resize":
        return mp4(mappings + ["-vf", f"scale={p['width']}:{p['height']}:flags=lanczos,setsar=1"])
    if name == "crop":
        return mp4(mappings + ["-vf", crop(source, p) + ",setsar=1"])
    if name == "transcode":
        return mp4(mappings + ["-vf", EVEN], crf=p["crf"])
    if name == "change_speed":
        factor = p["factor"]
        args = mappings + ["-vf", f"setpts=(PTS-STARTPTS)/{factor},{EVEN}"]
        if stream(source, "audio"):
            args += ["-af", f"atempo={factor}"]
        return mp4(args)
    if name == "thumbnail":
        check_range(source, p["at_ms"], 1)
        return picture(["-map", "0:V:0", "-ss", seconds(p["at_ms"])])
    if name == "to_gif":
        check_range(source, p["start_ms"], p["duration_ms"])
        start, end = seconds(p["start_ms"]), seconds(p["start_ms"] + p["duration_ms"])
        graph = (
            f"[0:V:0]trim=start={start}:end={end},setpts=PTS-STARTPTS,"
            f"fps={p['fps']},scale={p['width']}:-1:flags=lanczos,split[frames][palette_source];"
            "[palette_source]palettegen[palette];[frames][palette]paletteuse[out]"
        )
        return Recipe(["-filter_complex", graph, "-map", "[out]", "-an", "-loop", "0", "-f", "gif"], "image/gif")
    raise MediaError("unimplemented video tool")


def audio(name: str, media: dict, p: dict) -> Recipe:
    source = next(iter(media.values()))
    mapping = ["-map", "0:a:0"]
    if name == "trim":
        return sound(trim(source, p) + mapping)
    if name in ("concat", "mix"):
        filter_name = "concat=n=2:v=0:a=1" if name == "concat" else "amix=inputs=2:duration=longest"
        graph = f"[0:a:0]{STEREO},asetpts=PTS-STARTPTS[a];[1:a:0]{STEREO},asetpts=PTS-STARTPTS[b];[a][b]{filter_name}[out]"
        return sound(["-filter_complex", graph, "-map", "[out]"])
    if name == "normalize":
        return sound(mapping + ["-af", f"loudnorm=I={p['lufs']}:TP=-1.5:LRA=7"])
    if name == "fade":
        length = duration_ms(source)
        check_range(source, p["in_ms"], p["out_ms"])
        filters = []
        if p["in_ms"]:
            filters.append(f"afade=t=in:d={seconds(p['in_ms'])}")
        if p["out_ms"]:
            filters.append(f"afade=t=out:st={seconds(length - p['out_ms'])}:d={seconds(p['out_ms'])}")
        return sound(mapping + (["-af", ",".join(filters)] if filters else []))
    if name == "convert":
        return sound(mapping, p["format"])
    raise MediaError("unimplemented audio tool")


def image(name: str, media: dict, p: dict) -> Recipe:
    mapping = ["-map", "0:V:0"]
    if name == "resize":
        return picture(mapping + ["-vf", f"scale={p['width']}:{p['height']}:flags=lanczos"])
    if name == "crop":
        return picture(mapping + ["-vf", "format=rgba," + crop(media["image"], p)])
    if name == "convert":
        return picture(mapping, p["format"])
    if name == "overlay":
        base = stream(media["image"], "video")
        top = stream(media["overlay"], "video")
        if p["x"] + top["width"] > base["width"] or p["y"] + top["height"] > base["height"]:
            raise MediaError("overlay exceeds base image bounds")
        graph = f"[0:V:0][1:V:0]overlay={p['x']}:{p['y']}:format=rgb[out]"
        return picture(["-filter_complex", graph, "-map", "[out]"])
    if name == "to_video":
        return mp4(mapping + [
            "-vf", "loop=loop=-1:size=1:start=0,setpts=N/(24*TB)," + EVEN,
            "-r", "24", "-t", seconds(p["duration_ms"]), "-an",
        ])
    raise MediaError("unimplemented image tool")


RECIPE_FAMILIES = {"video": video, "audio": audio, "image": image}
