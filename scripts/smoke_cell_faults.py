"""Exercise a mixed TokenCraft cell while provider and artifact I/O are stalled."""

import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack, contextmanager
from datetime import datetime
from hashlib import sha256
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from math import ceil
from pathlib import Path
import re
from secrets import token_hex
import subprocess
from tempfile import TemporaryDirectory
from threading import Event, Lock, Thread
from time import monotonic, sleep
from urllib.parse import parse_qsl, quote, unquote, urlsplit

from postgres_tools import postgres_bin
from smoke_connector_recovery import square_png, read_image, wait_for_status
from smoke_workflow_contract import HOST, ROOT, call, loopback_port, provision, ready, rehearse, service

FORWARDED = {"X-Forwarded-For": "192.0.2.42"}
PIXELS = square_png((35, 95, 210))


@contextmanager
def serve(server):
    with server:
        thread = Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        thread.start()
        try:
            yield server
        finally:
            server.shutdown()
            thread.join(timeout=2)


class BucketHandler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, status, data=b""):
        try:
            self.send_response(status)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def signed_path(self):
        request = urlsplit(self.path)
        pairs = parse_qsl(request.query, keep_blank_values=True)
        params = dict(pairs)
        date = params.get("X-Amz-Date", "")
        scope = date[:8] + "/us-east-1/s3/aws4_request"
        expected_headers = "host;if-none-match" if self.command == "PUT" else "host"
        if (not request.path.startswith("/fixture-bucket/") or len(params) != len(pairs)
                or len(date) != 16 or params.get("X-Amz-Algorithm") != "AWS4-HMAC-SHA256"
                or params.get("X-Amz-Credential") != f"fixture-access/{scope}"
                or params.get("X-Amz-Expires") != "60" or params.get("X-Amz-SignedHeaders") != expected_headers):
            return None
        encoded = "&".join(f"{quote(key, safe='-._~')}={quote(value, safe='-._~')}"
                           for key, value in sorted(pairs) if key != "X-Amz-Signature")
        headers = "".join(f"{name}:{self.headers.get(name, '').strip()}\n" for name in expected_headers.split(";"))
        canonical = "\n".join((self.command, request.path, encoded, headers, expected_headers, "UNSIGNED-PAYLOAD"))
        message = "\n".join(("AWS4-HMAC-SHA256", date, scope, sha256(canonical.encode()).hexdigest()))
        key = ("AWS4" + self.server.secret).encode()
        for part in scope.split("/"):
            key = hmac.digest(key, part.encode(), "sha256")
        expected = hmac.new(key, message.encode(), "sha256").hexdigest()
        if hmac.compare_digest(expected, params.get("X-Amz-Signature", "")):
            return request.path
        return None

    def do_PUT(self):
        path = self.signed_path()
        length = int(self.headers.get("Content-Length", "0"))
        if not path or self.headers.get("If-None-Match") != "*" or not 0 < length <= 1024 * 1024:
            self.reply(403)
            return
        data = self.rfile.read(length)
        if not path.endswith("/readiness.tcm") and self.server.hold.is_set():
            with self.server.lock:
                self.server.blocked.add(path)
            try:
                if not self.server.release.wait(18):
                    self.close_connection = True
                    return
            finally:
                with self.server.lock:
                    self.server.blocked.discard(path)
        with self.server.lock:
            exists = path in self.server.objects
            if not exists:
                self.server.objects[path] = data
        self.reply(412 if exists else 200)

    def do_GET(self):
        path = self.signed_path()
        if not path:
            self.reply(403)
            return
        with self.server.lock:
            data = self.server.objects.get(path)
        self.reply(404 if data is None else 200, data or b"")

    def do_DELETE(self):
        path = self.signed_path()
        if not path:
            self.reply(403)
            return
        with self.server.lock:
            self.server.objects.pop(path, None)
        self.reply(204)


class HeldBucket(ThreadingHTTPServer):
    def __init__(self):
        super().__init__((HOST, 0), BucketHandler)
        self.secret = token_hex(32)
        self.lock = Lock()
        self.hold, self.release = Event(), Event()
        self.blocked, self.objects = set(), {}

    def stalled_uploads(self):
        with self.lock:
            return len(self.blocked)


class ConnectorHandler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, status, body, content_type="application/json"):
        data = json.dumps(body).encode() if content_type == "application/json" else body
        try:
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def authorized(self):
        return self.headers.get("Authorization") == "Bearer " + self.server.token

    def do_POST(self):
        if self.path != "/v1/jobs" or not self.authorized():
            self.reply(404, {})
            return
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        request = payload["request_id"]
        assert self.headers["Idempotency-Key"] == request
        with self.server.lock:
            self.server.jobs[request] += 1
        if self.server.hold:
            self.server.started.set()
            self.server.release.wait(18)
        self.reply(200, {"version": 1, "request_id": request, "outcome": {"status": "accepted", "handle": request}})

    def do_GET(self):
        if not self.authorized():
            self.reply(401, {})
            return
        chunks = self.path.strip("/").split("/")
        request = chunks[-1] if len(chunks) == 4 else chunks[2] if len(chunks) >= 3 else None
        with self.server.lock:
            known = request in self.server.jobs
        if not known:
            self.reply(404, {})
        elif len(chunks) == 4 and chunks[2] == "by-request":
            if self.server.hold and not self.server.release.is_set():
                self.reply(404, {})
            else:
                self.reply(200, {"version": 1, "request_id": request, "handle": request})
        elif len(chunks) == 3:
            self.reply(200, {"version": 1, "status": "succeeded", "handle": request,
                             "provider_charge": {"unit": "credit", "quantity": "1.25"}})
        elif chunks[3:] == ["artifacts", "0"]:
            self.reply(200, PIXELS, "image/png")
        else:
            self.reply(404, {})


class Connector(ThreadingHTTPServer):
    def __init__(self, hold=False):
        super().__init__((HOST, 0), ConnectorHandler)
        self.token = token_hex(32)
        self.hold = hold
        self.jobs = Counter()
        self.lock = Lock()
        self.started, self.release = Event(), Event()


def eventually(probe, label, timeout=22):
    deadline = monotonic() + timeout
    while monotonic() < deadline:
        if probe():
            return
        sleep(0.06)
    raise RuntimeError(f"Timed out waiting for {label}")


def percentiles(values):
    ordered = sorted(values)
    assert ordered, "expected at least one timing sample"
    return {name: round(ordered[ceil(len(ordered) * rank) - 1], 2)
            for name, rank in (("p50", 0.50), ("p95", 0.95), ("p99", 0.99))}


@contextmanager
def sample_database_locks(directory, settings):
    # The runtime login can see wait events for the cell's other connections.
    database = urlsplit(settings["DATABASE_URL"])
    env = {key: settings[key] for key in ("PATH", "LANG")}
    env.update(PGHOST=database.hostname, PGPORT=str(database.port),
               PGUSER=unquote(database.username), PGPASSWORD=unquote(database.password),
               PGDATABASE=unquote(database.path.removeprefix("/")), PGOPTIONS="-c statement_timeout=2000")
    path = directory / "lock-waits.log"
    with path.open("wb") as log, subprocess.Popen([postgres_bin() / "psql", "-XqAtw", "-v", "ON_ERROR_STOP=1"],
                                                  stdin=subprocess.PIPE, stdout=log, env=env) as monitor:
        monitor.stdin.write(rb"""SELECT count(*) FROM pg_stat_activity
                                 WHERE datname = current_database() AND usename = current_user
                                   AND wait_event_type = 'Lock' AND pid <> pg_backend_pid();
                              \watch 0.1
                              """)
        monitor.stdin.close()
        try:
            yield path
        finally:
            if monitor.poll() is None:
                monitor.terminate()
                try:
                    monitor.wait(timeout=4)
                except subprocess.TimeoutExpired:
                    monitor.kill()
                    monitor.wait()
            else:
                assert monitor.returncode == 0, "PostgreSQL wait sampler failed"


def pool_times(log_offsets):
    values = []
    for path, offset in log_offsets.items():
        with path.open("rb") as log:
            log.seek(offset)
            contents = re.sub(rb"\x1b\[[0-9;]*m", b"", log.read())
        values.extend(float(value) * 1000 for value in re.findall(rb"aquired_after_secs=([0-9.eE+-]+)", contents))
    return values


def queue_elapsed(view):
    times = {event["kind"]: datetime.fromisoformat(event["at"]) for event in view["events"]}
    return (times["provider_submit_started"] - times["accepted"]).total_seconds() * 1000


def run(gateway, directory, settings, session, account, project):
    forward_port, peer_port, worker_port = (loopback_port() for _ in range(3))
    api = int(settings["API_GATEWAY_PORT"])
    with ExitStack() as stack:
        bucket = stack.enter_context(serve(HeldBucket()))
        adapters = {name: stack.enter_context(serve(Connector(name == "slow")))
                    for name in ("media-a", "media-b", "slow")}
        for adapter in (*adapters.values(), bucket):
            stack.callback(adapter.release.set)
        catalog = json.loads(Path(settings["TOKENCRAFT_CORE_CATALOG"]).read_bytes())
        prototype = json.loads((ROOT / "examples/connector/catalog.json").read_bytes())["offers"][0]
        for name in adapters:
            catalog["offers"].append(dict(prototype, id=name, model=name, local_binding=name))
        path = directory / "cell-catalog.json"
        path.write_text(json.dumps(catalog))
        bindings = directory / "cell-connectors.json"
        bindings.write_text(json.dumps({"version": 1, "bindings": [{
            "name": name, "revision": "stress", "origin": f"http://{HOST}:{adapter.server_port}/",
            "token_env": f"TOKENCRAFT_CONNECTOR_TOKEN_{index}", "completion_webhooks": True,
        } for index, (name, adapter) in enumerate(adapters.items())]}))
        settings = dict(settings, RUST_LOG="warn,sqlx::pool::acquire=debug", NO_COLOR="1",
                        TOKENCRAFT_CORE_CATALOG=str(path), TOKENCRAFT_CONNECTORS=str(bindings), **{
            f"TOKENCRAFT_CONNECTOR_TOKEN_{index}": adapter.token for index, adapter in enumerate(adapters.values())
        })
        settings.update({f"TOKENCRAFT_ARTIFACT_S3_{name}": value for name, value in {
            "ENDPOINT": f"http://{HOST}:{bucket.server_port}", "BUCKET": "fixture-bucket", "REGION": "us-east-1",
            "ACCESS_KEY": "fixture-access", "SECRET_KEY": bucket.secret,
        }.items()})
        for label, port, role in (("cell-a", api, "api"), ("cell-b", peer_port, "api"), ("cell-worker", worker_port, "worker")):
            stack.enter_context(service(label, [gateway], directory,
                dict(settings, TOKENCRAFT_PROCESS_ROLE=role, API_GATEWAY_PORT=str(port)), lambda: ready(port)))
        trust = directory / "trust.txt"
        trust.write_text("127.0.0.1/32\n")
        proxy = directory / "haproxy.cfg"
        proxy.write_text(re.sub(r"^    server-template .*$", "\n".join(
            f"    server api{index} {HOST}:{port} check" for index, port in enumerate((api, peer_port))),
            (ROOT / "deploy/haproxy/cell-api.cfg").read_text(), flags=re.M))
        proxy_env = dict(PATH=settings["PATH"], LANG="C", TOKENCRAFT_LB_BIND=f"{HOST}:{forward_port}",
                         TOKENCRAFT_LB_TRUSTED_INGRESS_FILE=str(trust), TOKENCRAFT_LB_MAX_CONNECTIONS="256",
                         TOKENCRAFT_LB_CONNECTIONS_PER_API="64", TOKENCRAFT_LB_ENABLE_IP_RATE_LIMIT="false")
        stack.enter_context(service("cell-ingress", ["haproxy", "-db", "-f", proxy], directory, proxy_env,
                                   lambda: call(forward_port, "/_tokencraft/ingress-ready", 200)[0] == b""))
        def req(path, status=200, *, auth=None, **kwargs):
            return call(forward_port, path, status, headers={**FORWARDED, **(auth or {})}, **kwargs)

        def project_key(session, account, project):
            response, _ = call(api, f"/v1/projects/{project}/service-principals?account_id={account}", 201,
                headers=session, method="POST", body={"name": "Cell fixture", "scopes": [
                    "requests:submit", "requests:read", "usage:read",
                ]})
            return {**FORWARDED, "Authorization": "Bearer " + response["token"]}

        media_key = project_key(session, account, project)
        chat_keys = [project_key(*provision(api)) for _ in range(2)]
        health = {"Authorization": "Bearer " + settings["TOKENCRAFT_CAPACITY_TOKEN"]}
        lock_log = stack.enter_context(sample_database_locks(directory, settings))
        logs = [directory / f"{replica}.log" for replica in ("cell-a", "cell-b", "cell-worker")]
        offsets = {path: path.stat().st_size for path in logs}
        bucket.hold.set()

        def image(index, model, port=forward_port):
            headers = {**media_key, "Idempotency-Key": f"stress-media-{index}"}
            receipt, _ = call(port, "/v1/image", (200, 202), headers=headers,
                              method="POST", body={
                                  "model": model, "task": "text_to_image", "prompt": "fixture pixels",
                                  "count": 1, "aspect_ratio": "1:1",
                              })
            return receipt

        slow = image(0, "slow")
        assert adapters["slow"].started.wait(5)
        with ThreadPoolExecutor(max_workers=12) as executor:
            media = list(executor.map(lambda i: image(i, ["media-a", "media-b"][i % 2]), range(1, 13)))
            eventually(lambda: bucket.stalled_uploads() >= 3, "three pending artifact uploads", 10)
            wait_for_status(forward_port, slow, media_key, "awaiting_reconciliation", 8)
            signals, _ = call(worker_port, "/internal/capacity", 200, headers=health)
            assert signals["provider_tasks"] == 3 and signals["provider_capacity"] == 4
            def small_chat(index):
                started = monotonic()
                response, headers = req("/v1/chat/completions", auth={**chat_keys[index % 2],
                    "Idempotency-Key": f"stress-chat-{index}"}, method="POST",
                    body={"model": "fixture-partner", "messages": [{"role": "user", "content": "healthy chat"}]})
                assert response["choices"][0]["message"]["content"]
                return monotonic() - started, headers.get("TokenCraft-Request-Id")
            futures = [executor.submit(small_chat, i) for i in range(6)]
            try:
                values = [work.result(timeout=6) for work in futures]
                assert bucket.stalled_uploads() >= 3
                assert all(identifier for _, identifier in values)
                fault_summary = {
                    "healthy_chat_requests": len(values), "stalled_artifact_writes": bucket.stalled_uploads(),
                    "chat_latency_ms": percentiles([value * 1000 for value, _ in values]),
                }
            finally:
                bucket.release.set()
                for adapter in adapters.values():
                    adapter.release.set()
        for receipt in (slow, *media):
            wait_for_status(forward_port, receipt, media_key, "succeeded", 40)
            assert read_image(forward_port, receipt, media_key) == PIXELS
        # Reuse the original request intent on both replicas after recovery.
        for replica in (api, peer_port):
            assert image(0, "slow", replica)["id"] == slow["id"]
            assert image(1, "media-b", replica)["id"] == media[0]["id"]
        submitted = sum((adapter.jobs for adapter in adapters.values()), Counter())
        assert submitted == Counter(item["id"] for item in (slow, *media))
        usage, _ = req("/v1/usage", auth=media_key)
        assert usage["totals"]["completed_outputs"] == len(media) + 1
        assert usage["totals"]["provider_charges"][0]["quantity"] == "16.25"
        for key in chat_keys:
            usage, _ = req("/v1/usage", auth=key)
            assert usage["totals"]["succeeded"] == 3
            assert usage["totals"]["completed_outputs"] == 3
            req(slow["status_url"], 404, auth=key)
        jobs = [(media_key, receipt["id"]) for receipt in (slow, *media)]
        jobs += [(chat_keys[index % 2], identifier) for index, (_, identifier) in enumerate(values)]
        queue_ms = [queue_elapsed(req(f"/v1/requests/{identifier}", auth=key)[0]) for key, identifier in jobs]
        takes = pool_times(offsets)
        lock_counts = [int(row) for row in lock_log.read_text().splitlines() if row.isdecimal()]
        assert len(lock_counts) > 1, "expected repeated PostgreSQL lock samples"
        print(json.dumps({
            **fault_summary, "media_queue_delay_ms": percentiles(queue_ms[:len(media) + 1]),
            "chat_queue_delay_ms": percentiles(queue_ms[len(media) + 1:]),
            "pool_acquire_ms": percentiles(takes), "pool_acquisitions_observed": len(takes),
            "lock_samples": len(lock_counts), "peak_sampled_db_lock_waiters": max(lock_counts),
        }), flush=True)
        print("PASS mixed tenants survived provider and storage stalls with exact artifacts and no resubmissions", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    gateway, admin = (path.resolve(strict=True) for path in (args.gateway, args.admin))
    with TemporaryDirectory(prefix="tokencraft-cell-faults-") as directory:
        rehearse(gateway, admin, Path(directory), verification=run)
