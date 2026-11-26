from pathlib import Path
import json
import shutil
import subprocess
import tempfile
import unittest

from studio import Studio


@unittest.skipUnless(shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg and ffprobe required")
class StudioTests(unittest.TestCase):
    def test_every_studio_operation(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            video, audio = folder / "video.mp4", folder / "audio.wav"
            image, icon = folder / "image.png", folder / "icon.png"
            cuts, quiet = folder / "cuts.mp4", folder / "quiet.wav"
            captions, output = folder / "captions", folder / "result"
            engine = Studio()

            def make(*args):
                subprocess.run([engine.ffmpeg, "-nostdin", "-v", "error", *args],
                               check=True, capture_output=True, timeout=12)

            make("-f", "lavfi", "-i", "testsrc2=s=96x64:r=24:d=1.5",
                 "-f", "lavfi", "-i", "sine=440:d=1.5", "-c:v", "libx264", "-c:a", "aac",
                 "-shortest", str(video))
            make("-f", "lavfi", "-i", "sine=880:d=1", "-c:a", "pcm_s16le", str(audio))
            make("-f", "lavfi", "-i", "color=red:s=96x64", "-frames:v", "1", str(image))
            make("-f", "lavfi", "-i", "color=blue:s=16x16", "-frames:v", "1", str(icon))
            make("-f", "lavfi", "-i", "color=black:s=96x64:r=24:d=0.75",
                 "-f", "lavfi", "-i", "color=white:s=96x64:r=24:d=0.75",
                 "-filter_complex", "concat=n=2:v=1:a=0", "-c:v", "libx264", str(cuts))
            make("-f", "lavfi", "-i", "anullsrc=r=8000:cl=mono:d=0.5",
                 "-f", "lavfi", "-i", "sine=880:d=0.5:r=8000",
                 "-filter_complex", "concat=n=2:v=0:a=1", "-c:a", "pcm_s16le", str(quiet))
            captions.write_text("1\n00:00:00,200 --> 00:00:00,900\nA timed caption\n")

            cases = {
                "video.letterbox": ({"video": video}, {"width": 120, "height": 120}, "video/mp4", (120, 120)),
                "video.rotate": ({"video": video}, {"turn": "clockwise"}, "video/mp4", (64, 96)),
                "video.frame_rate": ({"video": video}, {"fps": 15}, "video/mp4", None),
                "video.overlay": ({"video": video, "overlay": icon}, {"x": 3, "y": 7, "opacity": .75}, "video/mp4", (96, 64)),
                "video.contact_sheet": ({"video": video}, {"columns": 2, "rows": 2, "tile_size": 64}, "image/png", (134, 134)),
                "video.caption_track": ({"video": video, "captions": captions}, {}, "video/mp4", None),
                "video.to_webm": ({"video": video}, {}, "video/webm", None),
                "video.crossfade": ({"first": video, "second": cuts}, {"duration_ms": 250}, "video/mp4", (96, 64)),
                "image.letterbox": ({"image": image}, {"width": 120, "height": 120}, "image/png", (120, 120)),
                "image.rotate": ({"image": image}, {"turn": "counterclockwise"}, "image/png", (64, 96)),
                "image.flip": ({"image": image}, {"axis": "horizontal"}, "image/png", (96, 64)),
                "image.monochrome": ({"image": image}, {}, "image/png", (96, 64)),
                "image.chroma_key": ({"image": icon}, {"color": "blue"}, "image/png", (16, 16)),
                "image.alpha_mask": ({"image": image, "mask": image}, {}, "image/png", (96, 64)),
                "audio.change_speed": ({"audio": audio}, {"factor": 1.5}, "audio/wav", None),
                "audio.gain": ({"audio": audio}, {"db": -6}, "audio/wav", None),
                "audio.denoise": ({"audio": audio}, {}, "audio/wav", None),
                "audio.resample": ({"audio": audio}, {"rate": "16000"}, "audio/wav", None),
                "audio.mono": ({"audio": audio}, {}, "audio/wav", None),
                "audio.crossfade": ({"first": audio, "second": audio}, {"duration_ms": 250}, "audio/wav", None),
                "analysis.waveform": ({"audio": audio}, {"points": 32}, "application/json", None),
                "analysis.palette": ({"image": image}, {}, "application/json", None),
                "analysis.shots": ({"video": cuts}, {"sensitivity": 0.1}, "application/json", None),
                "analysis.silence": ({"audio": quiet}, {}, "application/json", None),
            }
            self.assertEqual(set(cases), {op for op, _ in engine.registry.operations})
            for name, (sources, parameters, mime, geometry) in cases.items():
                with self.subTest(operation=name):
                    self.assertEqual(engine.execute(name, 1, sources, parameters, output), mime)
                    if geometry:
                        result = engine.inspect(output)["streams"][0]
                        self.assertEqual((result["width"], result["height"]), geometry)
                    if name == "video.caption_track":
                        result = subprocess.check_output([
                            engine.ffprobe, "-v", "error", "-select_streams", "s:0",
                            "-show_entries", "stream=codec_name", "-of", "json", str(output),
                        ], timeout=10)
                        self.assertEqual(json.loads(result)["streams"][0]["codec_name"], "mov_text")
                    elif name == "audio.resample":
                        self.assertEqual(engine.inspect(output)["streams"][0]["sample_rate"], 16000)
                    elif name == "analysis.waveform":
                        record = json.loads(output.read_bytes())
                        self.assertEqual(record["duration_ms"], 1000)
                        self.assertEqual(len(record["points"]), 32)
                        self.assertTrue(all(0 < point["rms"] <= point["peak"] <= 1 for point in record["points"]))
                    elif name == "audio.crossfade":
                        self.assertEqual(engine.inspect(output)["duration_ms"], 1750)
                    elif name == "analysis.shots":
                        result = json.loads(output.read_bytes())
                        self.assertEqual(result, {"cuts_ms": [750], "truncated": False})
                    elif name == "analysis.silence":
                        result = json.loads(output.read_bytes())
                        self.assertEqual(result, {"duration_ms": 1000, "quiet": [{"start_ms": 0, "end_ms": 500}]})
                    elif name == "analysis.palette":
                        colors = json.loads(output.read_bytes())["colors"]
                        self.assertEqual(colors, [{"hex": "#f80808", "samples": 4096}])
