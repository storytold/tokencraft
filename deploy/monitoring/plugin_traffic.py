"""Versioned private connector samples and interval performance."""

import json
import math
import re

OPERATIONS = ("submit", "status", "artifact", "lookup", "chat")
OUTCOMES = ("ok", "throttled", "client_error", "server_error", "interrupted")
BOUNDS_MS = (25, 50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000, 60000, 120000)
SAFE_INTEGER = 2 ** 53 - 1
BOOT_ID = re.compile(r"[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}")


def nonnegative_int(value):
    return type(value) is int and 0 <= value <= SAFE_INTEGER


def parse_sample(payload):
    data = json.loads(payload)
    if (not isinstance(data, dict) or set(data) != {"version", "boot_id", "operations"}
            or type(data["version"]) is not int or data["version"] != 1
            or not isinstance(data["boot_id"], str) or not BOOT_ID.fullmatch(data["boot_id"])):
        raise ValueError("invalid plugin monitoring envelope")
    records = data["operations"]
    if not isinstance(records, dict) or not records or set(records) - set(OPERATIONS):
        raise ValueError("unexpected plugin operation")
    for record in records.values():
        if (not isinstance(record, dict) or set(record) != {"in_flight", "responses", "latency_ms"}
                or not nonnegative_int(record["in_flight"])):
            raise ValueError("invalid plugin operation statistics")
        responses = record["responses"]
        if (not isinstance(responses, dict) or set(responses) != set(OUTCOMES)
                or any(not nonnegative_int(count) for count in responses.values())):
            raise ValueError("invalid plugin response counts")
        latency = record["latency_ms"]
        if not isinstance(latency, dict) or set(latency) != {"buckets", "sum"}:
            raise ValueError("invalid plugin latency statistics")
        buckets = latency["buckets"]
        total = latency["sum"]
        if (not isinstance(buckets, list) or len(buckets) != len(BOUNDS_MS) + 1
                or any(not nonnegative_int(count) for count in buckets)
                or sum(buckets) != responses["ok"] or type(total) not in (float, int)
                or not 0 <= total <= SAFE_INTEGER or (not responses["ok"] and total != 0)):
            raise ValueError("invalid plugin latency buckets")
    return data


def activity(current, previous, duration):
    """Return fresh deltas for a process with uninterrupted counters."""
    if previous is None or not 0 < duration or previous["boot_id"] != current["boot_id"]:
        return {}
    result = {}
    for name, now in current["operations"].items():
        prior = previous["operations"].get(name)
        if prior is None:
            continue
        responses = {outcome: now["responses"][outcome] - prior["responses"][outcome] for outcome in OUTCOMES}
        buckets = [new - old for new, old in zip(now["latency_ms"]["buckets"], prior["latency_ms"]["buckets"])]
        if min(responses.values()) < 0 or min(buckets) < 0 or now["latency_ms"]["sum"] < prior["latency_ms"]["sum"]:
            continue
        count = sum(responses.values())
        rank = math.ceil(responses["ok"] * 0.95)
        samples = 0
        endpoint = None
        if rank:
            for bound, entries in zip(BOUNDS_MS, buckets):
                samples += entries
                if samples >= rank:
                    endpoint = bound
                    break
        result[name] = {"seconds": duration, "responses": responses, "responses_per_second": count / duration,
                        "p95_ms": endpoint, "p95_above_max": bool(rank and endpoint is None)}
    return result


def metrics(targets):
    samples = [
        (f'cell="{target["cell"]}",target="{name}",provider="{target["provider"]}",operation="{operation}"', sample)
        for name, target in targets.items() if "plugin" in target
        for operation, sample in target["plugin"]["counters"].items()
    ]
    if not samples:
        return
    yield "# TYPE tokencraft_plugin_in_flight gauge"
    for dimensions, sample in samples:
        yield f'tokencraft_plugin_in_flight{{{dimensions}}} {sample["in_flight"]}'
    yield "# TYPE tokencraft_plugin_responses_total counter"
    for dimensions, sample in samples:
        for outcome, count in sample["responses"].items():
            yield f'tokencraft_plugin_responses_total{{{dimensions},outcome="{outcome}"}} {count}'
    yield "# TYPE tokencraft_plugin_success_duration_seconds histogram"
    for dimensions, sample in samples:
        elapsed = sample["latency_ms"]
        cumulative = 0
        for bound, count in zip(BOUNDS_MS, elapsed["buckets"]):
            cumulative += count
            yield f'tokencraft_plugin_success_duration_seconds_bucket{{{dimensions},le="{bound / 1000:g}"}} {cumulative}'
        count = sample["responses"]["ok"]
        yield f'tokencraft_plugin_success_duration_seconds_bucket{{{dimensions},le="+Inf"}} {count}'
        yield f'tokencraft_plugin_success_duration_seconds_count{{{dimensions}}} {count}'
        yield f'tokencraft_plugin_success_duration_seconds_sum{{{dimensions}}} {elapsed["sum"] / 1000}'
