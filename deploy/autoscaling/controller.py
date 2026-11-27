"""Decide at most one cell-local replica change per observation."""

import argparse
from collections.abc import Mapping
from contextlib import contextmanager, nullcontext
import fcntl
import json
import math
import os
from pathlib import Path
import re
import select
import subprocess
from tempfile import NamedTemporaryFile
import time


MAX_SAMPLE_AGE = 45
UP_WINDOW = 30
DOWN_WINDOW = 300
UP_COOLDOWN = 60
DOWN_COOLDOWN = 600
ROLES = ("api", "worker")
ADMISSION_SERVICE = re.compile(r"[A-Za-z0-9_-][A-Za-z0-9_.-]{0,127}")
ADMISSION_GUARD_SQL = b"""
BEGIN ISOLATION LEVEL READ COMMITTED READ ONLY;
SET LOCAL lock_timeout = '1500ms';
SET LOCAL statement_timeout = '3s';
SET LOCAL idle_in_transaction_session_timeout = '20s';
LOCK TABLE public.gateway_admission_control IN ACCESS SHARE MODE;
SELECT CASE WHEN pg_is_in_recovery() THEN 'unknown'
       ELSE coalesce((SELECT CASE WHEN paused THEN 'paused' ELSE 'open' END
                     FROM public.gateway_admission_control WHERE singleton), 'unknown') END;
"""


def integer(value, name, *, minimum=0):
    if type(value) is not int or value < minimum:
        raise ValueError(f"{name} must be an integer >= {minimum}")
    return value


def cpu_utilization(value):
    if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
        raise ValueError("api.cpu_fraction must be a nonnegative utilization ratio")
    return value


def validate_config(config):
    for field in ("cell", "context", "namespace"):
        if not isinstance(config[field], str) or not config[field].strip():
            raise ValueError(f"{field} must be a nonempty string")
    per_replica = integer(config["connections_per_replica"], "connections_per_replica", minimum=10)
    for role in ROLES:
        settings = config[role]
        lower = integer(settings["min"], f"{role}.min", minimum=1)
        upper = integer(settings["max"], f"{role}.max", minimum=lower)
        if upper > 1000:
            raise ValueError(f"{role}.max exceeds 1000 replicas")
        name = settings["deployment"]
        if not isinstance(name, str) or not name or name.startswith("-") or "/" in name:
            raise ValueError(f"{role}.deployment must be a deployment name")
    if config["api"]["deployment"] == config["worker"]["deployment"]:
        raise ValueError("API and worker deployments must be separate")
    if type(config["api"].get("allow_scale_in", False)) is not bool:
        raise ValueError("api.allow_scale_in must be a boolean")
    service = config["api"].get("admission_service")
    if service is not None and (not isinstance(service, str) or not ADMISSION_SERVICE.fullmatch(service)):
        raise ValueError("api.admission_service must be a libpq service name")
    minimum_connections = sum(config[role]["min"] for role in ROLES) * per_replica
    integer(config["connection_budget"], "connection_budget", minimum=minimum_connections)


def signals(snapshot, admissions_open):
    for role in ROLES:
        current = snapshot[role]
        desired = integer(current["desired"], f"{role}.desired")
        ready = integer(current["ready"], f"{role}.ready")
        version = current["deployment_version"]
        if not isinstance(version, str) or not version.strip():
            raise ValueError(f"{role}.deployment_version is missing")
        if "replica_set" in current:
            fleet = current["replica_set"]
            if not isinstance(fleet, str) or not re.fullmatch(r"[0-9a-f]{64}", fleet):
                raise ValueError(f"{role}.replica_set must be a SHA-256 digest")
        if ready != desired or not desired or current["stable"] is not True:
            raise ValueError(f"{role} deployment is starting, draining, or rolling out")

    api, worker = snapshot["api"], snapshot["worker"]
    cpu = cpu_utilization(api["cpu_fraction"])
    readers = integer(api["sse_readers"], "api.sse_readers")
    reader_slots = integer(api["sse_capacity"], "api.sse_capacity", minimum=readers)
    tasks = integer(worker["active_tasks"], "worker.active_tasks")
    task_slots = integer(worker["task_capacity"], "worker.task_capacity", minimum=tasks)
    age = integer(worker["oldest_ready_age_seconds"], "worker.oldest_ready_age_seconds")
    headroom = worker["provider_headroom"]
    if type(headroom) is not bool:
        raise ValueError("worker.provider_headroom must be a boolean from provider telemetry")

    return {
        "api": (
            cpu >= 0.70 or (reader_slots > 0 and readers / reader_slots >= 0.75),
            admissions_open and cpu <= 0.35 and readers == 0,
            "sustained API CPU or SSE occupancy",
        ),
        "worker": (
            age >= 15 and task_slots > 0 and tasks / task_slots >= 0.75 and headroom,
            False,
            "eligible queue age and busy worker slots with provider headroom",
        ),
    }


def fresh(sampled_at, now):
    return (type(sampled_at) in (int, float) and math.isfinite(sampled_at)
            and 0 <= now - sampled_at <= MAX_SAMPLE_AGE)


def decide(config, snapshot, previous, now):
    """Return an optional action and the next observation state."""
    validate_config(config)
    if previous and previous.get("cell") != config["cell"]:
        raise ValueError("controller state belongs to another cell")
    state = {
        "cell": config["cell"], "last_observed_at": None, "last_action_at": previous.get("last_action_at"),
        "last_scaled_role": previous.get("last_scaled_role"), "database_primary": False,
        "admissions_paused": None, "roles": {},
    }

    def hold(reason):
        return {"decision": "hold", "reason": reason}, state

    sampled_at = snapshot["observed_at"]
    if snapshot["cell"] != config["cell"]:
        raise ValueError("snapshot belongs to another cell")
    if not fresh(sampled_at, now):
        return hold("snapshot is stale or has an invalid timestamp")
    database = snapshot["database"]
    prior_at = previous.get("last_observed_at")
    if database.get("primary") is not True:
        state["last_observed_at"] = max(sampled_at, prior_at) if prior_at is not None else sampled_at
        return hold("autoscaling requires a sample from the cell's primary database")
    paused = database.get("admissions_paused")
    if type(paused) is not bool:
        paused = None
    if prior_at is not None and sampled_at <= prior_at:
        # A newer database read can share the same oldest contributing timestamp.
        if sampled_at == prior_at and paused is not False:
            previous = {**previous, "admissions_paused": paused}
        return {"decision": "hold", "reason": "waiting for a fresh snapshot"}, previous
    state["last_observed_at"] = sampled_at
    state["database_primary"] = True
    state["admissions_paused"] = paused
    if database["healthy"] is not True:
        return hold("database health guard is closed")
    free_connections = integer(database["additional_connections"], "database.additional_connections")
    try:
        pressure = signals(snapshot, paused is False)
    except ValueError as error:
        return hold(str(error))
    reserved = sum(snapshot[role]["desired"] for role in ROLES) * config["connections_per_replica"]
    remaining = min(free_connections, config["connection_budget"] - reserved)

    continuity = (previous.get("database_primary") is True and prior_at is not None
                  and sampled_at - prior_at <= MAX_SAMPLE_AGE)
    elapsed = now - state["last_action_at"] if state["last_action_at"] is not None else math.inf
    actions = []
    for role, (high, low, reason) in pressure.items():
        settings = config[role]
        count = snapshot[role]["desired"]
        replica_set = snapshot[role].get("replica_set")
        history = previous.get("roles", {}).get(role, {}) if continuity else {}
        if history.get("desired") != count or history.get("replica_set") != replica_set:
            history = {}
        high_since = history.get("high_since", sampled_at) if high else None
        low_since = history.get("low_since") if low and previous.get("admissions_paused") is False else None
        if high and high_since is None:
            high_since = sampled_at
        if low and low_since is None:
            low_since = sampled_at
        state["roles"][role] = {
            "desired": count, "replica_set": replica_set, "high_since": high_since, "low_since": low_since,
        }
        if high and sampled_at - high_since >= UP_WINDOW and elapsed >= UP_COOLDOWN and count < settings["max"]:
            if remaining >= config["connections_per_replica"]:
                actions.append((0, high_since, role == state["last_scaled_role"], role, count + 1, reason))
        if low and settings.get("allow_scale_in", False) and role == "api" and count > settings["min"]:
            if sampled_at - low_since >= DOWN_WINDOW and elapsed >= DOWN_COOLDOWN:
                actions.append((1, low_since, False, role, count - 1, "sustained low API CPU and no active SSE readers"))
    if not actions:
        if remaining < config["connections_per_replica"]:
            return {"decision": "hold", "reason": "database connection headroom is below one replica"}, state
        if paused is not False and config["api"].get("allow_scale_in") and snapshot["api"]["desired"] > config["api"]["min"]:
            return hold("API scale-in requires a fresh observation of open admissions")
        return {"decision": "hold", "reason": "replica bounds, pressure windows, or cooldown have not triggered a change"}, state
    _, _, _, role, replicas, reason = min(actions)
    return {
        "decision": "scale", "role": role, "deployment": config[role]["deployment"],
        "observed_at": sampled_at,
        "expected": snapshot[role]["desired"], "deployment_version": snapshot[role]["deployment_version"],
        "replicas": replicas, "reason": reason,
    }, state


@contextmanager
def admission_guard(psql, service):
    """Keep the same shared lock as an admission while the actuator sends its scale request."""
    if not service:
        yield "API scale-in requires api.admission_service"
        return
    unknown = "API scale-in could not verify open admissions on the cell writer"
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("PG") or key in {"PGSERVICEFILE", "PGPASSFILE"}}
    env["PGCONNECT_TIMEOUT"] = "3"
    try:
        child = subprocess.Popen(
            [psql, "-XqAtw", "-v", "ON_ERROR_STOP=1", "-d", f"service={service}"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env,
        )
    except OSError:
        yield unknown
        return
    with child:
        try:
            try:
                child.stdin.write(ADMISSION_GUARD_SQL)
                child.stdin.flush()
                ready, _, _ = select.select([child.stdout], [], [], 6)
                reply = os.read(child.stdout.fileno(), 64) if ready else b""
            except (OSError, ValueError):
                reply = b""
            if reply == b"open\n" and child.poll() is None:
                yield None
            else:
                yield "API scale-in held while admissions are paused" if reply == b"paused\n" else unknown
        finally:
            try:
                child.stdin.close()
            except BrokenPipeError:
                pass
            try:
                child.wait(timeout=2)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


def apply_scale(kubectl, config, decision, *, psql="psql"):
    """Return a reason when the live guard holds the decision."""
    scale_in = decision["role"] == "api" and decision["replicas"] < decision["expected"]
    gate = admission_guard(psql, config["api"].get("admission_service")) if scale_in else nullcontext()
    with gate as hold_reason:
        if hold_reason:
            return hold_reason
        if scale_in and not fresh(decision.get("observed_at"), time.time()):
            return "API scale-in observation expired before actuation"
        subprocess.run([
            kubectl, f"--context={config['context']}", f"--namespace={config['namespace']}",
            "--request-timeout=10s", "scale", f"deployment/{decision['deployment']}",
            f"--current-replicas={decision['expected']}", f"--resource-version={decision['deployment_version']}",
            f"--replicas={decision['replicas']}",
        ], check=True, timeout=15, stdout=subprocess.DEVNULL)


def read_object(path):
    with path.open() as stream:
        value = json.load(stream)
    if not isinstance(value, Mapping):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def save_state(path, state):
    with NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as output:
        staged = Path(output.name)
        try:
            json.dump(state, output)
            output.write("\n")
            output.flush()
            staged.replace(path)
        finally:
            staged.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--snapshot", type=Path, required=True)
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("--kubectl", default="kubectl")
    parser.add_argument("--psql", default="psql", help="PostgreSQL client for the API scale-in guard")
    parser.add_argument("--apply", action="store_true", help="Permit changes to the configured Kubernetes deployments")
    args = parser.parse_args()
    config = read_object(args.config)
    with args.state.with_suffix(args.state.suffix + ".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        previous = read_object(args.state) if args.state.exists() else {}
        decision, state = decide(config, read_object(args.snapshot), previous, time.time())
        applied = False
        if args.apply and decision["decision"] == "scale":
            hold_reason = apply_scale(args.kubectl, config, decision, psql=args.psql)
            if hold_reason:
                state["admissions_paused"] = None
                state["roles"]["api"]["low_since"] = None
                decision = {"decision": "hold", "reason": hold_reason}
            else:
                state["last_action_at"] = time.time()
                state["last_scaled_role"] = decision["role"]
                state["roles"].pop(decision["role"], None)
                applied = True
        save_state(args.state, state)
    print(json.dumps({**decision, "cell": config["cell"], "applied": applied}, sort_keys=True))


if __name__ == "__main__":
    main()
