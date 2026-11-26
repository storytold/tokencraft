"""TokenCraft's separate, self-hosted operations collector and console."""

import argparse
import base64
from concurrent.futures import ThreadPoolExecutor, as_completed
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import ipaddress
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
from urllib.parse import parse_qs, urlsplit

from history import HISTORY_FIELDS, History
import plugin_traffic
from sources import ID, load_config, observe_target, sample_database, token_from

ASSETS = {
    "/": ("index.html", "text/html; charset=utf-8"),
    "/console.js": ("console.js", "text/javascript; charset=utf-8"),
    "/console.css": ("console.css", "text/css; charset=utf-8"),
    "/tokencraft-mark.png": ("tokencraft-mark.png", "image/png"),
}


class Collector:
    def __init__(self, config, history, psql):
        self.config = config
        self.history = history
        self.psql = psql
        self.binding_cache = {}
        self.plugin_cache = {}
        previous, _ = history.overview()
        self.plugin_operations = {
            name: target["plugin_operations"]
            for name, target in (previous or {}).get("targets", {}).items()
            if "plugin_operations" in target
        }

    def collect(self):
        started = time.monotonic()
        now = int(time.time())
        observations = {}
        with ThreadPoolExecutor(max_workers=16) as pool:
            pending = {pool.submit(observe_target, item): item for item in self.config["targets"]}
            for future in as_completed(pending):
                target = pending[future]
                result, bindings = future.result()
                plugin = result.pop("plugin", None)
                if target["kind"] == "plugin":
                    prior = self.plugin_cache.pop(target["id"], None)
                    if plugin:
                        elapsed = now - prior[0] if prior else 0
                        before = prior[1] if prior and elapsed <= self.config["interval_seconds"] * 3 else None
                        result["plugin"] = {
                            "counters": plugin["operations"],
                            "operations": plugin_traffic.activity(plugin, before, elapsed),
                        }
                        self.plugin_cache[target["id"]] = (now, plugin)
                        self.plugin_operations[target["id"]] = sorted(plugin["operations"])
                    if target["id"] in self.plugin_operations:
                        result["plugin_operations"] = self.plugin_operations[target["id"]]
                observations[target["id"]] = result
                if bindings is not None and target["kind"] in {"worker", "combined"}:
                    self.binding_cache[target["id"]] = (now, bindings)
            futures = {}
            cells = {}
            for name, definition in self.config["cells"].items():
                targets = [item for item in self.config["targets"] if item["cell"] == name]
                workers = [item["id"] for item in targets if item["kind"] in {"worker", "combined"}]
                cached = [self.binding_cache.get(worker) for worker in workers]
                cutoff = now - min(600, self.config["interval_seconds"] * 10)
                live = [item for item in cached if item is not None and item[0] >= cutoff]
                combined = {form: sorted({key for _, binding in live for key in binding[form]})
                            for form in ("local", "connector")}
                scoped_bindings = len(live) == len(workers) and bool(workers) and all(
                    len(items) <= 512 for items in combined.values())
                cells[name] = {
                    "providers": definition["providers"],
                    "reserved_connections": definition["reserved_connections"],
                    "queue_coverage": "fresh" if scoped_bindings and all(at == now for at, _ in live)
                        else "cached" if scoped_bindings else "unknown",
                    "status": "not_configured" if not definition["pg_service"] else "unknown",
                    "scopes": {scope: {"status": "unknown"} for scope in definition["scopes"]},
                    "worker_bindings_uniform": scoped_bindings and all(
                        {form: sorted(set(binding[form])) for form in combined} == combined for _, binding in live),
                }
                if definition["pg_service"]:
                    if not scoped_bindings:
                        combined = {"local": [], "connector": []}
                    futures[name, None] = pool.submit(sample_database, self.psql, definition, targets, combined, {})
                    for scope, params in definition["scopes"].items():
                        futures[name, scope] = pool.submit(sample_database, self.psql, definition, targets, combined, params)
            for (name, scope), future in futures.items():
                cell = cells[name]
                result = cell if scope is None else cell["scopes"][scope]
                try:
                    result.update(future.result())
                except (OSError, ValueError, subprocess.TimeoutExpired):
                    result.update(status="down", reason="database_sample_unavailable")
                sample = result.get("sample")
                if sample and cell["queue_coverage"] == "unknown":
                    sample["eligible"] = None
                    sample["oldest_eligible_seconds"] = None
        snapshot = {"observed_at": now, "duration_ms": round((time.monotonic() - started) * 1000),
                    "cells": cells, "targets": observations}
        self.history.record(snapshot, evaluate(snapshot, self.config["alerts"]),
                            self.config["alerts"]["consecutive_samples"], self.config["interval_seconds"])


def evaluate(snapshot, policy):
    result = {}
    for name, probe in snapshot["targets"].items():
        state = probe["status"]
        result["probe/" + name] = (f"{name} is unavailable or degraded",
                                   (state != "up") if state != "unknown" else None)
        result["telemetry/" + name] = (f"{name} could not be observed", state == "unknown")
        if probe["kind"] == "plugin":
            activity = probe.get("plugin", {}).get("operations", {})
            for operation in plugin_traffic.OPERATIONS:
                counts = activity.get(operation, {}).get("responses")
                failed = counts["server_error"] + counts["interrupted"] + counts["throttled"] if counts else 0
                observed = failed + counts["ok"] if counts else 0
                unhealthy = (100 * failed / observed >= policy["failure_percent"]
                             if observed >= policy["minimum_outcomes"] else None)
                result[f"plugin/{name}/{operation}"] = (f"{name} {operation} is failing or throttled", unhealthy)
    for name, cell in snapshot["cells"].items():
        if cell["status"] == "not_configured":
            continue
        sample = cell.get("sample")
        result["database/" + name] = (f"{name} database metrics are unavailable", sample is None)
        result["connections/" + name] = (f"{name} database has no spare connections",
                                          sample["database_connections_available"] == 0 if sample else None)
        eligible = sample.get("oldest_eligible_seconds") if sample else None
        covered = sample is not None and cell["queue_coverage"] in {"fresh", "cached"}
        result["dispatch/" + name] = (f"{name} dispatch-ready jobs are aging",
                                       (eligible or 0) >= policy["queue_seconds"] if covered else None)
        publication = None if not sample or sample["truncated"] else any(
            (row["oldest_pending_seconds"] or 0) >= policy["publication_seconds"] for row in sample["publication"])
        result["publication/" + name] = (f"{name} has pending artifact publications", publication)
        overdue = None if not sample else any(row["overdue"] > 0 for row in sample["workflow_queues"])
        result["workflow/" + name] = (f"{name} has workflows past their deadlines", overdue)
        webhook = None if not sample else any(row["kind"] == "webhook_pending"
            and (row["oldest_seconds"] or 0) >= policy["webhook_seconds"] for row in sample["delivery_queues"])
        result["webhook/" + name] = (f"{name} has delayed webhook deliveries", webhook)
        for provider in cell["providers"]:
            row = sample["provider"].get(provider) if sample and not sample["truncated"] else None
            failed = row["failed"] + row["uncertain"] if row else 0
            outcomes = failed + row["succeeded"] if row else 0
            unhealthy = (100 * failed / outcomes >= policy["failure_percent"]
                         if outcomes >= policy["minimum_outcomes"] else None)
            result[f"provider/{name}/{provider}"] = (f"{provider} has elevated failures in {name}", unhealthy)
    return result


def metrics(snapshot, incidents, stale):
    yield "# TYPE tokencraft_monitor_up gauge"
    yield f"tokencraft_monitor_up {0 if stale else 1}"
    if stale:
        return
    yield f"tokencraft_monitor_sample_timestamp_seconds {snapshot['observed_at']}"
    for name, target in snapshot["targets"].items():
        labels = f'cell="{target["cell"]}",target="{name}",kind="{target["kind"]}"'
        state = target["status"]
        yield f'tokencraft_monitor_target_state{{{labels},state="{state}"}} 1'
        yield f'tokencraft_monitor_probe_seconds{{{labels}}} {target["latency_ms"] / 1000}'
        for key, val in target["signals"].items():
            if type(val) in (int, float):
                yield f"tokencraft_monitor_{key}{{{labels}}} {val}"
    yield from plugin_traffic.metrics(snapshot["targets"])
    for name, cell in snapshot["cells"].items():
        sample = cell.get("sample")
        if cell["status"] != "not_configured":
            yield f'tokencraft_monitor_database_sample_up{{cell="{name}"}} {int(sample is not None)}'
        if not sample:
            continue
        for key in ("database_connections_available", "oldest_eligible_seconds", "eligible"):
            val = sample[key]
            if val is not None:
                yield f'tokencraft_monitor_{key}{{cell="{name}"}} {val}'
        yield f'tokencraft_monitor_admission_sample_truncated{{cell="{name}"}} {int(sample["truncated"])}'
        for layer, label in (("provider", "provider"), ("compute", "target")):
            for identifier, values in sample[layer].items():
                for key, val in values.items():
                    if type(val) in (int, float):
                        yield f'tokencraft_monitor_{layer}_{key}{{cell="{name}",{label}="{identifier}"}} {val}'
        topics = {
            "request_queue": (sample["request_queues"], ("provider", "workload", "state")),
            "workflow_queue": (sample["workflow_queues"], ("status",)),
            "workflow_outcome": ([sample["workflow_outcomes"]], ()),
            "delivery_queue": (sample["delivery_queues"], ("kind",)),
            "publication": (sample["publication"], ("provider", "workload")),
        }
        for topic, (rows, dimensions) in topics.items():
            for row in rows:
                labels = ",".join([f'cell="{name}"', *(f'{key}="{row[key]}"' for key in dimensions)])
                for key, val in row.items():
                    if type(val) in (int, float):
                        yield f"tokencraft_monitor_{topic}_{key}{{{labels}}} {val}"
    for incident in incidents:
        if incident["resolved_at"] is None:
            rule, subject = incident["key"].split("/", 1)
            yield f'tokencraft_monitor_incident_open{{rule="{rule}",subject="{subject}"}} 1'


class Console(ThreadingHTTPServer):
    def __init__(self, address, password, history, interval):
        self.address_family = socket.AF_INET6 if ":" in address[0] else socket.AF_INET
        self.auth = b"Basic " + base64.b64encode(("operator:" + password).encode())
        self.history = history
        self.interval = interval
        self.slots = threading.BoundedSemaphore(16)
        self.assets = {path: (Path(__file__).parent.joinpath("web", name).read_bytes(), mime)
                       for path, (name, mime) in ASSETS.items()}
        super().__init__(address, Handler)

    def process_request(self, request, address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            request.settimeout(5)
            super().process_request(request, address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            self.slots.release()


class Handler(BaseHTTPRequestHandler):
    server_version = "TokenCraft-Operations"
    sys_version = ""

    def log_message(self, *_args):
        pass

    def reply(self, status, body=b"", content_type="text/plain; charset=utf-8"):
        self.send_response(status)
        if status == 401:
            self.send_header("WWW-Authenticate", 'Basic realm="TokenCraft operations", charset="UTF-8"')
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Security-Policy", "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.end_headers()
        self.wfile.write(body)

    def json_reply(self, status, body):
        self.reply(status, json.dumps(body, allow_nan=False).encode(), "application/json")

    def do_GET(self):
        authorization = self.headers.get_all("Authorization", ())
        if len(authorization) != 1 or not hmac.compare_digest(
                authorization[0].encode("latin-1"), self.server.auth):
            return self.reply(401, b"Operator authentication required")
        try:
            url = urlsplit(self.path)
            if url.path == "/api/history":
                return self.history(url.query)
        except ValueError:
            return self.reply(400, b"Invalid monitoring query")
        if url.path in self.server.assets and not url.query:
            body, content_type = self.server.assets[url.path]
            return self.reply(200, body, content_type)
        if url.path not in {"/api/overview", "/metrics", "/healthz"} or url.query:
            return self.reply(404)
        snapshot, incidents = self.server.history.overview()
        now = int(time.time())
        stale = snapshot is None or not 0 <= now - snapshot["observed_at"] <= self.server.interval * 3
        if url.path == "/metrics":
            active = [] if stale else self.server.history.active_incidents()
            return self.reply(200, ("\n".join(metrics(snapshot, active, stale)) + "\n").encode(),
                              "text/plain; version=0.0.4; charset=utf-8")
        if url.path == "/healthz":
            return self.json_reply(503 if stale else 200, {"collector_fresh": not stale})
        self.json_reply(200, {"snapshot": snapshot, "incidents": incidents,
                              "stale": stale, "interval_seconds": self.server.interval})

    def history(self, query):
        parameters = parse_qs(query, strict_parsing=True, max_num_fields=5)
        if any(len(val) != 1 for val in parameters.values()) or set(parameters) - {"metric", "id", "subject", "hours", "scope"}:
            raise ValueError()
        metric = parameters.get("metric", [""])[0]
        identifier = parameters.get("id", [""])[0]
        subject = parameters.get("subject", [""])[0]
        scope = parameters.get("scope", [""])[0]
        hours = int(parameters.get("hours", ["1"])[0])
        if (metric not in HISTORY_FIELDS or not ID.fullmatch(identifier)
                or hours not in {1, 6, 24, 168}
                or (scope and (not ID.fullmatch(scope) or HISTORY_FIELDS[metric][0] != "cells")) or
                (metric in {"request_p95", "target_p95"} and not ID.fullmatch(subject)) or
                (metric in {"plugin_p95", "plugin_throughput"} and subject not in plugin_traffic.OPERATIONS)):
            raise ValueError()
        since = int(time.time()) - hours * 3600
        bucket_size = max(self.server.interval, hours * 5)
        self.json_reply(200, self.server.history.series(metric, since, bucket_size, identifier, subject, scope))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--state-dir", type=Path)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=9417)
    parser.add_argument("--check-config", action="store_true")
    args = parser.parse_args()
    try:
        config = load_config(args.config)
        if args.check_config:
            print("Monitoring configuration is valid")
            return
        if not ipaddress.ip_address(args.host).is_loopback or not 1 <= args.port <= 65535:
            raise ValueError("bind the console to a loopback address and use a private HTTPS proxy for remote access")
        if args.state_dir is None:
            raise ValueError("set --state-dir to a private directory")
        password = token_from("TOKENCRAFT_MONITOR_PASSWORD")
        psql = shutil.which("psql")
        if not psql and any(cell["pg_service"] for cell in config["cells"].values()):
            raise ValueError("psql is required for configured PostgreSQL sources")
        os.umask(0o077)
        args.state_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        if args.state_dir.is_symlink() or args.state_dir.stat().st_mode & 0o077:
            raise ValueError("monitoring state must be in a private directory owned by the service account")
        history = History(args.state_dir / "monitor.sqlite", config["retention_days"] * 86400)
        interval = config["interval_seconds"]
        server = Console((args.host, args.port), password, history, interval)
    except (ValueError, OSError) as error:
        parser.error(str(error))
    collector = Collector(config, history, psql)
    stop = threading.Event()

    def collect():
        while not stop.is_set():
            started = time.monotonic()
            try:
                collector.collect()
            except Exception as error:
                # Source errors can contain secrets or private origins.
                print("Collection failed", type(error).__name__, file=sys.stderr, flush=True)
            stop.wait(max(1, interval - (time.monotonic() - started)))

    sampler = threading.Thread(target=collect, name="monitor-sampler")
    sampler.start()
    listener = threading.Thread(target=server.serve_forever, name="monitor-console")
    listener.start()
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, lambda *_args: stop.set())
    print(f"TokenCraft Operations is listening on port {args.port}", flush=True)
    try:
        stop.wait()
    finally:
        server.shutdown()
        listener.join()
        server.server_close()
        sampler.join()


if __name__ == "__main__":
    main()
