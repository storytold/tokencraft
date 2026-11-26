import base64
from hashlib import sha256
import json
from pathlib import Path
import struct
import tempfile
import unittest
from zipfile import ZipFile

from contract import MediaError
from delivery import Delivery
from engine import Limits
from meshes import GLB, Meshes, read_glb


def embedded_triangle(path: Path):
    vertices = struct.pack("<9f", 0, 0, 0, 1, 0, 0, 0, 1, 0)
    data_url = "data:application/octet-stream;base64," + base64.b64encode(vertices).decode("ascii")
    path.write_text(json.dumps({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0, "extras": {"comment": "fixture"}}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
        "buffers": [{"uri": data_url, "byteLength": len(vertices)}],
        "bufferViews": [{"buffer": 0, "byteLength": len(vertices), "target": 34962}],
        "accessors": [{
            "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
            "min": [0, 0, 0], "max": [1, 1, 0],
        }],
    }))
    return vertices


class MeshTests(unittest.TestCase):
    def test_pack_scale_strip_and_inspect(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "triangle.gltf"
            mesh = root / "triangle.glb"
            scaled = root / "scaled.glb"
            stripped = root / "stripped.glb"
            metadata = root / "mesh.json"
            vertices = embedded_triangle(source)
            runner = Meshes(Limits())
            self.assertEqual(runner.execute("mesh.to_glb", 1, {"source": source}, {}, mesh), GLB)
            self.assertEqual(runner.execute("mesh.scale", 1, {"source": mesh}, {"factor": 2}, scaled), GLB)
            self.assertEqual(runner.execute("mesh.strip_extras", 1, {"source": scaled}, {}, stripped), GLB)
            self.assertEqual(runner.execute("mesh.inspect", 1, {"source": stripped}, {}, metadata), "application/json")
            document, buffer = read_glb(stripped)
            self.assertEqual(buffer, vertices)
            self.assertEqual(document["scenes"][0]["nodes"], [1])
            self.assertEqual(document["nodes"][1]["children"], [0])
            self.assertEqual(document["nodes"][1]["scale"], [2, 2, 2])
            self.assertNotIn("extras", document["nodes"][0])
            self.assertEqual(json.loads(metadata.read_text())["meshes"], 1)
            altered = json.loads(source.read_text())
            altered["buffers"][0]["uri"] = "file:///var/tmp/triangle.bin"
            source.write_text(json.dumps(altered))
            with self.assertRaises(MediaError):
                runner.execute("mesh.to_glb", 1, {"source": source}, {}, mesh)
            mesh.write_bytes(scaled.read_bytes()[:-5])
            with self.assertRaises(MediaError):
                runner.execute("mesh.inspect", 1, {"source": mesh}, {}, metadata)


class DeliveryTests(unittest.TestCase):
    def test_deterministic_archives_and_fingerprint(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sources = {}
            for slot in ("first", "second", "third", "fourth"):
                path = root / slot
                path.write_text(slot + "\n")
                sources[slot] = path
            runner = Delivery(Limits())
            metadata = root / "fingerprint.json"
            artifact = root / "bundle.zip"
            self.assertEqual(runner.execute("delivery.fingerprint", 1, {"source": sources["first"]}, {}, metadata), "application/json")
            self.assertEqual(json.loads(metadata.read_text())["sha256"], sha256(b"first\n").hexdigest())
            for operation, inputs in [
                ("delivery.bundle_pair", dict(list(sources.items())[:2])),
                ("delivery.bundle_four", sources),
            ]:
                with self.subTest(operation=operation):
                    self.assertEqual(runner.execute(operation, 1, inputs, {}, artifact), "application/zip")
                    baseline = artifact.read_bytes()
                    runner.execute(operation, 1, dict(reversed(list(inputs.items()))), {}, artifact)
                    self.assertEqual(artifact.read_bytes(), baseline)
                    with ZipFile(artifact) as bundle:
                        items = json.loads(bundle.read("manifest.json"))["assets"]
                        self.assertEqual([item["slot"] for item in items], list(inputs))
                        for item in items:
                            self.assertEqual(sha256(bundle.read(item["path"])).hexdigest(), item["sha256"])
