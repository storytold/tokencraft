"""Version 1 media tool descriptors and request validation."""
from __future__ import annotations

import json
import math
import os
from pathlib import Path
import tempfile
from collections.abc import Callable


class MediaError(ValueError):
    """An invalid request, unsupported source, or failed media operation."""


class Registry:
    """One reviewed manifest, including any retained operation revisions."""

    def __init__(self, path: Path):
        self.manifest = json.loads(path.read_bytes())
        self.operations = {(entry["id"], entry["revision"]): entry for entry in self.manifest["tools"]}
        if self.manifest["version"] != 1 or len(self.operations) != len(self.manifest["tools"]):
            raise MediaError("unsupported or duplicate tool descriptors")

    def operation(self, tool_id: str, revision: int) -> dict:
        if not isinstance(tool_id, str) or type(revision) is not int:
            raise MediaError("unsupported tool or revision")
        operation = self.operations.get((tool_id, revision))
        if operation is None:
            raise MediaError("unsupported tool or revision")
        return operation


DEFAULT_REGISTRY = Registry(Path(__file__).with_name("manifest.json"))
TOOLS = {tool_id: tool for (tool_id, _), tool in DEFAULT_REGISTRY.operations.items()}


def validate_request(
    tool_id: str, revision: int, slots: object, parameters: object, registry: Registry = DEFAULT_REGISTRY,
) -> dict:
    tool = registry.operation(tool_id, revision)
    if not isinstance(slots, dict) or slots.keys() != tool["inputs"].keys():
        raise MediaError("input slots must match the tool manifest")
    schema = tool["parameters"]
    properties = schema["properties"]
    if not isinstance(parameters, dict) or parameters.keys() - properties.keys():
        raise MediaError("unknown tool parameter")
    if set(schema["required"]) - parameters.keys():
        raise MediaError("missing required tool parameter")
    for name, value in parameters.items():
        spec = properties[name]
        kind = spec["type"]
        if kind == "string":
            if not isinstance(value, str) or value not in spec["enum"]:
                raise MediaError(f"invalid value for {name}")
            continue
        if kind == "integer":
            valid_type = type(value) is int
        else:
            valid_type = type(value) in (int, float)
        if not valid_type or not spec["minimum"] <= value <= spec["maximum"]:
            raise MediaError(f"value for {name} is outside its allowed range")
        if isinstance(value, float) and not math.isfinite(value):
            raise MediaError(f"value for {name} must be finite")
        if "multipleOf" in spec and value % spec["multipleOf"]:
            raise MediaError(f"value for {name} must be divisible by {spec['multipleOf']}")
    return tool


def produce_artifact(
    tool: dict,
    inputs: dict[str, Path],
    parameters: dict,
    output: Path,
    input_limit: int,
    output_limit: int,
    render: Callable[[dict, dict[str, Path], dict, Path], str],
) -> str:
    sources = {slot: inputs[slot].resolve() for slot in tool["inputs"]}
    if output.resolve() in sources.values():
        raise MediaError("output must be separate from its sources")
    if any(not source.is_file() or not 0 < source.stat().st_size <= input_limit for source in sources.values()):
        raise MediaError("source is missing, empty, or exceeds the byte limit")

    defaults = {
        name: spec["default"]
        for name, spec in tool["parameters"]["properties"].items()
        if "default" in spec
    }
    with tempfile.TemporaryDirectory(prefix=".tool-", dir=output.parent) as directory:
        staging = Path(directory) / "result"
        content_type = render(tool, sources, defaults | parameters, staging)
        if content_type not in tool["output"]["content_types"]:
            raise MediaError("output does not match the declared content type")
        if not staging.is_file() or not 0 < staging.stat().st_size < output_limit:
            raise MediaError("output was missing, empty, or reached its byte limit")
        os.replace(staging, output)
    return content_type
