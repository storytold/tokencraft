"""Self-contained glTF 2.0 inspection and file transformations."""

from __future__ import annotations

import base64
import json
from pathlib import Path
import struct

from contract import MediaError, Registry, produce_artifact, validate_request
from engine import Limits

GLB = "model/gltf-binary"
MAX_JSON = 4 * 1024 * 1024
JSON_CHUNK = b"JSON"
BIN_CHUNK = b"BIN\0"


def decode_document(payload: bytes) -> dict:
    def unique_object(items):
        result = {}
        for name, value in items:
            if name in result:
                raise MediaError("glTF has duplicate properties")
            result[name] = value
        return result

    def reject_constant(_value):
        raise MediaError("glTF numbers must be finite")

    if not 0 < len(payload) <= MAX_JSON:
        raise MediaError("glTF JSON exceeds its size limit")
    try:
        document = json.loads(
            payload.decode("utf-8"), object_pairs_hook=unique_object, parse_constant=reject_constant,
        )
    except (ValueError, RecursionError) as error:
        raise MediaError("invalid glTF JSON") from error
    if not isinstance(document, dict) or not isinstance(document.get("asset"), dict):
        raise MediaError("glTF lacks an asset definition")
    if document["asset"].get("version") != "2.0":
        raise MediaError("glTF version 2.0 is required")
    return document


def objects(document: dict, field: str) -> list[dict]:
    value = document.get(field, [])
    if not isinstance(value, list) or any(not isinstance(item, dict) for item in value):
        raise MediaError(f"glTF {field} must contain objects")
    return value


def chunk(file, limit: int, expected: bytes) -> bytes:
    header = file.read(8)
    if len(header) != 8:
        raise MediaError("truncated GLB chunk")
    size, tag = struct.unpack("<I4s", header)
    if tag != expected or size % 4 or size > limit:
        raise MediaError("GLB chunk type, alignment, or size is invalid")
    data = file.read(size)
    if len(data) != size:
        raise MediaError("truncated GLB chunk contents")
    return data


def read_glb(source: Path) -> tuple[dict, bytes]:
    length = source.stat().st_size
    with source.open("rb") as file:
        header = file.read(12)
        if len(header) != 12 or struct.unpack("<4sII", header) != (b"glTF", 2, length):
            raise MediaError("GLB header is invalid")
        document = decode_document(chunk(file, MAX_JSON, JSON_CHUNK))
        binary = b""
        if file.tell() < length:
            binary = chunk(file, length - file.tell() - 8, BIN_CHUNK)
        if file.tell() != length:
            raise MediaError("unsupported trailing GLB chunks")
    return document, embedded_buffers(document, binary)


def embedded_buffers(document: dict, binary: bytes) -> bytes:
    buffers = objects(document, "buffers")
    images = objects(document, "images")
    if any("uri" in image for image in images) or len(buffers) > 1:
        raise MediaError("glTF must use one embedded buffer and embedded images")
    length = 0
    if buffers:
        first = buffers[0]
        length = first.get("byteLength")
        if "uri" in first or type(length) is not int or not 0 < length <= len(binary):
            raise MediaError("glTF buffer is unavailable or invalid")
    if not 0 <= len(binary) - length <= 3 or any(binary[length:]):
        raise MediaError("GLB binary padding is invalid")
    views = objects(document, "bufferViews")
    for view in views:
        offset, size, index = view.get("byteOffset", 0), view.get("byteLength"), view.get("buffer")
        if (type(index) is not int or index != 0 or type(offset) is not int or type(size) is not int
                or offset < 0 or size <= 0 or offset + size > length):
            raise MediaError("glTF buffer view is outside the embedded buffer")
    if any(type(image.get("bufferView")) is not int or not 0 <= image["bufferView"] < len(views) for image in images):
        raise MediaError("glTF image is outside the embedded buffer")
    return binary[:length]


def pack_json(source: Path) -> tuple[dict, bytes]:
    if source.stat().st_size > MAX_JSON:
        raise MediaError("embedded glTF source exceeds its size limit")
    document = decode_document(source.read_bytes())
    buffers = objects(document, "buffers")
    binary = b""
    if buffers:
        uri = buffers[0].pop("uri", None)
        if not isinstance(uri, str):
            raise MediaError("glTF buffer requires an embedded data URI")
        descriptor, separator, payload = uri.partition(",")
        if not separator or descriptor not in (
            "data:application/octet-stream;base64", "data:application/gltf-buffer;base64",
        ):
            raise MediaError("glTF buffer requires an embedded base64 data URI")
        try:
            binary = base64.b64decode(payload, validate=True)
        except ValueError as error:
            raise MediaError("invalid glTF base64 buffer") from error
        if len(binary) != buffers[0].get("byteLength"):
            raise MediaError("embedded glTF buffer size does not match its declaration")
    return document, embedded_buffers(document, binary)


def pad(data: bytes, byte: bytes) -> bytes:
    return data + byte * (-len(data) % 4)


def write_glb(document: dict, binary: bytes, target: Path) -> None:
    try:
        encoded = pad(json.dumps(document, ensure_ascii=True, separators=(",", ":"), allow_nan=False).encode(), b" ")
    except (ValueError, RecursionError) as error:
        raise MediaError("cannot serialize glTF") from error
    if len(encoded) > MAX_JSON:
        raise MediaError("GLB JSON would exceed its size limit")
    binary = pad(binary, b"\0")
    length = 20 + len(encoded) + (8 + len(binary) if binary else 0)
    with target.open("wb") as file:
        file.write(struct.pack("<4sIII4s", b"glTF", 2, length, len(encoded), JSON_CHUNK))
        file.write(encoded)
        if binary:
            file.write(struct.pack("<I4s", len(binary), BIN_CHUNK))
            file.write(binary)


def scale_scenes(document: dict, factor: float) -> None:
    scenes = objects(document, "scenes")
    if not scenes:
        raise MediaError("scaling requires at least one scene")
    nodes = objects(document, "nodes")
    original_length = len(nodes)
    for scene in scenes:
        roots = scene.get("nodes", [])
        if not isinstance(roots, list) or any(type(node) is not int or not 0 <= node < original_length for node in roots):
            raise MediaError("glTF scene references an unknown node")
        scene["nodes"] = [len(nodes)]
        node = {"name": "TokenCraft scene scale", "scale": [factor] * 3}
        if roots:
            node["children"] = roots
        nodes.append(node)
    document["nodes"] = nodes


def strip_extras(document: dict) -> None:
    pending = [document]
    while pending:
        item = pending.pop()
        if isinstance(item, dict):
            item.pop("extras", None)
            pending.extend(item.values())
        elif isinstance(item, list):
            pending.extend(item)


class Meshes:
    registry = Registry(Path(__file__).with_name("mesh.manifest.json"))

    def __init__(self, limits: Limits):
        self.limits = limits

    def execute(self, tool_id: str, revision: int, inputs: dict[str, Path], parameters: dict, output: Path) -> str:
        tool = validate_request(tool_id, revision, inputs, parameters, self.registry)
        return produce_artifact(
            tool, inputs, parameters, output, self.limits.input_bytes, self.limits.output_bytes, self.render,
        )

    @staticmethod
    def render(tool: dict, sources: dict[str, Path], parameters: dict, target: Path) -> str:
        source = sources["source"]
        name = tool["id"]
        document, binary = pack_json(source) if name == "mesh.to_glb" else read_glb(source)
        if name == "mesh.inspect":
            fields = ("scenes", "nodes", "meshes", "materials", "textures", "images", "animations")
            record = {name: len(objects(document, name)) for name in fields}
            record.update(size_bytes=source.stat().st_size, buffer_bytes=len(binary), version="2.0")
            target.write_text(json.dumps(record, sort_keys=True) + "\n", encoding="utf-8")
            return "application/json"
        if name == "mesh.scale":
            scale_scenes(document, parameters["factor"])
        elif name == "mesh.strip_extras":
            strip_extras(document)
        write_glb(document, binary, target)
        return GLB
