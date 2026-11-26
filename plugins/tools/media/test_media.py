from pathlib import Path
import json
import shutil
import subprocess
import tempfile
import unittest

from contract import MediaError, TOOLS, validate_request
from engine import Engine, Limits


class ContractTests(unittest.TestCase):
    def test_strict_types_skips_neither_required_nor_unknown_parameters(self):
        for args in (
            ("video.trim", 2, {"video": True}, {"start_ms": 0, "duration_ms": 500}),
            ("video.trim", 1, {"video": True}, {"start_ms": False, "duration_ms": 500}),
            ("video.trim", 1, {"video": True}, {"start_ms": 0}),
            ("video.trim", 1, {"video": True}, {"start_ms": 0, "duration_ms": 500, "ffmpeg": []}),
            ("video.trim", 1, {"video": True, "audio": True}, {"start_ms": 0, "duration_ms": 500}),
            ("video.resize", 1, {"video": True}, {"width": 255, "height": 120}),
            ("video.change_speed", 1, {"video": True}, {"factor": float("nan")}),
            ("image.convert", 1, {"image": True}, {"format": "drawtext=text=local"}),
        ):
            with self.subTest(args=args), self.assertRaises(MediaError):
                validate_request(*args)


@unittest.skipUnless(shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg and ffprobe required")
class EngineTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        fixture = tempfile.TemporaryDirectory()
        cls.addClassCleanup(fixture.cleanup)
        cls.root = Path(fixture.name)
        cls.video = cls.root / "video.mp4"
        cls.muted = cls.root / "silent.mp4"
        cls.audio = cls.root / "audio.wav"
        cls.picture = cls.root / "image.png"
        cls.icon = cls.root / "icon.png"
        cls.engine = Engine()
        cls.make("-f", "lavfi", "-i", "testsrc2=s=96x64:r=24:d=1.5",
                 "-f", "lavfi", "-i", "sine=frequency=440:d=1.5",
                 "-c:v", "libx264", "-c:a", "aac", "-shortest", str(cls.video))
        cls.make("-f", "lavfi", "-i", "color=blue:s=64x48:r=24:d=1",
                 "-c:v", "libx264", str(cls.muted))
        cls.make("-f", "lavfi", "-i", "sine=frequency=880:d=1", "-c:a", "pcm_s16le", str(cls.audio))
        cls.make("-f", "lavfi", "-i", "color=red:s=96x64", "-frames:v", "1", str(cls.picture))
        cls.make("-f", "lavfi", "-i", "color=yellow:s=16x16", "-frames:v", "1", str(cls.icon))

    @classmethod
    def make(cls, *args):
        subprocess.run(
            [cls.engine.ffmpeg, "-nostdin", "-hide_banner", "-v", "error", *args],
            check=True, capture_output=True, timeout=15,
        )

    def test_every_manifest_operation_runs_in_ffmpeg(self):
        video, audio, picture = {"video": self.video}, {"audio": self.audio}, {"image": self.picture}
        test_cases = {
            "media.inspect": ({"source": self.video}, {}, "application/json"),
            "video.trim": (video, {"start_ms": 250, "duration_ms": 500}, "video/mp4"),
            "video.concat": ({"second": self.muted, "first": self.video}, {}, "video/mp4"),
            "video.attach_audio": ({"audio": self.audio, "video": self.muted}, {}, "video/mp4"),
            "video.extract_audio": (video, {}, "audio/wav"),
            "video.mute": (video, {}, "video/mp4"),
            "video.resize": (video, {"width": 64, "height": 48}, "video/mp4"),
            "video.crop": (video, {"x": 8, "y": 4, "width": 64, "height": 48}, "video/mp4"),
            "video.transcode": (video, {"crf": 20}, "video/mp4"),
            "video.change_speed": (video, {"factor": 1.5}, "video/mp4"),
            "video.thumbnail": (video, {"at_ms": 500}, "image/png"),
            "video.to_gif": (video, {"start_ms": 0, "duration_ms": 500, "width": 48, "fps": 8}, "image/gif"),
            "audio.trim": (audio, {"start_ms": 200, "duration_ms": 500}, "audio/wav"),
            "audio.concat": ({"first": self.audio, "second": self.audio}, {}, "audio/wav"),
            "audio.mix": ({"first": self.audio, "second": self.audio}, {}, "audio/wav"),
            "audio.normalize": (audio, {"lufs": -14}, "audio/wav"),
            "audio.fade": (audio, {"in_ms": 250, "out_ms": 250}, "audio/wav"),
            "audio.convert": (audio, {"format": "mp3"}, "audio/mpeg"),
            "image.resize": (picture, {"width": 19, "height": 37}, "image/png"),
            "image.crop": (picture, {"x": 1, "y": 3, "width": 45, "height": 31}, "image/png"),
            "image.convert": (picture, {"format": "webp"}, "image/webp"),
            "image.overlay": ({"overlay": self.icon, "image": self.picture}, {"x": 5, "y": 7}, "image/png"),
            "image.to_video": (picture, {"duration_ms": 500}, "video/mp4"),
        }
        self.assertEqual(set(test_cases), set(TOOLS))
        geometry = {
            "video.concat": (96, 64), "video.resize": (64, 48), "video.crop": (64, 48),
            "video.to_gif": (48, 32), "image.resize": (19, 37), "image.crop": (45, 31),
            "image.overlay": (96, 64), "image.to_video": (96, 64),
        }
        durations = {
            "video.trim": 500, "video.concat": 2500, "video.attach_audio": 1000,
            "video.change_speed": 1000, "audio.trim": 500, "audio.concat": 2000,
            "audio.mix": 1000, "image.to_video": 500,
        }
        for name, (inputs, parameters, content_type) in test_cases.items():
            with self.subTest(tool=name):
                output = self.root / name
                self.assertEqual(self.engine.execute(name, 1, inputs, parameters, output), content_type)
                if name == "media.inspect":
                    self.assertEqual(json.loads(output.read_bytes())["kind"], "video")
                    continue
                result = self.engine.inspect(output)
                if name in geometry:
                    stream = result["streams"][0]
                    self.assertEqual((stream["width"], stream["height"]), geometry[name])
                if name in durations:
                    self.assertAlmostEqual(result["duration_ms"], durations[name], delta=90)
                if name in ("video.concat", "video.attach_audio", "video.mute"):
                    audio_stream = any(s["type"] == "audio" for s in result["streams"])
                    self.assertEqual(audio_stream, name != "video.mute")

    def test_other_format_choices(self):
        for name, source, slot, options in (
            ("image.convert", self.picture, "image", {"png": "image/png", "jpeg": "image/jpeg"}),
            ("audio.convert", self.audio, "audio", {"wav": "audio/wav", "flac": "audio/flac"}),
        ):
            for file_type, mime in options.items():
                with self.subTest(tool=name, format=file_type):
                    self.assertEqual(self.engine.execute(
                        name, 1, {slot: source}, {"format": file_type}, self.root / f"result.{file_type}",
                    ), mime)

    def test_invalid_ranges_wrong_media_and_output_budget(self):
        target = self.root / "invalid-result"
        for engine, name, inputs, params in (
            (self.engine, "video.trim", {"video": self.video}, {"start_ms": 1000, "duration_ms": 1000}),
            (self.engine, "video.trim", {"video": self.audio}, {"start_ms": 0, "duration_ms": 500}),
            (self.engine, "image.crop", {"image": self.picture}, {"x": 8, "y": 0, "width": 96, "height": 16}),
            (Engine(Limits(output_bytes=512)), "video.transcode", {"video": self.video}, {}),
        ):
            with self.subTest(tool=name, params=params), self.assertRaises(MediaError):
                engine.execute(name, 1, inputs, params, target)
            self.assertFalse(target.exists())

    def test_input_container_allowlist_blocks_playlists(self):
        source = self.root / "playlist.m3u8"
        source.write_text("#EXTM3U\n#EXT-X-VERSION:3\n#EXTINF:1,\nhttps://example.invalid/media.ts\n")
        with self.assertRaises(MediaError):
            self.engine.inspect(source)


if __name__ == "__main__":
    unittest.main()
