"""Feed a verified operations sample to the existing autoscaling controller."""

import argparse
from hashlib import sha256
import json
import math
from pathlib import Path
import time

from history import History
from sources import INSTANCE_ID


def snapshot(observation, source, now, max_age=45):
    name = source["cell"]
    sampled_at = observation["observed_at"]
    external_at = source["observed_at"]
    if (type(external_at) not in (int, float) or not math.isfinite(external_at)
            or not 0 <= now - min(sampled_at, external_at) <= max_age
            or max(sampled_at, external_at) > now):
        raise ValueError("operator or deployment observations are stale")
    cell = observation["cells"][name]
    sample = cell.get("sample")
    if sample is None or cell["queue_coverage"] != "fresh" or not cell["worker_bindings_uniform"]:
        raise ValueError("cell needs a fresh database sample and matching worker bindings")
    if source["reserve_connections"] != cell["reserved_connections"]:
        raise ValueError("monitor and autoscaling database connection reserves must match")
    result = {
        "cell": name, "observed_at": min(sampled_at, external_at),
        "database": {"healthy": source["database_healthy"] is True,
                     "additional_connections": sample["database_connections_available"]},
    }
    seen_instances = set()
    for role, kind, columns in (
        ("api", "gateway", ("sse_readers", "sse_capacity")),
        ("worker", "worker", ("provider_tasks", "provider_capacity")),
    ):
        external = source[role]
        expected = {target["id"] for target in observation["targets"].values()
                    if target["cell"] == name and target["kind"] == kind}
        observed = external["replicas"]
        if not expected or len(observed) != len(expected) or set(observed) != expected:
            raise ValueError("deployment inventory differs from the monitor's split replicas")
        probes = [observation["targets"][identifier] for identifier in observed]
        if any(probe["status"] != "up" or any(probe["signals"].get(col) is None for col in columns)
               for probe in probes):
            raise ValueError("a replica is unhealthy or has incomplete capacity telemetry")
        instances = [probe["signals"].get("instance_id") for probe in probes]
        if any(not isinstance(instance, str) or not INSTANCE_ID.fullmatch(instance) for instance in instances):
            raise ValueError("capacity probes must report a valid instance ID")
        if len(set(instances)) != len(instances) or seen_instances.intersection(instances):
            raise ValueError("multiple monitor targets reached the same replica process")
        seen_instances.update(instances)
        result[role] = {
            "replica_set": sha256("".join(sorted(instances)).encode("ascii")).hexdigest(),
            "ready": len(probes), "desired": external["desired"], "stable": external["stable"] is True,
            "deployment_version": external["deployment_version"],
        }
        first, second = (sum(probe["signals"][key] for probe in probes) for key in columns)
        if role == "api":
            result[role].update(cpu_fraction=external["cpu_fraction"], sse_readers=first, sse_capacity=second)
        else:
            result[role].update(
                active_tasks=first, task_capacity=second,
                oldest_ready_age_seconds=math.floor(sample["oldest_eligible_seconds"] or 0),
                eligible_sample=sample["eligible"], provider_headroom=source.get("provider_headroom") is True,
            )
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-dir", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    args = parser.parse_args()
    try:
        path = args.state_dir / "monitor.sqlite"
        if not path.is_file():
            raise ValueError("monitor state is unavailable")
        state = History(path)
        observation, _ = state.overview()
        if observation is None:
            raise ValueError("wait for a successful operator sample")
        data = snapshot(observation, json.loads(args.source.read_bytes()), time.time())
    except (KeyError, TypeError, ValueError, OSError) as error:
        parser.error(str(error))
    print(json.dumps(data, allow_nan=False))


if __name__ == "__main__":
    main()
