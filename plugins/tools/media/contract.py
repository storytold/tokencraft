"""Version 1 media tool descriptors and request validation."""
from __future__ import annotations

import json
import math
from pathlib import Path


class MediaError(ValueError):
    """An invalid request, unsupported source, or failed media operation."""


MANIFEST = json.loads(Path(__file__).with_name("manifest.json").read_bytes())
TOOLS = {tool["id"]: tool for tool in MANIFEST["tools"]}


def validate_request(tool_id: str, revision: int, slots: object, parameters: object) -> dict:
    if not isinstance(tool_id, str) or tool_id not in TOOLS:
        raise MediaError("unknown tool")
    tool = TOOLS[tool_id]
    if type(revision) is not int or revision != tool["revision"]:
        raise MediaError("unsupported tool revision")
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
