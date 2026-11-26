"""Read-only observations from gateway replicas, provider plugins, and PostgreSQL."""

from hashlib import sha256
import ipaddress
from http.client import HTTPException
import json
import math
import os
import re
import subprocess
import time
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener

from plugin_traffic import parse_sample as plugin_sample

ID = re.compile(r"[a-zA-Z0-9][a-zA-Z0-9_.-]{0,63}")
SECRET = re.compile(r"[A-Za-z0-9_-]{32,128}")
DIGEST = re.compile(r"[0-9a-f]{64}")
KINDS = {"gateway", "worker", "combined", "plugin", "compute", "upstream", "gpu"}
REPLICA_ROLES = {"gateway": "api", "worker": "worker", "combined": "combined"}
CHECKS = {"database", "migrations", "worker", "provider_worker", "artifact_storage"}
CHECK_STATES = {"checking", "ready", "unavailable", "not_configured", "external"}
GPU_FIELDS = {
    "DCGM_FI_DEV_GPU_UTIL": ("gpu_utilization_percent", max, 0, 100),
    "DCGM_FI_DEV_GPU_TEMP": ("gpu_temperature_celsius", max, 0, 150),
    "DCGM_FI_DEV_FB_USED": ("gpu_memory_used_mib", sum, 0, 10_000_000),
    "DCGM_FI_DEV_FB_FREE": ("gpu_memory_free_mib", sum, 0, 10_000_000),
}
GPU_LINE = re.compile(r"([A-Z0-9_]+)\{([^}]*)\}\s+(\S+)(?:\s+\d+)?\s*")
GPU_UUID = re.compile(r'(?:^|,)UUID="([A-Za-z0-9_.-]+)"')
PSQL_SAMPLE = """
BEGIN READ ONLY;
SET LOCAL statement_timeout = '2500ms';
SELECT tokencraft_monitor.sample(:'providers'::jsonb, :'targets'::jsonb, :'bindings'::jsonb,
       :'reserve'::integer, nullif(:'account',''), nullif(:'project',''));
COMMIT;
"""


def require_id(value, label):
    if not isinstance(value, str) or not ID.fullmatch(value):
        raise ValueError(f"invalid {label}")
    return value


def require_int(value, label, minimum, maximum):
    if type(value) is not int or not minimum <= value <= maximum:
        raise ValueError(f"invalid {label}")
    return value


def fields(value, required, optional=()):
    if not isinstance(value, dict) or not set(required) <= value.keys() or value.keys() - set(required) - set(optional):
        raise ValueError("unexpected or missing configuration fields")


def token_from(name):
    require_id(name, "secret variable")
    token = os.environ.get(name, "")
    if not SECRET.fullmatch(token):
        raise ValueError("set the configured monitoring secret to a strong URL-safe value")
    return token


def loopback(host):
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def probe_url(value):
    if not isinstance(value, str) or not value.isascii() or any(char.isspace() for char in value):
        raise ValueError("invalid probe URL")
    url = urlsplit(value)
    if (not url.hostname or not url.path or url.username is not None or url.password is not None
            or url.query or url.fragment or url.port == 0 or "\\" in value):
        raise ValueError("probe URL requires a host and path and cannot contain credentials or query data")
    if url.scheme != "https" and not (url.scheme == "http" and loopback(url.hostname)):
        raise ValueError("probes require verified HTTPS or literal loopback HTTP")
    return url


def expected_model(value):
    if not isinstance(value, str) or not 1 <= len(value) <= 160 or not value.isprintable():
        raise ValueError("model listings require a valid model identifier")
    return value


def profile_targets(directory, cell_id, providers, profile):
    fields(profile, {"catalog", "bindings"}, {"probes"})
    documents = {}
    for key, collection in (("catalog", "offers"), ("bindings", "bindings")):
        filename = profile[key]
        if not isinstance(filename, str) or not filename.strip():
            raise ValueError(f"compute profile needs a {key} path")
        data = json.loads((directory / filename).read_bytes())
        if (not isinstance(data, dict) or type(data.get("version")) is not int or data["version"] != 1
                or not isinstance(data.get(collection), list)
                or any(not isinstance(item, dict) for item in data[collection])):
            raise ValueError(f"compute profile needs a version 1 {key} document")
        documents[key] = data
    if documents["bindings"].get("cell") != cell_id:
        raise ValueError("compute profile home cell must match the monitor cell ID")
    stacks = {}
    for binding in documents["bindings"]["bindings"]:
        name = require_id(binding.get("name"), "compute binding name")
        if name in stacks:
            raise ValueError("duplicate binding names in compute profile")
        stacks[name] = binding.get("targets")

    overrides = {}
    probes = profile.get("probes", [])
    if not isinstance(probes, list) or len(probes) > 64:
        raise ValueError("compute probe overrides must be a list of up to 64 entries")
    for probe in probes:
        fields(probe, {"provider", "stack", "target"}, {"id", "url", "token_env", "verify_models"})
        key = tuple(require_id(probe[field], field) for field in ("provider", "stack", "target"))
        if key in overrides:
            raise ValueError("duplicate compute probe override")
        overrides[key] = probe

    models = {}
    for offer in documents["catalog"]["offers"]:
        provider = offer.get("provider")
        if offer.get("adapter") != "openai_compatible_chat" or provider not in providers:
            continue
        name = require_id(offer.get("local_binding"), "chat offer binding")
        if name not in stacks:
            raise ValueError("catalog and compute profile disagree about the chat bindings")
        if stacks[name] is not None:
            models.setdefault((provider, name), set()).add(expected_model(offer.get("upstream_model")))
    targets = []
    for (provider, name), offered in sorted(models.items()):
        stack = stacks[name]
        if (not isinstance(stack, list) or not 1 <= len(stack) <= 4
                or any(not isinstance(item, dict) or not isinstance(item.get("location"), dict) for item in stack)):
            raise ValueError("compute profile requires one to four valid targets per stack")
        for item in stack:
            target_id = require_id(item.get("id"), "compute target")
            override = overrides.pop((provider, name, target_id), {})
            endpoint = probe_url(item.get("endpoint"))
            if endpoint.path != "/v1/chat/completions":
                raise ValueError("compute profile endpoints must use /v1/chat/completions")
            url = override.get("url", endpoint._replace(path="/v1/models").geturl())
            verify_models = override.get("verify_models", probe_url(url).path == "/v1/models")
            if type(verify_models) is not bool:
                raise ValueError("verify_models must be true or false")
            fingerprint = sha256(json.dumps([cell_id, provider, name, target_id]).encode()).hexdigest()[:20]
            generated = {
                "id": override.get("id", f"{name[:18]}-{target_id[:18]}-{fingerprint}"),
                "kind": "compute", "cell": cell_id, "provider": provider, "stack": name, "target": target_id,
                "url": url, "cloud": item["location"].get("cloud"), "region": item["location"].get("region"),
                "zone": item.get("zone"),
            }
            if verify_models:
                generated["expected_models"] = sorted(offered)
            if "token_env" in override:
                generated["token_env"] = override["token_env"]
            targets.append(generated)
    if overrides:
        raise ValueError("compute probe override does not match a catalog provider, stack and target")
    if not targets:
        raise ValueError("compute profile found no stack targets for this cell's catalog providers")
    return targets


def load_config(path):
    path = Path(path)
    data = json.loads(path.read_bytes())
    fields(data, {"version", "cells", "targets"}, {"interval_seconds", "retention_days", "alerts"})
    require_int(data["version"], "version", 1, 1)
    interval = require_int(data.get("interval_seconds", 30), "interval_seconds", 15, 300)
    retention = require_int(data.get("retention_days", 7), "retention_days", 1, 30)
    alerts = data.get("alerts", {})
    fields(alerts, set(), {"consecutive_samples", "queue_seconds", "publication_seconds", "webhook_seconds",
                           "failure_percent", "minimum_outcomes"})
    policy = {
        "consecutive_samples": require_int(alerts.get("consecutive_samples", 3), "consecutive_samples", 2, 10),
        "queue_seconds": require_int(alerts.get("queue_seconds", 60), "queue_seconds", 5, 899),
        "publication_seconds": require_int(alerts.get("publication_seconds", 120), "publication_seconds", 5, 900),
        "webhook_seconds": require_int(alerts.get("webhook_seconds", 300), "webhook_seconds", 5, 86400),
        "failure_percent": require_int(alerts.get("failure_percent", 25), "failure_percent", 1, 100),
        "minimum_outcomes": require_int(alerts.get("minimum_outcomes", 10), "minimum_outcomes", 2, 1000),
    }
    cells = {}
    imported_targets = []
    if not isinstance(data["cells"], list) or not 1 <= len(data["cells"]) <= 16:
        raise ValueError("configure 1 to 16 cells")
    for cell in data["cells"]:
        fields(cell, {"id", "providers"}, {"pg_service", "reserved_connections", "scopes", "compute_profile"})
        cell_id = require_id(cell["id"], "cell id")
        if cell_id in cells or not isinstance(cell["providers"], list) or not 1 <= len(cell["providers"]) <= 64:
            raise ValueError("configure unique cells with 1 to 64 providers each")
        providers = [require_id(provider, "provider") for provider in cell["providers"]]
        if len(set(providers)) != len(providers):
            raise ValueError("provider ids must be unique in each cell")
        service = cell.get("pg_service")
        if service is not None:
            require_id(service, "PostgreSQL service name")
        scopes = {}
        records = cell.get("scopes", [])
        if not isinstance(records, list) or len(records) > 4 or (records and service is None):
            raise ValueError("configure up to four detail scopes per PostgreSQL source")
        for scope in records:
            fields(scope, {"id", "account"}, {"project"})
            name = require_id(scope["id"], "scope id")
            if name == "cell" or name in scopes:
                raise ValueError("scope ids must be unique and may not be named cell")
            account = require_id(scope["account"], "account id")
            project = require_id(scope["project"], "project id") if "project" in scope else ""
            scopes[name] = {"account": account, "project": project}
        cells[cell_id] = {"providers": providers, "pg_service": service, "scopes": scopes,
                          "reserved_connections": require_int(cell.get("reserved_connections", 10),
                                                              "reserved_connections", 0, 10000)}
        if "compute_profile" in cell:
            imported_targets.extend(profile_targets(path.parent, cell_id, providers, cell["compute_profile"]))
    if sum(len(cell["scopes"]) for cell in cells.values()) + len(cells) > 16:
        raise ValueError("configure up to sixteen aggregate and detail samplers per collector")
    targets = []
    if not isinstance(data["targets"], list) or not 1 <= len(data["targets"]) + len(imported_targets) <= 64:
        raise ValueError("configure 1 to 64 probe targets")
    ids = set()
    bindings = set()
    for item in [*data["targets"], *imported_targets]:
        fields(item, {"id", "cell", "kind", "url"},
               {"provider", "token_env", "stack", "target", "gpu_for", "expected_model", "expected_models",
                "cloud", "region", "zone"})
        target = dict(item)
        target_id = require_id(target["id"], "target id")
        cell = cells.get(require_id(target["cell"], "target cell"))
        kind = require_id(target["kind"], "target kind")
        if target_id in ids or cell is None or kind not in KINDS:
            raise ValueError("invalid target id, cell, or kind")
        ids.add(target_id)
        destination = probe_url(target["url"])
        provider = target.get("provider")
        if kind in {"plugin", "compute", "upstream"}:
            if provider not in cell["providers"]:
                raise ValueError("provider target requires a configured provider")
        elif provider is not None:
            raise ValueError("provider is allowed for plugin, upstream and compute targets")
        if "expected_model" in target or "expected_models" in target:
            if kind not in {"compute", "upstream"} or {"expected_model", "expected_models"} <= target.keys():
                raise ValueError("configure either expected_model or expected_models for compute or upstream")
            models = target.get("expected_models", [target.get("expected_model")])
            if not isinstance(models, list) or not 1 <= len(models) <= 64:
                raise ValueError("model listings require one to 64 expected models")
            target["expected_models"] = [expected_model(model) for model in models]
        if kind == "compute":
            stack = require_id(target.get("stack"), "compute stack")
            name = require_id(target.get("target"), "compute target")
            binding = (target["cell"], provider, stack, name)
            if binding in bindings:
                raise ValueError("duplicate compute target mapping")
            bindings.add(binding)
        elif "stack" in target or "target" in target:
            raise ValueError("stack and target require a compute probe")
        for label in ("cloud", "region", "zone"):
            if label in target:
                require_id(target[label], label)
        if kind == "gpu":
            require_id(target.get("gpu_for"), "GPU target")
        elif "gpu_for" in target:
            raise ValueError("gpu_for requires a GPU probe")
        if kind == "plugin" and destination.path != "/internal/monitoring":
            raise ValueError("plugin health uses /internal/monitoring")
        if kind in REPLICA_ROLES and destination.path != "/internal/capacity":
            raise ValueError("replica health uses /internal/capacity")
        if kind == "gpu" and destination.path != "/metrics":
            raise ValueError("GPU exporter uses /metrics")
        if kind in {"plugin", *REPLICA_ROLES} and not target.get("token_env"):
            raise ValueError("plugin and capacity probes require a dedicated token")
        target["token"] = token_from(target.pop("token_env")) if "token_env" in target else None
        if kind in REPLICA_ROLES and not DIGEST.fullmatch(target["token"]):
            raise ValueError("capacity probe requires the 32-byte hex token")
        targets.append(target)
    by_id = {target["id"]: target for target in targets}
    for gpu in (target for target in targets if target["kind"] == "gpu"):
        compute = by_id.get(gpu["gpu_for"])
        if not compute or compute["kind"] != "compute" or compute["cell"] != gpu["cell"]:
            raise ValueError("GPU exporter must belong to a compute target in its cell")
    return {"cells": cells, "targets": targets, "interval_seconds": interval, "retention_days": retention, "alerts": policy}


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None


def read_probe(url, token, max_bytes):
    headers = {"User-Agent": "TokenCraft-Operations/1", "Accept-Encoding": "identity"}
    if token:
        headers["Authorization"] = "Bearer " + token
    if urlsplit(url).path == "/internal/monitoring":
        headers["Accept"] = "application/json"
    # Honor no ambient proxy settings. Never send the token to a redirect.
    opener = build_opener(ProxyHandler({}), NoRedirect())
    with opener.open(Request(url, headers=headers), timeout=3) as response:
        if not max_bytes:
            return response.status, b""
        body = bytearray()
        deadline = time.monotonic() + 3
        while len(body) <= max_bytes:
            if time.monotonic() >= deadline:
                raise TimeoutError()
            chunk = response.read1(min(8192, max_bytes + 1 - len(body)))
            if not chunk:
                break
            body.extend(chunk)
        if len(body) > max_bytes:
            raise ValueError("probe response too large")
        return response.status, body


def replica_sample(payload, kind):
    value = json.loads(payload)
    if not isinstance(value, dict) or value.get("role") != REPLICA_ROLES[kind] or type(value.get("ready")) is not bool:
        raise ValueError("unexpected replica role or readiness")
    result = {}
    for key in ("sse_readers", "sse_capacity", "provider_tasks", "provider_capacity"):
        result[key] = None if value.get(key) is None else require_int(value[key], key, 0, 65536)
    checks = value.get("checks", {})
    if not isinstance(checks, dict):
        raise ValueError("invalid replica checks")
    result["checks"] = {key: check for key, check in checks.items()
                        if key in CHECKS and isinstance(check, str) and check in CHECK_STATES}
    bindings = {}
    for form in ("local", "connector"):
        values = value.get(form + "_bindings", [])
        if (not isinstance(values, list) or len(values) > 512
                or any(not isinstance(digest, str) or not DIGEST.fullmatch(digest) for digest in values)):
            raise ValueError("invalid binding digests")
        bindings[form] = values
    return value["ready"], result, bindings


def gpu_sample(payload):
    samples = {field: {} for field in GPU_FIELDS}
    for line in payload.decode().splitlines():
        match = GPU_LINE.fullmatch(line)
        if not match or match[1] not in GPU_FIELDS:
            continue
        field, labels, raw = match.groups()
        uuid = GPU_UUID.search(labels)
        if uuid is None or 'GPU_I_ID="' in labels or 'GPU_I_PROFILE="' in labels:
            continue
        number = float(raw)
        _, _, low, high = GPU_FIELDS[field]
        if math.isfinite(number) and low <= number <= high:
            samples[field][uuid[1]] = number
    result = {}
    for field, (name, reducer, _, _) in GPU_FIELDS.items():
        if samples[field]:
            result[name] = reducer(samples[field].values())
    if not result:
        raise ValueError("no supported GPU series")
    result["gpu_count"] = len(set().union(*(values.keys() for values in samples.values())))
    return result


def observe_target(target, read=read_probe):
    started = time.monotonic()
    kind = target["kind"]
    labels = ("id", "cell", "kind", "provider", "stack", "target", "gpu_for", "cloud", "region", "zone")
    result = {key: target[key] for key in labels if key in target}
    state, reason, signals, bindings = "up", None, {}, None
    try:
        expected = target.get("expected_models", [target["expected_model"]] if "expected_model" in target else [])
        max_bytes = 262144 if kind == "gpu" else 65536 if kind in REPLICA_ROLES or expected else 0
        if kind == "plugin":
            max_bytes = 8192
        status, payload = read(target["url"], target["token"], max_bytes)
        if kind == "plugin":
            if status == 200:
                result["plugin"] = plugin_sample(payload)
            elif status != 204:
                state, reason = "unknown", "plugin_contract"
        elif kind in REPLICA_ROLES:
            ready, signals, bindings = replica_sample(payload, kind)
            if not ready:
                state, reason = "down", "not_ready"
            elif any(check == "unavailable" for check in signals["checks"].values()):
                state, reason = "degraded", "dependency_unavailable"
        elif kind == "gpu":
            signals = gpu_sample(payload)
        elif expected:
            listing = json.loads(payload)
            rows = listing.get("data") if isinstance(listing, dict) else None
            if not isinstance(rows, list):
                raise ValueError("invalid model listing")
            advertised = {row["id"] for row in rows if isinstance(row, dict) and isinstance(row.get("id"), str)}
            missing = len(set(expected) - advertised)
            signals = {"expected_models": len(set(expected)), "missing_models": missing}
            if missing:
                state, reason = "degraded", "model_unavailable"
    except HTTPError as error:
        with error:
            status = error.code
        if status == 429:
            state, reason = "degraded", "rate_limited"
        elif status >= 500:
            state, reason = "down", "http_error"
        else:
            state, reason = "unknown", "unexpected_http_status"
        signals = {"http_status": status}
    except (TimeoutError, OSError, URLError):
        state, reason = "down", "unreachable"
    except (ValueError, TypeError, HTTPException):
        state, reason = "unknown", "invalid_response"
    result.update(status=state, reason=reason, latency_ms=round((time.monotonic() - started) * 1000), signals=signals)
    return result, bindings


def sample_database(psql, cell, targets, bindings, scope):
    settings = {
        "providers": json.dumps(cell["providers"]),
        "targets": json.dumps([{key: target[key] for key in ("id", "cell", "provider", "stack", "target")}
                               for target in targets if target["kind"] == "compute"]),
        "bindings": json.dumps(bindings),
        "reserve": str(cell["reserved_connections"]),
        "account": scope.get("account", ""),
        "project": scope.get("project", ""),
    }
    cmd = [psql, "-XqAtw", "-d", "service=" + cell["pg_service"], "-v", "ON_ERROR_STOP=1"]
    for name, value in settings.items():
        cmd.extend(("-v", f"{name}={value}"))
    started = time.monotonic()
    libpq_environment = {key: value for key, value in os.environ.items()
                          if key.startswith("PG") or key in {"PATH", "LANG", "LC_ALL"}}
    result = subprocess.run(cmd, input=PSQL_SAMPLE, capture_output=True, text=True, timeout=5, env=libpq_environment)
    if result.returncode or len(result.stdout) > 524288:
        raise ValueError("database sample unavailable")
    sample = json.loads(result.stdout)
    if not isinstance(sample, dict) or sample.get("window_seconds") != 900:
        raise ValueError("unexpected database sampler response")
    return {"status": "up", "latency_ms": round((time.monotonic() - started) * 1000), "sample": sample}
