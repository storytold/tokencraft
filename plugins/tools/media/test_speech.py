import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

from contract import MediaError
from engine import Limits
from speech import Speech, read_segments

SPEECH_FIXTURE = '''import json
from pathlib import Path
import sys
import wave

flags = sys.argv[1:]
def option(flag):
    return flags[flags.index(flag) + 1]

assert "--no-gpu" in flags
assert "--output-json" in flags
assert Path(option("--model")).read_bytes() == b"fixture"
with wave.open(option("--file"), "rb") as audio:
    assert audio.getframerate() == 16000
    assert audio.getnchannels() == 1
    assert audio.getsampwidth() == 2
    assert audio.getnframes() == 16000
text = "Translated" if "--translate" in flags else "  Text <&> here  "
target = Path(option("--output-file") + ".json")
target.write_text(json.dumps({
    "result": {"language": "en"},
    "transcription": [{"offsets": {"from": 100, "to": 800}, "text": text}],
}))
'''


@unittest.skipUnless(shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg and ffprobe required")
class SpeechTests(unittest.TestCase):
    def test_speech_cli_contract_and_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            fake = folder / "whisper-cli"
            fake.write_text(f"#!{sys.executable}\n" + SPEECH_FIXTURE)
            os.chmod(fake, 0o700)
            model = folder / "fixture-model.bin"
            model.write_bytes(b"fixture")
            wave = folder / "audio.wav"
            output = folder / "result"
            engine = Speech(Limits(timeout_seconds=12), "ffmpeg", "ffprobe", str(fake), model)
            subprocess.run([
                engine.audio.ffmpeg, "-v", "error", "-f", "lavfi", "-i", "sine=440:d=1",
                "-c:a", "pcm_s16le", "-ar", "44100", str(wave),
            ], check=True, capture_output=True, timeout=12)
            operations = {
                "speech.transcribe": "text/plain", "speech.translate": "text/plain",
                "speech.captions": "text/vtt", "speech.segments": "application/json",
            }
            self.assertEqual(set(operations), {name for name, _ in engine.registry.operations})
            for operation, mime in operations.items():
                with self.subTest(operation=operation):
                    self.assertEqual(engine.execute(operation, 1, {"audio": wave}, {}, output), mime)
                    data = output.read_text()
                    if operation == "speech.transcribe":
                        self.assertEqual(data, "Text <&> here\n")
                    elif operation == "speech.translate":
                        self.assertEqual(data, "Translated\n")
                    elif operation == "speech.captions":
                        self.assertIn("00:00:00.100 --> 00:00:00.800\nText &lt;&amp;&gt; here", data)
                    else:
                        self.assertEqual(json.loads(data), {
                            "language": "en", "duration_ms": 1000,
                            "segments": [{"start_ms": 100, "end_ms": 800, "text": "Text <&> here"}],
                        })

    def test_invalid_speech_segment_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "result.json"
            source.write_text(json.dumps({
                "transcription": [{"offsets": {"from": -10, "to": 100}, "text": "Invalid"}],
            }))
            with self.assertRaises(MediaError):
                read_segments(source, 100)
