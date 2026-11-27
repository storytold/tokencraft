"""Exercise a published workflow contract with disposable gateway and delivery worker processes."""

import argparse
from base64 import b64decode, b64encode
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack, contextmanager
from datetime import datetime, timedelta, timezone
from hashlib import sha256
import hmac
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
from queue import Queue
from secrets import token_hex
import signal
import socket
import subprocess
import sys
from tempfile import TemporaryDirectory
from threading import Thread
from time import monotonic, sleep, time

from postgres_tools import postgres_bin


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / ".github/fixtures/container"
MEDIA = ROOT / "plugins/tools/media"
MONITOR = ROOT / "deploy/monitoring"
HOST = "127.0.0.1"
CALLER_TRACE_ID = "4bf92f3577b34da6a3ce929d0e0e4736"
CALLER_SPAN_ID = "00f067aa0ba902b7"
CALLER_CONTEXT = {
    "traceparent": f"00-{CALLER_TRACE_ID}-{CALLER_SPAN_ID}-01",
    "tracestate": "fixture=private",
    "baggage": "customer=private",
}


def loopback_port():
    with socket.socket() as listener:
        listener.bind((HOST, 0))
        return listener.getsockname()[1]


def command(*args, cwd, env):
    result = subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise RuntimeError(f"{Path(args[0]).name} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def call(port, path, expected, *, method="GET", body=None, headers=None,
         max_bytes=2 * 1024 * 1024, timeout=5):
    statuses = (expected,) if isinstance(expected, int) else expected
    fields = {"Origin": f"http://localhost:{port}", **(headers or {})}
    if body is not None and not isinstance(body, bytes):
        fields["Content-Type"] = "application/json"
        body = json.dumps(body).encode()
    connection = HTTPConnection(HOST, port, timeout=timeout)
    try:
        connection.request(method, path, body, fields)
        result = connection.getresponse()
        if result.status not in statuses:
            raise RuntimeError(f"{method} {path.split('?')[0]} returned {result.status}, expected {expected}")
        data = result.read(max_bytes + 1)
        if len(data) > max_bytes:
            raise RuntimeError("The fixture response exceeded the expected size")
        if result.headers.get_content_type() == "application/json":
            data = json.loads(data)
        return data, result.headers
    finally:
        connection.close()


def provider_traceparent(port, request_id, auth):
    document, _ = call(port, f"/v1/requests/{request_id}/trace/otlp", 200, headers=auth)
    root = document["resourceSpans"][0]["scopeSpans"][0]["spans"][0]
    assert root["traceId"] != CALLER_TRACE_ID
    assert root["links"] == [{"traceId": CALLER_TRACE_ID, "spanId": CALLER_SPAN_ID}]
    return f"00-{root['traceId']}-{root['spanId']}-01"


def assert_private_response(headers):
    assert headers.get_all("Cache-Control", []) == ["private, no-store"], headers


def verify_cache_policy(port, session, account, project):
    for path, status, headers in (
        ("/v1/auth/me", 200, session),
        ("/v1/auth/me", 401, None),
        ("/v1/auth/sessions", 200, session),
        ("/v1/accounts", 200, session),
        ("/v1/user/audit_logs", 200, session),
        (f"/v1/usage?account_id={account}&project_id={project}", 200, session),
        ("/v1/not-a-route", 404, session),
    ):
        _, response = call(port, path, status, headers=headers)
        assert_private_response(response)
    for path in ("/openapi.json", "/v1/schemas/primitives"):
        _, response = call(port, path, 200)
        assert response.get_all("Cache-Control", []) == ["public, max-age=300"], response
    print("PASS session, audit, usage and errors are non-storable while public schema documents retain their TTL")


def ready(port):
    return call(port, "/ready", 200)[0]["ready"] is True


@contextmanager
def service(name, args, directory, env, probe, *, shutdown=signal.SIGTERM):
    with (directory / f"{name}.log").open("w+") as log:
        child = subprocess.Popen(args, cwd=directory, env=env, stdin=subprocess.DEVNULL,
                                 stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = monotonic() + 30
            while child.poll() is None and monotonic() < deadline:
                try:
                    if probe():
                        break
                except (OSError, RuntimeError):
                    pass
                sleep(0.1)
            else:
                raise RuntimeError(f"{name} failed to become ready")
            yield child
        except Exception:
            log.seek(0)
            print(log.read()[-6000:], file=sys.stderr)
            raise
        finally:
            if child.poll() is None:
                child.send_signal(shutdown)
                try:
                    child.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()


def provision(port, *, origin=None):
    origin_header = {} if origin is None else {"Origin": origin}
    _, headers = call(port, "/v1/auth/signup", 201, method="POST", body={
        "email": f"workflow-smoke-{token_hex(8)}@example.test",
        "password": token_hex(24),
        "display_name": "Workflow contract fixture",
    }, headers=origin_header)
    session = {**origin_header, "Cookie": headers["Set-Cookie"].split(";", 1)[0]}
    accounts, _ = call(port, "/v1/accounts", 200, headers=session)
    account = accounts["accounts"][0]["account_id"]
    project, _ = call(port, f"/v1/projects?account_id={account}", 201, method="POST",
                      body={"name": "Workflow contract check"}, headers=session)
    return session, account, project["id"]


def wait_for_run(port, receipt, headers):
    deadline = monotonic() + 20
    while monotonic() < deadline:
        state, result_headers = call(port, receipt["status_url"], 200, headers=headers)
        if state["status"] == "succeeded":
            break
        if state["status"] in ("failed", "canceled", "expired"):
            raise RuntimeError(f"Workflow reached {state['status']}")
        sleep(0.2)
    else:
        raise RuntimeError("Workflow did not complete before the deadline")
    return state, result_headers


def verify_output(port, receipt, headers, digest, size):
    state, result_headers = wait_for_run(port, receipt, headers)
    output = state["outputs"]["fingerprint"]
    path, separator, query = receipt["status_url"].partition("?")
    assert output == {
        "type": "media", "kind": "metadata", "download_url": f"{path}/outputs/fingerprint{separator}{query}",
    }
    record, response = call(port, output["download_url"], 200, headers=headers)
    assert response.get_content_type() == "application/json"
    assert record == {"content_type": "image/png", "size_bytes": size, "sha256": digest}
    return state, result_headers


def verify_json_response(spec, operation, code, payload, headers, expected_headers=()):
    response = operation["responses"][str(code)]
    if "$ref" in response:
        response = spec["components"]["responses"][response["$ref"].rsplit("/", 1)[1]]
    schema = response["content"]["application/json"]["schema"]
    if "$ref" in schema:
        schema = spec["components"]["schemas"][schema["$ref"].rsplit("/", 1)[1]]
    assert set(schema["required"]) <= payload.keys()
    for field in expected_headers:
        assert field in response["headers"] and headers[field]


@contextmanager
def begin_upload(port, path, headers, size):
    # The interim response confirms the server started reading this upload.
    connection = HTTPConnection(HOST, port, timeout=5)
    try:
        connection.putrequest("POST", path)
        for name, value in headers.items():
            connection.putheader(name, value)
        connection.putheader("Content-Length", str(size))
        connection.putheader("Expect", "100-continue")
        connection.endheaders()
        with connection.sock.makefile("rb", buffering=0) as interim:
            assert interim.readline().split()[1:2] == [b"100"]
            assert interim.readline() == b"\r\n"
        yield connection
    finally:
        connection.close()


def verify_upload_capacity(port, path, spec, operation, auth, file_headers, png, receipt):
    headers = {**auth, **file_headers}
    with begin_upload(port, path, headers, len(png)) as first, begin_upload(port, path, headers, len(png)) as second:
        error, response = call(port, path, 503, method="POST", body=png, headers=headers)
        verify_json_response(spec, operation, 503, error, response, expected_headers=("Retry-After",))
        assert error["error"]["code"] == "service_unavailable" and int(response["Retry-After"]) > 0
        call(port, path, 401, method="POST", body=png, headers=file_headers)
        for connection in (first, second):
            connection.send(png)
            with connection.getresponse() as completed:
                assert completed.status == 201 and json.load(completed) == receipt
    retried, _ = call(port, path, 201, method="POST", body=png, headers=headers)
    assert retried == receipt
    print("PASS two authenticated upload slots, retry hint, and stable replay after capacity frees")


def verify_run_capacity(port, peer, path, spec, body, original, rotated):
    initial = {**original, "Idempotency-Key": token_hex(16)}
    first, _ = call(port, path, 202, method="POST", body=body, headers=initial)
    attempts = [{**auth, "Idempotency-Key": token_hex(16)} for auth in (original, rotated)]
    with ThreadPoolExecutor(max_workers=2) as threads:
        pending = [threads.submit(call, target, path, (202, 429), method="POST", body=body, headers=headers)
                   for target, headers in zip((port, peer), attempts)]
        responses = [future.result() for future in pending]
    (denied,) = [index for index, (payload, _) in enumerate(responses) if "error" in payload]
    error, response = responses[denied]
    verify_json_response(spec, spec["paths"][path]["post"], 429, error, response, expected_headers=("Retry-After",))
    assert error["error"]["code"] == "active_request_limit" and int(response["Retry-After"]) > 0
    assert responses[1 - denied][0]["id"] != first["id"]

    replay, _ = call(peer, path, 200, method="POST", body=body, headers={**initial, **rotated})
    assert replay == {**first, "replayed": True}
    call(peer, first["status_url"] + "/cancel", 204, method="POST", headers=rotated)
    retried, _ = call(port, path, 202, method="POST", body=body, headers=attempts[denied])
    assert retried["replayed"] is False
    print("PASS two active runs across API replicas and keys, receipt replay at capacity, and admission after cancellation")
    return first["id"]


def queue_count(report, kind):
    return sum(row["count"] for row in report["queues"] if row["kind"] == kind)


def verify_workflow_operations(port, session, auth, query, project, sibling_project):
    path = "/v1/operations/queues" + query
    in_project, _ = call(port, path + f"&project_id={project}", 200, headers=session)
    organization, _ = call(port, path, 200, headers=session)
    sibling, _ = call(port, path + f"&project_id={sibling_project}", 200, headers=session)
    call(port, path, 401, headers=auth)
    workflows = in_project["workflows"]
    assert workflows == organization["workflows"]
    assert workflows["queued"]["count"] == workflows["due_queued"] == 2
    assert workflows["queued"]["oldest_at"] <= in_project["observed_at"]
    assert workflows["running"] == workflows["reconciling"] == {"count": 0, "oldest_at": None}
    assert workflows["overdue"] == 0
    outcomes = workflows["outcomes"]
    assert outcomes["sampled"] == 3 and outcomes["sample_limit"] == 5000
    assert outcomes["succeeded"] == outcomes["timing_samples"] == 2 and outcomes["canceled"] == 1
    assert outcomes["failed"] == outcomes["expired"] == 0 and outcomes["p95_succeeded_ms"] >= 0
    assert sibling["workflows"]["outcomes"]["sampled"] == 0
    assert sibling["workflows"]["queued"] == {"count": 0, "oldest_at": None}
    assert queue_count(in_project, "webhook_pending") == queue_count(organization, "webhook_pending") == 1
    assert queue_count(sibling, "webhook_pending") == 0
    print("PASS scoped operations shows workflow pressure, bounded outcomes, and pending app deliveries")


class WebhookReceiver(BaseHTTPRequestHandler):
    def do_POST(self):
        event = {key.lower(): value for key, value in self.headers.items()}
        body = self.rfile.read(int(event["content-length"]))
        event_id = event["webhook-id"]
        first = event_id not in self.server.seen
        self.server.seen.add(event_id)
        self.server.received.put((event, body))
        self.send_response(503 if first else 204)
        self.end_headers()

    def log_message(self, *_):
        pass


@contextmanager
def webhook_receiver():
    with HTTPServer((HOST, 0), WebhookReceiver) as server:
        server.seen = set()
        server.received = Queue()
        listener = Thread(target=server.serve_forever)
        listener.start()
        try:
            yield server
        finally:
            server.shutdown()
            listener.join()


def verify_signed_delivery(
    port, listing, query, session, receiver, subscription, event_type, data, *, expected_payload=None,
):
    secret = b64decode(subscription["signing_secret"].removeprefix("whsec_"))
    original = None
    for _ in range(2):
        headers, raw = receiver.received.get(timeout=10)
        if expected_payload is not None:
            assert raw == expected_payload
        notice = json.loads(raw)
        event_id = headers["webhook-id"]
        timestamp = headers["webhook-timestamp"]
        signed = f"{event_id}.{timestamp}.".encode() + raw
        expected = "v1," + b64encode(hmac.digest(secret, signed, sha256)).decode()
        assert abs(time() - int(timestamp)) < 60 and hmac.compare_digest(expected, headers["webhook-signature"])
        assert notice["id"] == event_id and notice["type"] == event_type and notice["schema_version"] == 1
        assert notice["data"] == data
        assert original is None or original == (event_id, raw)
        original = (event_id, raw)
    detail_path = listing + f"/{event_id}" + query
    until = monotonic() + 5
    while monotonic() < until:
        detail, _ = call(port, detail_path, 200, headers=session)
        delivery = detail["delivery"]
        if delivery["state"] == "delivered":
            break
        sleep(0.1)
    else:
        raise RuntimeError("Signed webhook delivery was not recorded")
    assert delivery["event_type"] == event_type and delivery["subscription_id"] == subscription["id"]
    assert [attempt["http_status"] for attempt in detail["attempts"]] == [204, 503]
    return delivery


def verify_request_webhook(port, session, project_path, sibling_project, query, receiver, workflow_subscription):
    client = "native.customer-42"
    registration = {"name": "Native receipt fixture", "destination_id": "fixture", "client_ref": client}
    native, _ = call(port, project_path + "/webhooks" + query, 201, method="POST", headers=session, body=registration)
    principal, _ = call(port, project_path + "/service-principals" + query, 201, method="POST", headers=session,
                        body={"name": "Native webhook receipt", "scopes": ["requests:submit", "requests:read"]})
    auth = {"Authorization": "Bearer " + principal["token"]}
    native_path = project_path + "/webhooks/" + native["id"] + query
    visible, response = call(port, native_path, 200, headers=session)
    assert visible == {field: value for field, value in native.items() if field != "signing_secret"}
    assert_private_response(response)
    call(port, native_path, 401, headers=auth)
    _, hidden = call(port, f"/v1/projects/{sibling_project}/webhooks/{native['id']}" + query, 404, headers=session)
    assert_private_response(hidden)
    quote, _ = call(port, f"/v1/quotes/scoped?client_ref={client}", 200, method="POST", headers=auth,
                    body={"model": "runtime_image", "workload": {"kind": "image", "count": 1, "aspect_ratio": "4:3"}})
    body = {"quote_id": quote["id"], "input": quote["input"], "client_ref": client,
            "delivery": {"webhook_subscription_id": workflow_subscription["id"]}}
    invoke = {**auth, "Idempotency-Key": token_hex(16)}
    call(port, "/v1/requests", 404, method="POST", body=body, headers=invoke)
    body["delivery"]["webhook_subscription_id"] = native["id"]
    accepted, _ = call(port, "/v1/requests", 202, method="POST", body=body, headers=invoke)
    delivery = verify_signed_delivery(port, project_path + "/webhook-deliveries", query, session, receiver,
        native, "request.succeeded",
        {"request_id": accepted["id"], "project_id": project_path.rsplit("/", 1)[1],
         "status": "succeeded", "fetch_url": accepted["status_url"], "client_ref": client})
    assert delivery["request_id"] == accepted["id"] and "workflow_run_id" not in delivery
    print("PASS native request webhooks sign the client reference and cannot use an app-only subscription")


def verify_tool_rollout(gateway, directory, settings, registry, sibling_project, digest):
    candidate = directory / "candidate-tool-bindings.json"
    catalog = settings["TOKENCRAFT_CORE_CATALOG"]
    config = json.loads(registry.read_bytes())
    (binding,) = config["bindings"]
    projects = binding["projects"]

    def check(reason):
        env = dict(settings, TOKENCRAFT_WORKFLOW_TOOL_BINDINGS=str(candidate))
        result = subprocess.run([gateway, "catalog", "check-drain", "--catalog", catalog],
                                cwd=directory, env=env, capture_output=True, text=True, timeout=10)
        assert result.returncode == (1 if reason else 0), result.stderr
        report = json.loads(result.stdout)
        assert report["checked_workflow_tool_references"] == 2 and report["ready"] is (reason is None)
        blockers = [] if reason is None else [{
            "binding_sha256": digest, "operation": "delivery.fingerprint", "operation_revision": 1,
            "runs": 2, "reason": reason,
        }]
        assert report["workflow_tool_blockers"] == blockers

    candidate.write_text(json.dumps({"version": 1, "bindings": []}))
    check("tool_binding_unavailable")
    candidate.write_text(json.dumps(config))
    check(None)
    binding["drain_only"] = True
    candidate.write_text(json.dumps(config))
    check(None)
    binding["projects"] = [sibling_project]
    candidate.write_text(json.dumps(config))
    check("tool_scope_unavailable")
    binding["projects"] = projects
    candidate.write_text(json.dumps(config))
    settings["TOKENCRAFT_WORKFLOW_TOOL_BINDINGS"] = str(candidate)
    print("PASS rollout checks queued tool pins, accepts drain-only retention, and preserves project authorization")


def verify_worker_binding_scope(gateway, directory, settings, registry, port, session, account, project, sibling,
                                query_db):
    candidate = directory / "other-worker-bindings.json"
    config = json.loads(registry.read_bytes())
    config["bindings"][0]["projects"] = [sibling]
    candidate.write_text(json.dumps(config))

    scope = f"?account_id={account}&project_id={project}"
    graph, _ = call(port, "/v1/workflows/plan" + scope, 200, method="POST", headers=session,
                    body={"intent": "Check worker binding selection", "primitives": [{"kind": "image", "title": "Image"}]})
    assert graph["nodes"][0]["kind"] == "image"
    query = f"?account_id={account}"
    endpoint = f"/v1/projects/{project}/workflows"
    draft, _ = call(port, endpoint + query, 201, method="POST", headers=session, body=graph)

    for bindings in (None, candidate):
        worker_port = loopback_port()
        env = dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port))
        del env["TOKENCRAFT_DEMO_WEBHOOK_CONFIG"]
        if bindings is None:
            del env["TOKENCRAFT_WORKFLOW_TOOL_BINDINGS"]
        else:
            env["TOKENCRAFT_WORKFLOW_TOOL_BINDINGS"] = str(bindings)
        with service(f"other-worker-{worker_port}", [gateway], directory, env, lambda: ready(worker_port)):
            receipt, _ = call(port, f"{endpoint}/{draft['id']}/test-runs" + query, 202, method="POST",
                              headers={**session, "Idempotency-Key": token_hex(16)},
                              body={"revision": draft["revision"], "inputs": {"brief": "Synthetic image"}})
            wait_for_run(port, receipt, session)
            rows = query_db(
                "SELECT count(*) FROM workflow_runs WHERE project_id = :'project' AND lease = 0 "
                "AND status = 'queued' AND graph->'nodes'->0->>'kind' = 'tool'", project=project)
            assert rows == "2", f"Expected both pinned runs to stay unclaimed, found {rows}"
    print("PASS workers without the matching project binding leave tool runs for an eligible worker")


def verify_generation_rollout(gateway, directory, settings, step_cap):
    candidate = directory / "candidate-generation-catalog.json"
    config = json.loads(Path(settings["TOKENCRAFT_CORE_CATALOG"]).read_bytes())
    offers = config["offers"]
    offer_by_id = {entry["id"]: entry for entry in offers}
    offer = offer_by_id["runtime_image"]
    private = offer_by_id["a_private_image"]

    def check(blocked, **overrides):
        candidate.write_text(json.dumps(config))
        command = [gateway, "catalog", "check-drain", "--catalog", candidate]
        result = subprocess.run(command, cwd=directory, env=dict(settings, **overrides),
                                capture_output=True, text=True, timeout=10)
        assert result.returncode == int(blocked), (result.stdout, result.stderr)
        report = json.loads(result.stdout)
        assert report["ready"] is (not blocked) and report["checked_requests"] == 0
        assert report["checked_workflow_generation_references"] == 1, report
        expected = [{"model": offer["model"], "kind": "image", "first_frame": False,
                     "references": 1, "reason": "generation_route_unavailable"}] if blocked else []
        assert report["workflow_generation_blockers"] == expected, report
        return report["base_revision"]

    check(False)
    config["offers"] = [entry for entry in offers if entry["id"] != offer["id"]]
    check(True)
    config["offers"] = offers
    offer["pricing"]["aspect_ratios"] = ["1:1"]
    check(True)
    offer["pricing"]["aspect_ratios"] = ["4:3"]
    offer["pricing"]["usd_nanos_per_image"] = step_cap + 1
    check(True)
    offer["pricing"]["usd_nanos_per_image"] = step_cap
    offer["provider"] = "alternate_fixture"
    check(True)
    offer["provider"] = "self_hosted"
    config["connection_access"]["self_hosted"] = config["connection_access"][private["provider"]]
    check(True)
    del config["connection_access"]["self_hosted"]
    revision = check(False)

    feed = directory / "candidate-generation-rates.json"
    now = datetime.now(timezone.utc)
    feed.write_text(json.dumps({
        "base_revision": revision,
        "observed_at": (now - timedelta(minutes=1)).isoformat(),
        "valid_until": (now + timedelta(hours=1)).isoformat(),
        "rates": {offer["id"]: {"source_url": "https://example.invalid/fixture-image", "schedule": {
            "kind": "image", "usd_nanos_per_image": step_cap + 1,
        }}},
    }))
    check(True, TOKENCRAFT_DEMO_RATE_FEED=str(feed))
    print("PASS rollout checks queued generation models, fixed shapes, step caps, project policy, sharing and rates")


def verify_project_catalog(port, session, account, project):
    private = "a_private_image"
    caps_path = "/v1/workflows/capabilities"
    account_query = f"?account_id={account}"
    owner_scope = account_query + f"&project_id={project}"
    sibling, _ = call(port, "/v1/projects" + account_query, 201, method="POST", headers=session,
                      body={"name": "Catalog scope fixture"})
    sibling_scope = account_query + f"&project_id={sibling['id']}"
    for query in ("", sibling_scope):
        caps, _ = call(port, caps_path + query, 200, headers=session)
        assert private not in {model["id"] for model in caps["models"]}
    caps, _ = call(port, caps_path + owner_scope, 200, headers=session)
    assert private in {model["id"] for model in caps["models"]}
    call(port, caps_path + account_query, 400, headers=session)

    intent = {"intent": "Create a hero image", "primitives": [{"kind": "image", "title": "Generate image"}]}
    graph, _ = call(port, "/v1/workflows/plan" + owner_scope, 200, method="POST", headers=session, body=intent)
    sibling_graph, _ = call(port, "/v1/workflows/plan" + sibling_scope, 200, method="POST", headers=session, body=intent)
    assert graph["nodes"][0]["model"] == private and sibling_graph["nodes"][0]["model"] == "runtime_image"

    for target in (project, sibling["id"]):
        route = f"/v1/projects/{target}/workflows"
        draft, _ = call(port, route + account_query, 201, method="POST", headers=session, body=graph)
        manage = f"{route}/{draft['id']}"
        run, _ = call(port, manage + "/test-runs" + account_query, 202 if target == project else 400,
                      method="POST", headers={**session, "Idempotency-Key": token_hex(16)},
                      body={"revision": draft["revision"], "inputs": {"brief": "Synthetic image"}})
        if target == project:
            wait_for_run(port, run, session)
            call(port, manage + "/versions" + account_query, 200, method="POST", headers=session,
                 body={"revision": draft["revision"], "active_version": None})
        else:
            assert run["error"]["code"] == "bad_request"
    print("PASS workflow discovery, planning, testing, and publication enforce the scoped custom catalog")


def test_during_tool_pause(port, session, tool_path, tool_body, delivery, query_db):
    image_session, account, project = provision(port)
    query = f"?account_id={account}"
    graph, _ = call(port, "/v1/workflows/plan" + query + f"&project_id={project}", 200, method="POST",
                    headers=image_session, body={"intent": "Check parallel workflow claims",
                    "primitives": [{"kind": "image", "title": "Image"}]})
    assert graph["nodes"][0]["kind"] == "image"
    endpoint = f"/v1/projects/{project}/workflows"
    draft, _ = call(port, endpoint + query, 201, method="POST", headers=image_session, body=graph)

    def submit(path, body, authentication):
        receipt, _ = call(port, path, 202, method="POST", body=body,
                          headers={**authentication, "Idempotency-Key": token_hex(16)})
        return receipt

    def wait_for_claim(run, stalled):
        end = monotonic() + 8
        while monotonic() < end:
            own, other = query_db(
                "SELECT lease > 0, (SELECT lease = 1 AND status = 'queued' FROM workflow_runs WHERE id = :'stalled') "
                "FROM workflow_runs WHERE id = :'run'", run=run, stalled=stalled).split("|")
            if other != "t" and run != stalled:
                raise AssertionError("The tool worker timed out before claiming the independent run")
            if own == "t":
                return
            sleep(0.03)
        raise AssertionError("The worker did not claim the expected run")

    delivery.send_signal(signal.SIGSTOP)
    try:
        slow = submit(tool_path, tool_body, session)
        wait_for_claim(slow["id"], slow["id"])
        fast = submit(f"{endpoint}/{draft['id']}/test-runs" + query,
                      {"revision": draft["revision"], "inputs": {"brief": "Synthetic image"}}, image_session)
        wait_for_claim(fast["id"], slow["id"])
    finally:
        delivery.send_signal(signal.SIGCONT)
    wait_for_run(port, fast, image_session)
    print("PASS one workflow advances while the same worker is waiting for a media tool")
    return slow


def verify_workflow(port, peer, session, account, project, stop_worker, restart_worker, receiver, check_rollout,
                    submit_trial):
    project_path = f"/v1/projects/{project}"
    query = f"?account_id={account}"
    tools, _ = call(port, project_path + "/workflow-tools" + query + "&verify=fixture-delivery", 200, headers=session)
    (binding,) = tools
    assert binding["reachable"] and binding["manifest"]["plugin"] == "tokencraft-delivery"
    draft, _ = call(port, project_path + "/workflows" + query, 201, method="POST", headers=session, body={
        "schema": 1, "title": "Workflow contract fixture", "max_usd_nanos": 0,
        "inputs": {"image": {"title": "Source image", "kind": "image"}},
        "outputs": {"fingerprint": "fingerprint"},
        "nodes": [{
            "id": "fingerprint", "title": "Fingerprint image", "kind": "tool",
            "model": "delivery.fingerprint", "max_usd_nanos": 0,
            "settings": {"source": {"source": "request", "field": "image"}},
            "tool": {"binding_revision": binding["revision"], "revision": 1, "inputs": {"source": "media"},
                     "output": "metadata", "parameters": {}},
        }],
    })
    manage = project_path + f"/workflows/{draft['id']}"
    endpoint = project_path + f"/workflow-endpoints/{draft['id']}"
    png = (ROOT / "frontend/public/brand/tokencraft-mark.png").read_bytes()
    digest = sha256(png).hexdigest()
    file_headers = {"Content-Type": "image/png", "X-TokenCraft-Content-Sha256": digest}
    uploaded, _ = call(port, endpoint + "/uploads" + query, 201, method="POST",
                       body=png, headers={**session, **file_headers})
    trial = {"revision": draft["revision"], "inputs": {"image": {"upload_id": uploaded["upload_id"]}}}
    test_path = manage + "/test-runs" + query
    checked = submit_trial(test_path, trial)
    verify_output(port, checked, session, digest, len(png))
    released, _ = call(port, manage + "/versions" + query, 200, method="POST", headers=session,
                       body={"revision": draft["revision"], "active_version": None})
    assert released["active_version"] == 1
    app, _ = call(port, manage + "/apps" + query, 201, method="POST", headers=session, body={
        "name": "Contract fixture app", "days": 1,
        "scopes": ["runs:submit", "runs:read", "outputs:read", "runs:cancel"],
    })
    hooks = project_path + "/webhooks"
    listing = project_path + "/webhook-deliveries"
    registration = {"name": "Terminal workflow fixture", "destination_id": "fixture", "workflow_app_id": app["app_id"]}
    sibling_project, _ = call(port, "/v1/projects" + query, 201, method="POST", headers=session,
                              body={"name": "Webhook scope fixture"})
    call(port, f"/v1/projects/{sibling_project['id']}/webhooks" + query, 404, method="POST",
         headers=session, body=registration)
    subscription, _ = call(port, hooks + query, 201, method="POST", headers=session, body=registration)
    assert subscription["workflow_app_id"] == app["app_id"]
    call(port, hooks + query, 409, method="POST", headers=session, body=registration)
    auth = {"Authorization": "Bearer " + app["secret"]}
    spec, _ = call(port, endpoint + "/openapi.json", 200, headers=auth)
    paths = spec["paths"]
    uploads = paths[endpoint + "/uploads"]["post"]
    assert spec["openapi"] == "3.1.0" and spec["info"]["version"] == "1"
    assert list(spec["x-workflow-upload-inputs"].items()) == [("image", "image")]
    assert set(uploads["requestBody"]["content"]) == {"image/png", "image/jpeg", "image/webp", "image/gif"}
    assert uploads["x-workflow-required-scope"] == "runs:submit"
    paths_run = endpoint + "/runs/{run_id}"
    assert paths[paths_run + "/cancel"]["post"]["x-workflow-required-scope"] == "runs:cancel"
    output_operation = paths[paths_run + "/outputs/{name}"]["get"]
    assert output_operation["x-workflow-required-scope"] == "outputs:read"
    assert "Retry-After" in output_operation["responses"]["503"]["headers"]

    uploaded, upload_headers = call(port, endpoint + "/uploads", 201, method="POST", body=png, headers={**auth, **file_headers})
    verify_json_response(spec, uploads, 201, uploaded, upload_headers)
    verify_upload_capacity(port, endpoint + "/uploads", spec, uploads, auth, file_headers, png, uploaded)
    body = {"inputs": {"image": {"upload_id": uploaded["upload_id"]}}}
    invoke = {**auth, "Idempotency-Key": token_hex(16)}
    receipt, accepted = call(port, endpoint + "/runs", 202, method="POST", body=body, headers=invoke)
    replay, repeated = call(port, endpoint + "/runs", 200, method="POST", body=body, headers=invoke)
    for code, payload, headers in ((202, receipt, accepted), (200, replay, repeated)):
        verify_json_response(spec, paths[endpoint + "/runs"]["post"], code, payload, headers,
                             expected_headers=("Location", "Retry-After"))
        assert payload["status_url"] == headers["Location"] == f"{endpoint}/runs/{payload['id']}"
        assert int(headers["Retry-After"]) > 0
    assert replay == {**receipt, "replayed": True} and receipt["replayed"] is False
    state, status_headers = verify_output(port, receipt, auth, digest, len(png))
    event = verify_signed_delivery(port, listing, query, session, receiver, subscription, "workflow.run.succeeded", {
        "run_id": receipt["id"], "workflow_id": draft["id"], "project_id": project,
        "workflow_version": 1, "status": "succeeded", "fetch_url": receipt["status_url"],
    })
    assert event["workflow_run_id"] == receipt["id"] and "request_id" not in event
    call(port, f"/v1/projects/{sibling_project['id']}/webhook-deliveries/{event['id']}" + query,
         404, headers=session)
    verify_request_webhook(port, session, project_path, sibling_project["id"], query, receiver, subscription)
    print("PASS app-scoped terminal webhook signature and retry retain the original metadata-only event")
    status_operation = paths[paths_run]["get"]
    verify_json_response(spec, status_operation, 200, state, status_headers, expected_headers=("ETag",))
    assert "304" in status_operation["responses"] and "ETag" in status_operation["responses"]["304"]["headers"]
    tag = status_headers["ETag"]
    empty, cached = call(port, receipt["status_url"], 304, headers={**auth, "If-None-Match": tag})
    assert empty == b"" and cached["ETag"] == tag
    assert_private_response(status_headers)
    assert_private_response(cached)
    error, rejected = call(port, receipt["status_url"] + "/cancel", 409, method="POST", headers=auth)
    verify_json_response(spec, paths[paths_run + "/cancel"]["post"], 409, error, rejected)
    assert error["error"]["code"] == "conflict"

    reader, _ = call(port, manage + "/apps" + query, 201, method="POST", headers=session, body={
        "name": "Separate read-only app", "days": 1, "scopes": ["runs:read"],
    })
    read_only = {"Authorization": "Bearer " + reader["secret"]}
    call(port, endpoint + "/openapi.json", 403, headers=read_only)
    call(port, endpoint + "/uploads", 403, method="POST", body=png, headers={**read_only, **file_headers})
    call(port, receipt["status_url"], 404, headers=read_only)

    rotated, _ = call(port, manage + f"/apps/{app['app_id']}/keys" + query, 201, method="POST",
                      headers=session, body={"days": 1})
    assert rotated["app_id"] == app["app_id"] and rotated["key_id"] != app["key_id"]
    replacement = {"Authorization": "Bearer " + rotated["secret"]}
    # Keep new runs queued while testing admission across independent API processes.
    stop_worker()
    canceled = verify_run_capacity(port, peer, endpoint + "/runs", spec, body, auth, replacement)
    events, _ = call(port, listing + query, 200, headers=session)
    workflow_events = [(event["event_type"], event["workflow_run_id"], event["state"])
                       for event in events["items"] if "workflow_run_id" in event]
    assert set(workflow_events) == {("workflow.run.succeeded", receipt["id"], "delivered"),
                                   ("workflow.run.canceled", canceled, "pending")}
    assert len(workflow_events) == 2
    print("PASS canceled run queued one durable event with the delivery worker stopped")
    verify_workflow_operations(port, session, auth, query, project, sibling_project["id"])
    check_rollout(sibling_project["id"], binding["revision"])

    sibling, _ = call(port, manage + "/apps" + query, 201, method="POST", headers=session, body={
        "name": "Independent submitting app", "days": 1, "scopes": ["runs:submit", "runs:read", "outputs:read"],
    })
    sibling_auth = {"Authorization": "Bearer " + sibling["secret"]}
    sibling_upload, _ = call(peer, endpoint + "/uploads", 201, method="POST",
                             body=png, headers={**sibling_auth, **file_headers})
    sibling_body = {"inputs": {"image": {"upload_id": sibling_upload["upload_id"]}}}
    sibling_run, _ = call(peer, endpoint + "/runs", 202, method="POST", body=sibling_body,
                          headers={**sibling_auth, "Idempotency-Key": token_hex(16)})
    # Two runs for the original app, one for the sibling, and seven draft tests fill the workflow.
    for _ in range(7):
        call(port, test_path, 202, method="POST", body=trial, headers={**session, "Idempotency-Key": token_hex(16)})
    full, full_headers = call(peer, endpoint + "/runs", 429, method="POST", body=sibling_body,
                              headers={**sibling_auth, "Idempotency-Key": token_hex(16)})
    assert full["error"]["code"] == "active_request_limit" and int(full_headers["Retry-After"]) > 0
    call(port, test_path, 429, method="POST", body=trial, headers={**session, "Idempotency-Key": token_hex(16)})
    print("PASS independent app capacity and the shared ten-run ceiling for published and draft runs")

    call(port, manage + f"/keys/{app['key_id']}" + query, 204, method="DELETE", headers=session)
    call(port, endpoint + "/uploads", 401, method="POST", body=png, headers={**auth, **file_headers})
    reused, _ = call(port, endpoint + "/uploads", 201, method="POST", body=png, headers={**replacement, **file_headers})
    assert reused == uploaded
    replay_after_rotation, _ = call(port, endpoint + "/runs", 200, method="POST", body=body,
                                   headers={**invoke, **replacement})
    assert replay_after_rotation == replay
    print("PASS application key rotation preserves upload ownership and run replay after revocation")

    pending = next(event for event in events["items"] if event.get("workflow_run_id") == canceled)
    call(port, hooks + f"/{subscription['id']}" + query, 204, method="DELETE", headers=session)
    restart_worker()
    delivery_path = listing + f"/{pending['id']}"
    until = monotonic() + 5
    while monotonic() < until:
        detail, _ = call(port, delivery_path + query, 200, headers=session)
        if detail["delivery"]["state"] == "dead":
            break
        sleep(0.1)
    else:
        raise RuntimeError("The restarted worker did not close the revoked delivery")
    assert detail["delivery"]["last_error"] == "subscription_revoked" and detail["attempts"] == []
    operations, _ = call(port, "/v1/operations/queues" + query + f"&project_id={project}", 200, headers=session)
    assert queue_count(operations, "webhook_dead") >= 1
    call(port, delivery_path + "/redrive" + query, 409, method="POST", headers=session)
    verify_output(peer, sibling_run, sibling_auth, digest, len(png))
    print("PASS restarted worker finished a pinned tool run with the drain-only binding")
    entries, _ = call(port, listing + query, 200, headers=session)
    assert all(item.get("workflow_run_id") != sibling_run["id"] for item in entries["items"])
    print("PASS restarted worker honors subscription revocation before delivering or redriving a queued event")
    print("PASS published workflow contract, upload, digest, replay, conditional polling, cancellation, and app isolation")
    return test_path, trial


def verify_budget_reservations(port, peer, stop_worker, start_worker, query_db, hold_budget, receiver, check_rollout,
                               hold_worker_slots):
    session, account, project = provision(port)
    query = f"?account_id={account}"
    base = f"/v1/projects/{project}"
    estimate, run_cap = 1_250_000, 3_000_000
    empty = {"inputs": {}}
    draft, _ = call(port, base + "/workflows" + query, 201, method="POST", headers=session, body={
        "schema": 1, "title": "Paid workflow fixture", "max_usd_nanos": 8_000_000, "inputs": {},
        "nodes": [{"id": "generate", "title": "Generate", "kind": "image", "model": "runtime_image",
                   "max_usd_nanos": run_cap, "settings": {
                       "prompt": {"source": "fixed", "value": "A synthetic paid image"},
                       "aspect_ratio": {"source": "fixed", "value": "4:3"},
                   }}],
        "outputs": {"image": "generate"},
    })
    manage = base + f"/workflows/{draft['id']}"
    runs = base + f"/workflow-endpoints/{draft['id']}/runs"
    trial, _ = call(port, manage + "/test-runs" + query, 202, method="POST", body={**empty, "revision": draft["revision"]},
                    headers={**session, "Idempotency-Key": token_hex(16)})
    wait_for_run(port, trial, session)
    call(port, manage + "/versions" + query, 200, method="POST", headers=session,
         body={"revision": draft["revision"], "active_version": None})
    app, _ = call(port, manage + "/apps" + query, 201, method="POST", headers=session,
                  body={"name": "Paid workflow app", "days": 1, "scopes": ["runs:submit", "runs:read", "runs:cancel"]})
    auth = {"Authorization": "Bearer " + app["secret"]}
    project_policy = "/v1/routing-policy" + query + f"&project_id={project}"
    organization_policy = "/v1/routing-policy" + query
    client_budget = base + f"/client-budgets/{app['app_id']}" + query

    def set_caps(revision, total_limit, app_limit):
        policy = {"expected_revision": revision, "policy": {"monthly_estimated_budget_usd_nanos": total_limit}}
        for path in (organization_policy, project_policy):
            candidate = policy
            if path == project_policy:
                candidate = {**policy, "policy": {**policy["policy"], "allowed_providers": ["self_hosted"]}}
            call(port, path, 200, method="PUT", headers=session, body=candidate)
        call(port, client_budget, 200, method="PUT", headers=session,
             body={"expected_revision": revision, "limit_usd_nanos": app_limit})

    def assert_on_hold(total, application):
        for path in (organization_policy, project_policy):
            report, _ = call(peer, path, 200, headers=session)
            assert report["monthly_budget"]["reserved_usd_nanos"] == str(total)
        report, _ = call(peer, client_budget, 200, headers=session)
        assert report["reserved_usd_nanos"] == str(application)

    set_caps(0, estimate + run_cap, run_cap)
    assert_on_hold(estimate, 0)
    spec, _ = call(port, runs.removesuffix("/runs") + "/openapi.json", 200, headers=auth)
    stop_worker()
    attempts = [{**auth, "Idempotency-Key": token_hex(16)} for _ in range(2)]
    with ThreadPoolExecutor(max_workers=2) as threads:
        pending = [threads.submit(call, target, runs, (202, 409), method="POST", body=empty, headers=headers)
                   for target, headers in zip((port, peer), attempts)]
        outcomes = [future.result() for future in pending]
    (denied,) = [index for index, (payload, _) in enumerate(outcomes) if "error" in payload]
    error, response = outcomes[denied]
    verify_json_response(spec, spec["paths"][runs]["post"], 409, error, response)
    assert error["error"]["code"] == "monthly_budget_exceeded"
    receipt, _ = outcomes[1 - denied]
    assert_on_hold(estimate + run_cap, run_cap)
    check_rollout(run_cap)
    replay, _ = call(peer, runs, 200, method="POST", body=empty, headers=attempts[1 - denied])
    assert replay == {**receipt, "replayed": True}

    standalone, _ = call(port, base + "/service-principals" + query, 201, method="POST", headers=session,
                         body={"name": "Shared budget check", "scopes": ["requests:submit"]})
    direct = {"Authorization": "Bearer " + standalone["token"]}
    quote, _ = call(port, "/v1/quotes/scoped", 200, method="POST", headers=direct,
                    body={"model": "runtime_image", "workload": {"kind": "image", "count": 1, "aspect_ratio": "4:3"}})
    blocked, _ = call(peer, "/v1/requests", 409, method="POST", body={"quote_id": quote["id"], "input": quote["input"]},
                      headers={**direct, "Idempotency-Key": token_hex(16)})
    assert blocked["error"]["code"] == "monthly_budget_exceeded"
    call(peer, receipt["status_url"] + "/cancel", 204, method="POST", headers=auth)
    assert_on_hold(estimate, 0)

    accepted, _ = call(peer, runs, 202, method="POST", body=empty, headers=attempts[denied])
    start_worker()
    finished, _ = wait_for_run(peer, accepted, auth)
    assert len(finished["steps"]) == 1
    assert_on_hold(estimate * 2, estimate)
    print("PASS shared workflow budget across replicas and native requests, receipt replay, cancellation, and child transfer")

    set_caps(1, 7_000_000, 7_000_000)
    stop_worker()
    crossing, _ = call(port, runs, 202, method="POST", body=empty, headers={**auth, "Idempotency-Key": token_hex(16)})
    assert_on_hold(estimate * 2 + run_cap, estimate + run_cap)
    run_id = crossing["id"]
    query_db("UPDATE workflow_runs SET budget_month_start = budget_month_start - interval '1 month' WHERE id = :'run'",
             run=run_id)
    assert_on_hold(estimate * 2, estimate)
    start_worker()
    crossover, _ = wait_for_run(port, crossing, auth)
    (step,) = crossover["steps"]
    pinned = query_db(
        "SELECT (r.workflow_budget_month_start = w.budget_month_start)::text || '|' || w.budget_remaining_usd_nanos "
        "FROM gateway_requests r CROSS JOIN workflow_runs w WHERE w.id = :'run' AND r.id = :'child'",
        run=run_id, child=step["request"],
    )
    assert pinned == f"true|{run_cap - estimate}"
    assert_on_hold(estimate * 2, estimate)
    print("PASS newly admitted child keeps the original UTC month and releases unused hold after completion")

    subscription, _ = call(port, base + "/webhooks" + query, 201, method="POST", headers=session, body={
        "name": "Expired workflow fixture", "destination_id": "fixture", "workflow_app_id": app["app_id"],
    })
    listing = base + "/webhook-deliveries"

    def wait_for_db(statement, message, **values):
        until = monotonic() + 10
        while monotonic() < until:
            if query_db(statement, **values) == "t":
                return
            sleep(0.1)
        raise RuntimeError(message)

    def enqueue():
        admitted, _ = call(port, runs, 202, method="POST", body=empty,
                           headers={**auth, "Idempotency-Key": token_hex(16)})
        return admitted

    def expect_expired(admitted):
        run = admitted["id"]
        wait_for_db("SELECT status = 'expired' FROM workflow_runs WHERE id = :'run'",
                    "The worker did not expire the due run", run=run)
        status, headers = call(peer, admitted["status_url"], 200, headers=auth)
        assert status["status"] == "expired" and status["error"] == "run_deadline_exceeded"
        assert status.get("outputs") is None
        call(port, admitted["status_url"], 304, headers={**auth, "If-None-Match": headers["ETag"]})
        no_child = query_db("SELECT NOT EXISTS (SELECT 1 FROM gateway_requests WHERE actor_id = :'run')", run=run)
        assert no_child == "t"
        assert_on_hold(estimate * 2, estimate)
        delivery = verify_signed_delivery(port, listing, query, session, receiver, subscription, "workflow.run.expired", {
            "run_id": run, "workflow_id": draft["id"], "project_id": project, "workflow_version": 1,
            "status": "expired", "fetch_url": admitted["status_url"],
        })
        assert delivery["workflow_run_id"] == run

    stop_worker()
    unreadable = enqueue()
    query_db("UPDATE workflow_runs SET content = decode('00','hex'), deadline = now() - interval '1 second', "
             "next_at = now() - interval '1 second' WHERE id = :'run'", run=unreadable["id"])
    assert_on_hold(estimate * 2 + run_cap, estimate + run_cap)
    operations, _ = call(port, "/v1/operations/queues" + query + f"&project_id={project}", 200, headers=session)
    assert operations["workflows"]["queued"]["count"] == operations["workflows"]["overdue"] == 1
    assert operations["workflows"]["due_queued"] == 0
    start_worker()
    expect_expired(unreadable)
    preserved = query_db("SELECT encode(content, 'hex') FROM workflow_runs WHERE id = :'run'", run=unreadable["id"])
    assert preserved == "00"
    print("PASS unreadable checkpoint still expires, releases budget, and produces one signed terminal event")

    stop_worker()
    overdue = enqueue()
    query_db("UPDATE workflow_runs SET next_at = now() + interval '1 hour' WHERE id = :'run'", run=overdue["id"])
    assert_on_hold(estimate * 2 + run_cap, estimate + run_cap)
    with hold_worker_slots() as (first, second):
        query_db("UPDATE workflow_runs SET deadline = now() - interval '1 second', "
                 "next_at = now() - interval '1 second' WHERE id = :'run'", run=overdue["id"])
        wait_for_db(
            "SELECT status = 'expired' AND (SELECT count(*) FROM workflow_runs "
            "WHERE id IN (:'first', :'second') AND status = 'queued' AND lease = 1) = 2 "
            "FROM workflow_runs WHERE id = :'run'",
            "The deadline did not release the run while both worker slots were occupied",
            run=overdue["id"], first=first, second=second,
        )
        assert_on_hold(estimate * 2, estimate)
        assert query_db("SELECT count(*) FROM gateway_webhook_deliveries WHERE workflow_run_id = :'run'",
                        run=overdue["id"]) == "1"
    expect_expired(overdue)
    print("PASS deadline expiration releases the budget and records one signed event with both worker slots occupied")

    stop_worker()
    delayed = enqueue()
    with hold_budget(account):
        query_db("UPDATE workflow_runs SET deadline = clock_timestamp() + interval '5 seconds' WHERE id = :'run'",
                 run=delayed["id"])
        start_worker()
        wait_for_db("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE wait_event = 'advisory' "
                    "AND query LIKE '%gateway_monthly_budget:%')",
                    "Child admission did not reach the held budget lock")
        wait_for_db("SELECT next_at = deadline AND clock_timestamp() >= deadline FROM workflow_runs WHERE id = :'run'",
                    "The lease did not end at the run deadline", run=delayed["id"])
    expect_expired(delayed)
    print("PASS a child blocked on budget cannot commit after its lease reaches the run deadline")

    def set_credits(credits):
        # Freeze refill while checking exact costs. A debit must still use the real SQL function.
        query_db("UPDATE gateway_submission_limits SET requests_per_minute = 60, credit = :'balance', "
                 "refilled_at = clock_timestamp() + interval '1 hour' WHERE account_id = :'account'",
                 balance=credits * 60_000_000, account=account)

    stop_worker()
    set_credits(1)
    quote, _ = call(port, "/v1/quotes/scoped", 200, method="POST", headers=direct,
                    body={"model": "runtime_image", "workload": {"kind": "image", "count": 1, "aspect_ratio": "4:3"}})
    direct_receipt, _ = call(port, "/v1/requests", 202, method="POST",
                             body={"quote_id": quote["id"], "input": quote["input"]},
                             headers={**direct, "Idempotency-Key": token_hex(16)})
    draft_denied, draft_headers = call(peer, manage + "/test-runs" + query, 429, method="POST",
                                      body={**empty, "revision": draft["revision"]},
                                      headers={**session, "Idempotency-Key": token_hex(16)})
    assert draft_denied["error"]["code"] == "submission_rate_limit" and int(draft_headers["Retry-After"]) == 1
    assert_on_hold(estimate * 3, estimate)

    set_caps(2, 10_000_000, 10_000_000)
    set_credits(1)
    attempts = [{**auth, "Idempotency-Key": token_hex(16)} for _ in range(2)]
    with ThreadPoolExecutor(max_workers=2) as threads:
        pending = [threads.submit(call, target, runs, (202, 429), method="POST", body=empty, headers=headers)
                   for target, headers in zip((port, peer), attempts)]
        outcomes = [future.result() for future in pending]
    (denied,) = [index for index, (payload, _) in enumerate(outcomes) if "error" in payload]
    error, response = outcomes[denied]
    verify_json_response(spec, spec["paths"][runs]["post"], 429, error, response, expected_headers=("Retry-After",))
    assert error["error"]["code"] == "submission_rate_limit" and int(response["Retry-After"]) == 1
    receipt, _ = outcomes[1 - denied]
    replay, _ = call(port, runs, 200, method="POST", body=empty, headers=attempts[1 - denied])
    assert replay == {**receipt, "replayed": True}
    assert_on_hold(estimate * 3 + run_cap, estimate + run_cap)

    start_worker()
    wait_for_db("SELECT status = 'queued' AND revision > 1 AND next_at > now() FROM workflow_runs WHERE id = :'run'",
                "The workflow worker did not defer its generation step at the quota", run=receipt["id"])
    assert query_db("SELECT NOT EXISTS (SELECT 1 FROM gateway_requests WHERE actor_id = :'run')",
                    run=receipt["id"]) == "t"
    set_credits(1)
    finished, _ = wait_for_run(peer, receipt, auth)
    assert len(finished["steps"]) == 1
    assert query_db("SELECT credit FROM gateway_submission_limits WHERE account_id = :'account'",
                    account=account) == "0"
    assert_on_hold(estimate * 4, estimate * 2)
    print("PASS shared submission quota for native requests, draft tests, concurrent published runs, replay, and deferred generation")

    status = receipt["status_url"]
    console = status + query + "&include_execution=true"
    child = finished["steps"][0]["request"]

    def before_retirement():
        view, headers = call(port, console, 200, headers=session)
        assert any("no-store" in value for value in headers.get_all("Cache-Control", [])), headers
        assert not view["execution"]["evidence_expired"]
        (node,) = view["execution"]["nodes"]
        assert node["status"] == "succeeded" and node["request"] == child
        return node

    original_step = before_retirement()
    app_view, _ = call(port, status + "?include_execution=true", 200, headers=auth)
    assert "execution" not in app_view
    sibling, _ = call(port, "/v1/projects" + query, 201, method="POST", headers=session,
                      body={"name": "Another history scope"})
    call(port, console.replace(f"/projects/{project}/", f"/projects/{sibling['id']}/"), 404, headers=session)

    # Reproduce the state left by the retention worker without waiting for its minute-long sweep.
    query_db("UPDATE workflow_runs SET content = NULL WHERE id = :'run'", run=receipt["id"])
    retained, _ = call(peer, console, 200, headers=session)
    assert retained["execution"]["evidence_expired"] and "outputs" not in retained and not retained["steps"]
    (step,) = retained["execution"]["nodes"]
    assert step == original_step and step["title"] == "Generate"
    assert step["request_status"] == "succeeded" and step["provider"] == "self_hosted"
    assert step["admitted_at"] and step["finished_at"]
    print("PASS the session-only execution timeline retains its scoped request trace after run content expires")

    parents = base + "/workflow-activity/parents/"
    parent, _ = call(port, parents + child + query, 200, headers=session)
    assert parent == {"workflow_id": draft["id"], "run_id": receipt["id"], "title": "Paid workflow fixture"}
    direct_parent, _ = call(port, parents + direct_receipt["id"] + query, 200, headers=session)
    assert direct_parent is None
    sibling_parents = f"/v1/projects/{sibling['id']}/workflow-activity/parents/"
    sibling_parent, _ = call(port, sibling_parents + child + query, 200, headers=session)
    assert sibling_parent is None
    call(port, parents + child + query, 401, headers=auth)
    print("PASS a retained request finds its parent only through the matching project session")

    def expect_success_event(admitted, reference=None):
        data = {
            "run_id": admitted["id"], "workflow_id": draft["id"], "project_id": project, "workflow_version": 1,
            "status": "succeeded", "fetch_url": admitted["status_url"],
        }
        if reference is not None:
            data["client_ref"] = reference
        delivery = verify_signed_delivery(port, listing, query, session, receiver, subscription,
                                          "workflow.run.succeeded", data)
        assert delivery["workflow_run_id"] == admitted["id"]

    expect_success_event(receipt)

    client_field = spec["paths"][runs]["post"]["requestBody"]["content"]["application/json"]["schema"]["properties"]
    assert client_field["client_ref"]["type"] == "string"
    references = ("studio-alpha", "studio-beta")

    def client_limit(reference, revision, limit):
        path = base + f"/client-budgets/{reference}" + query
        call(port, path, 200, method="PUT", headers=session,
             body={"expected_revision": revision, "limit_usd_nanos": limit})

    def budget_for(reference):
        view, _ = call(peer, base + f"/client-budgets/{reference}" + query, 200, headers=session)
        return int(view["reserved_usd_nanos"])

    # Disable shared org and project caps so the concurrent requests use their client and app locks.
    set_caps(3, None, estimate * 2 + run_cap + 100_000)
    for reference in references:
        client_limit(reference, 0, run_cap)
    set_credits(20)
    stop_worker()
    bodies = [{**empty, "client_ref": reference} for reference in references]
    attempts = [{**auth, "Idempotency-Key": token_hex(16)} for _ in references]
    with ThreadPoolExecutor(max_workers=2) as threads:
        pending = [threads.submit(call, target, runs, (202, 409), method="POST", body=body, headers=headers)
                   for target, body, headers in zip((port, peer), bodies, attempts)]
        results = [future.result()[0] for future in pending]
    (winner,) = [index for index, result in enumerate(results) if "status_url" in result]
    loser = 1 - winner
    assert results[loser]["error"]["code"] == "monthly_budget_exceeded"
    assert budget_for(app["app_id"]) == estimate * 2 + run_cap
    assert budget_for(references[winner]) == run_cap and budget_for(references[loser]) == 0
    replay, _ = call(peer, runs, 200, method="POST", body=bodies[winner], headers=attempts[winner])
    assert replay == {**results[winner], "replayed": True}
    changed, _ = call(peer, runs, 409, method="POST", body=bodies[loser], headers=attempts[winner])
    assert changed["error"]["code"] == "conflict"
    call(peer, runs, 400, method="POST", body={**empty, "client_ref": "customer with spaces"}, headers=attempts[winner])

    start_worker()
    first, _ = wait_for_run(port, results[winner], auth)
    expect_success_event(results[winner], references[winner])
    assert budget_for(app["app_id"]) == estimate * 3
    assert budget_for(references[winner]) == estimate
    set_caps(4, None, 20_000_000)
    client_limit(references[loser], 1, run_cap - 1)
    denied, _ = call(peer, runs, 409, method="POST", body=bodies[loser], headers=attempts[loser])
    assert denied["error"]["code"] == "monthly_budget_exceeded"
    client_limit(references[loser], 2, run_cap)
    accepted, _ = call(peer, runs, 202, method="POST", body=bodies[loser], headers=attempts[loser])
    second, _ = wait_for_run(port, accepted, auth)
    expect_success_event(accepted, references[loser])
    assert budget_for(app["app_id"]) == estimate * 4

    requests = [first, second]
    clients = [references[winner], references[loser]]
    for item, reference in zip(requests, clients):
        assert item["client_ref"] == reference and budget_for(reference) == estimate
        child = item["steps"][0]["request"]
        filters = query + f"&project_id={project}&client_ref={reference}"
        history, _ = call(port, "/v1/requests" + filters, 200, headers=session)
        assert [entry["id"] for entry in history["requests"]] == [child]
        usage, _ = call(peer, "/v1/usage" + filters, 200, headers=session)
        assert usage["totals"]["requests"] == 1 and usage["totals"]["estimated_usd_nanos"] == str(estimate)
        parent, _ = call(port, parents + child + query, 200, headers=session)
        assert parent["run_id"] == item["id"]
        timeline, _ = call(peer, runs + f"/{item['id']}" + query + "&include_execution=true", 200, headers=session)
        assert timeline["execution"]["nodes"][0]["request"] == child

    bound, _ = call(port, base + "/service-principals" + query, 201, method="POST", headers=session,
                    body={"name": "One workflow customer", "scopes": ["requests:read", "usage:read"],
                          "client_ref": clients[0]})
    credential = {"Authorization": "Bearer " + bound["token"]}
    history, _ = call(peer, "/v1/requests", 200, headers=credential)
    assert [entry["id"] for entry in history["requests"]] == [requests[0]["steps"][0]["request"]]
    usage, _ = call(peer, "/v1/usage", 200, headers=credential)
    assert usage["client_ref"] == clients[0] and usage["totals"]["requests"] == 1
    call(peer, f"/v1/usage?client_ref={clients[1]}", 404, headers=credential)
    print("PASS shared workflow key attributes customers in signed webhook retries, traces, and usage under app and client budgets")


class ComputeFixture(BaseHTTPRequestHandler):
    def json_reply(self, status, payload):
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass

    def do_GET(self):
        if self.path != "/v1/models":
            return self.json_reply(404, {})
        models = [] if self.server.throttled else [{"id": self.server.model}]
        self.json_reply(200, {"data": models})

    def do_POST(self):
        if self.path != "/v1/chat/completions":
            return self.json_reply(404, {})
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if payload["model"] != self.server.model or payload["stream"]:
            return self.json_reply(400, {"error": {"message": "unexpected fixture request"}})
        self.server.requests += 1
        self.server.trace_headers.append((self.headers.get("traceparent"),
                                          self.headers.get("tracestate"), self.headers.get("baggage")))
        if self.server.throttled:
            return self.json_reply(429, {"error": {"message": "fixture capacity busy"}})
        self.json_reply(200, {
            "id": "completion-" + self.server.label,
            "model": self.server.model,
            "choices": [{"message": {"content": self.server.label}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 2, "completion_tokens": 1},
        })


@contextmanager
def compute_profile(directory):
    catalog = json.loads((FIXTURES / "runtime-catalog.json").read_bytes())
    bindings = []
    probes = []
    servers = {}
    location = {"cloud": "local", "region": "workstation"}
    with ExitStack() as stack:
        for supplier, group, upstream, replicas in (
            ("self_hosted", "fixture-chat", "local-chat", ("primary", "standby")),
            ("openrouter", "fixture-partner", "partner-chat", ("partner",)),
        ):
            catalog["offers"].append({
                "id": group, "provider": supplier, "model": group, "adapter": "openai_compatible_chat",
                "local_binding": group, "upstream_model": upstream,
                "pricing": {"kind": "text", "usd_nanos_per_1000_input_tokens": 0,
                            "usd_nanos_per_1000_output_tokens": 0},
            })
            targets = []
            for index, label in enumerate(replicas):
                server = stack.enter_context(HTTPServer((HOST, 0), ComputeFixture))
                server.label = label
                server.model = upstream
                server.throttled = label == "primary"
                server.requests = 0
                server.trace_headers = []
                thread = Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
                thread.start()
                stack.callback(thread.join, 2)
                stack.callback(server.shutdown)
                servers[label] = server
                targets.append({
                    "id": label, "location": location, "zone": "test", "priority": index,
                    "endpoint": f"http://{HOST}:{server.server_port}/v1/chat/completions",
                    "retry_on_rate_limit": server.throttled,
                })
                probes.append({"provider": supplier, "stack": group, "target": label, "id": "fixture-" + label})
            bindings.append({"name": group, "allowed_locations": [location], "targets": targets})
        catalog_path = directory / "operations-catalog.json"
        bindings_path = directory / "operations-bindings.json"
        catalog_path.write_text(json.dumps(catalog))
        bindings_path.write_text(json.dumps({"version": 1, "cell": "fixture", "bindings": bindings}))
        yield {"catalog": str(catalog_path), "bindings": str(bindings_path), "probes": probes}, servers


def verify_operations(directory, env, psql, pg_env, api_port, worker_port, account, project, capacity_token,
                      session, profile, servers):
    project_path = f"/v1/projects/{project}/service-principals?account_id={account}"
    principal, _ = call(api_port, project_path, 201, method="POST", headers=session, body={
        "name": "Operations compute smoke", "scopes": ["requests:submit", "requests:read"],
    })
    bearer = {"Authorization": "Bearer " + principal["token"]}
    chat_ids = {}
    for model, expected in (("fixture-chat", "standby"), ("fixture-partner", "partner")):
        result, headers = call(api_port, "/v1/chat/completions", 200, method="POST",
                               headers={**bearer, **CALLER_CONTEXT}, body={
            "model": model, "messages": [{"role": "user", "content": "Synthetic operations fixture"}],
            "max_tokens": 8,
        })
        assert result["choices"][0]["message"]["content"] == expected
        request_id = headers["TokenCraft-Request-Id"]
        chat_ids[expected] = request_id
        parent = provider_traceparent(api_port, request_id, bearer)
        observed = ("primary", "standby") if expected == "standby" else ("partner",)
        for label in observed:
            assert servers[label].trace_headers == [(parent, None, None)]
    assert {name: server.requests for name, server in servers.items()} == {"primary": 1, "standby": 1, "partner": 1}
    warnings, _ = call(api_port, f"/v1/requests?diagnostic=warnings&status=succeeded&trace_id={CALLER_TRACE_ID}",
                       200, headers=bearer)
    warned_ids = {request["id"] for request in warnings["requests"]}
    assert chat_ids["standby"] in warned_ids and chat_ids["partner"] not in warned_ids
    print("PASS configured chat targets retain the scoped OTLP parent across compute failover")

    password = token_hex(32)
    role = directory / "operations-role.sql"
    role.write_text("\\getenv monitor_password TOKENCRAFT_OPERATIONS_FIXTURE_PASSWORD\n"
                    "CREATE ROLE tokencraft_monitor_smoke LOGIN PASSWORD :'monitor_password';\n")
    command(*psql, "-v", "monitor_role=tokencraft_monitor_smoke", "-f", role,
            "-f", MONITOR / "install.sql", cwd=directory,
            env=dict(pg_env, TOKENCRAFT_OPERATIONS_FIXTURE_PASSWORD=password))
    service_file = directory / "monitor-libpq.conf"
    service_file.write_text(f"[monitor_fixture]\nhost={HOST}\nport={pg_env['PGPORT']}\ndbname=postgres\n"
                            "user=tokencraft_monitor_smoke\nsslmode=disable\n")
    monitor_env = dict(env, PGSERVICEFILE=str(service_file), PGPASSWORD=password,
                       TOKENCRAFT_MONITOR_PASSWORD=token_hex(32), TOKENCRAFT_CAPACITY_TOKEN=capacity_token,
                       PATH=f"{Path(psql[0]).parent}{os.pathsep}{env['PATH']}")
    rights = command(*psql, "-c", """
        SELECT NOT has_table_privilege('tokencraft_monitor_smoke', 'public.gateway_requests', 'SELECT')
           AND NOT has_any_column_privilege('tokencraft_monitor_smoke', 'public.gateway_provider_runs', 'SELECT')
           AND has_function_privilege('tokencraft_monitor_smoke',
               'tokencraft_monitor.sample(jsonb,jsonb,jsonb,integer,text,text)', 'EXECUTE')
    """, cwd=directory, env=pg_env)
    assert rights == "t"
    config = directory / "monitor.json"
    config.write_text(json.dumps({
        "version": 1, "interval_seconds": 15,
        "cells": [{"id": "fixture", "providers": ["self_hosted", "openrouter"], "pg_service": "monitor_fixture",
                   "compute_profile": profile,
                   "scopes": [{"id": "workflow-project", "account": account, "project": project},
                              {"id": "other-project", "account": account, "project": "gprj_other"}]}],
        "targets": [
            {"id": identifier, "cell": "fixture", "kind": kind,
             "url": f"http://{HOST}:{port}/internal/capacity", "token_env": "TOKENCRAFT_CAPACITY_TOKEN"}
            for identifier, kind, port in (("fixture-api", "gateway", api_port), ("fixture-worker", "worker", worker_port))
        ],
    }))
    port = loopback_port()
    credentials = b64encode(("operator:" + monitor_env["TOKENCRAFT_MONITOR_PASSWORD"]).encode()).decode()
    auth = {"Authorization": "Basic " + credentials}
    with service("operations", [sys.executable, "-B", MONITOR / "monitor.py", "--config", config,
                 "--state-dir", directory / "monitor-state", "--port", str(port)], directory, monitor_env,
                 lambda: call(port, "/healthz", 200, headers=auth)[0]["collector_fresh"]):
        call(port, "/", 401)
        assert b"TokenCraft Operations" in call(port, "/", 200, headers=auth)[0]
        exported, _ = call(port, "/metrics", 200, headers=auth)
        assert b"tokencraft_monitor_workflow_outcome_succeeded" in exported
        assert b'tokencraft_monitor_compute_rate_limited{cell="fixture",target="fixture-primary"} 1' in exported
        overview, _ = call(port, "/api/overview", 200, headers=auth)
        snapshot = overview["snapshot"]
        worker = snapshot["targets"]["fixture-worker"]
        assert worker["status"] == "up" and worker["signals"]["checks"]["migrations"] == "ready"
        assert snapshot["targets"]["fixture-primary"]["status"] == "degraded"
        assert snapshot["targets"]["fixture-primary"]["signals"]["missing_models"] == 1
        assert all(snapshot["targets"]["fixture-" + name]["status"] == "up" for name in ("standby", "partner"))
        cell = snapshot["cells"]["fixture"]
        scopes = cell["scopes"]
        measured = scopes["workflow-project"]["sample"]
        assert measured["compute"]["fixture-primary"]["rate_limited"] == 1
        assert measured["compute"]["fixture-primary"]["response_started"] == 0
        for name in ("standby", "partner"):
            target = measured["compute"]["fixture-" + name]
            assert target["attempted"] == target["response_started"] == 1 and target["p95_response_headers_seconds"] >= 0
        assert measured["provider"]["openrouter"]["succeeded"] == 1
        assert scopes["workflow-project"]["sample"]["workflow_outcomes"]["succeeded"] > 0
        assert scopes["other-project"]["sample"]["workflow_outcomes"]["sampled"] == 0
        assert account not in json.dumps(snapshot) and project not in json.dumps(snapshot)
        points, _ = call(port, "/api/history?metric=workflow_p95&id=fixture&scope=workflow-project",
                         200, headers=auth)
        assert any(point["value"] is not None for point in points["points"])
    print("PASS private Operations console, scoped PostgreSQL history, model availability, and compute fallback across providers")


def rehearse(gateway, admin, directory, *, verification=None):
    env = {"PATH": os.environ["PATH"], "LANG": "C", "RUST_LOG": "warn"}
    pg = postgres_bin()
    data, password_file = directory / "pg", directory / "password"
    password, runtime_password = token_hex(24), token_hex(24)
    password_file.write_text(password + "\n")
    command(pg / "initdb", "-D", data, "--no-locale", "--encoding=UTF8", "--wal-segsize=1",
            "--auth=scram-sha-256", "--username=tokencraft", "--pwfile", password_file, cwd=directory, env=env)
    password_file.unlink()
    db_port = loopback_port()
    pg_env = dict(env, PGHOST=HOST, PGPORT=str(db_port), PGUSER="tokencraft", PGPASSWORD=password)

    def db_ready():
        return subprocess.run([pg / "pg_isready", "-q", "-t", "1", "-d", "postgres"],
                              env=pg_env, timeout=3, check=False).returncode == 0

    with ExitStack() as stack:
        stack.enter_context(service("postgres", [pg / "postgres", "-D", data, "-h", HOST, "-p", str(db_port),
            "-c", "unix_socket_directories="], directory, env, db_ready, shutdown=signal.SIGINT))
        owner = dict(env, FOUNDATION_ENV="development",
                     DATABASE_URL=f"postgresql://tokencraft:{password}@{HOST}:{db_port}/postgres")
        psql = (pg / "psql", "-XAtw", "-d", "postgres", "-v", "ON_ERROR_STOP=1")
        def schema_status(settings, state, *, ready=False):
            result = subprocess.run([admin, "schema-status"], cwd=directory, env=settings,
                                    capture_output=True, text=True, timeout=15)
            assert result.returncode == (0 if ready else 1), result.stderr
            lines = dict(line.split(": ", 1) for line in result.stdout.strip().splitlines())
            assert lines["schema status"] == state, lines
            return lines

        empty = schema_status(owner, "empty")
        assert empty["pending migrations"].split(",")[-1] == empty["binary schema"]
        command(*psql, "-c", "CREATE TABLE public.existing_fixture (id integer)", cwd=directory, env=pg_env)
        assert "pending migrations" not in schema_status(owner, "unmanaged")
        rejected = subprocess.run([admin, "initialize-demo"], cwd=directory, env=owner,
                                  capture_output=True, text=True, timeout=15)
        assert rejected.returncode != 0 and "requires an empty public schema" in rejected.stderr
        ledger = command(*psql, "-c", "SELECT to_regclass('public._sqlx_migrations') IS NULL",
                         cwd=directory, env=pg_env)
        assert ledger == "t"
        command(*psql, "-c", "DROP TABLE public.existing_fixture", cwd=directory, env=pg_env)
        assert command(admin, "initialize-demo", cwd=directory, env=owner) == "initialized empty demo schema"
        command(pg / "psql", "-Xqw", "-d", "postgres", "-v", "runtime_role=tokencraft_runtime",
                "-f", FIXTURES / "create-runtime-role.sql", "-f", ROOT / "deploy/postgres/grant-runtime.sql",
                cwd=directory, env=dict(pg_env, TOKENCRAFT_RUNTIME_APP_PASSWORD=runtime_password))
        api_port = loopback_port()
        capacity_token = token_hex(32)
        profile, compute = stack.enter_context(compute_profile(directory))
        settings = dict(env, FOUNDATION_ENV="development", TOKENCRAFT_PROCESS_ROLE="api",
            DATABASE_URL=f"postgresql://tokencraft_runtime:{runtime_password}@{HOST}:{db_port}/postgres",
            APP_BASE_URL=f"http://localhost:{api_port}", API_GATEWAY_PORT=str(api_port),
            COOKIE_DOMAIN="localhost", COOKIE_SECURE="false", SESSION_SECRET=token_hex(32),
            TOKENCRAFT_CAPACITY_TOKEN=capacity_token,
            TOKENCRAFT_CORE_CATALOG=profile["catalog"], TOKENCRAFT_LOCAL_CHAT_BINDINGS=profile["bindings"],
            TOKENCRAFT_DEMO_PAYLOAD_KEY=token_hex(32),
            TOKENCRAFT_WORKFLOW_KEY=token_hex(32))
        compatible = command(admin, "initialize-demo", cwd=directory, env=settings)
        assert compatible == "existing demo schema is compatible"
        current = schema_status(settings, "current", ready=True)
        assert current["database schema"] == current["binary schema"] == empty["binary schema"]
        assert "pending migrations" not in current
        print("PASS startup initializes an empty schema and schema status verifies the ledger with the runtime login")
        with service("setup-api", [gateway], directory, settings, lambda: ready(api_port)):
            session, account, project = provision(api_port)
            verify_cache_policy(api_port, session, account, project)
            contract, _ = call(api_port, "/openapi.json", 200)
            for path in ("/v1/setup/status", "/v1/operations/queues", "/v1/operations/artifact-publication"):
                assert path not in contract["paths"]
                call(api_port, f"{path}?account_id={account}", 404, headers=session)

        if verification is not None:
            verification(gateway, directory, settings, session, account, project)
            return

        settings["TOKENCRAFT_ENABLE_LEGACY_OPERATIONS"] = "true"
        scoped_catalog = directory / "scoped-catalog.json"
        catalog = json.loads(Path(profile["catalog"]).read_bytes())
        catalog["offers"].append({
            **catalog["offers"][0], "id": "a_private_image", "model": "a_private_image", "provider": "fixture_private",
        })
        catalog["connection_access"] = {"fixture_private": {"account": account, "projects": [project]}}
        scoped_catalog.write_text(json.dumps(catalog))
        profile["catalog"] = str(scoped_catalog)
        settings["TOKENCRAFT_CORE_CATALOG"] = str(scoped_catalog)
        media_port, token = loopback_port(), token_hex(32)
        delivery = stack.enter_context(service("delivery", [sys.executable, "-B", MEDIA / "server.py", "--pack", "delivery",
            "--host", HOST, "--port", str(media_port), "--state", directory / "delivery"], directory,
            dict(env, TOKENCRAFT_MEDIA_SERVICE_TOKEN=token), lambda: ready(media_port)))
        registry = directory / "bindings.json"
        registry.write_text(json.dumps({"version": 1, "bindings": [{
            "name": "fixture-delivery", "revision": "v1", "origin": f"http://{HOST}:{media_port}/",
            "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_FIXTURE_DELIVERY", "manifest": str(MEDIA / "delivery.manifest.json"),
            "account": account, "projects": [project],
        }]}))
        settings.update(TOKENCRAFT_WORKFLOW_TOOL_BINDINGS=str(registry), TOKENCRAFT_CONNECTOR_TOKEN_FIXTURE_DELIVERY=token)
        receiver = stack.enter_context(webhook_receiver())
        webhooks = directory / "webhooks.json"
        webhooks.write_text(json.dumps({"destinations": {"fixture": f"http://{HOST}:{receiver.server_port}/events"}}))
        settings.update(TOKENCRAFT_DEMO_WEBHOOK_CONFIG=str(webhooks), TOKENCRAFT_DEMO_WEBHOOK_KEY=token_hex(32))
        stack.enter_context(service("api", [gateway], directory, settings, lambda: ready(api_port)))
        peer_port = loopback_port()
        stack.enter_context(service("peer-api", [gateway], directory,
            dict(settings, API_GATEWAY_PORT=str(peer_port), APP_BASE_URL=f"http://localhost:{peer_port}"),
            lambda: ready(peer_port)))
        with ExitStack() as workers:
            def query_db(statement, **values):
                args = [argument for key, value in values.items() for argument in ("-v", f"{key}={value}")]
                # psql substitutes variables in a script, rather than in a single -c argument.
                script = directory / "assert-budget.sql"
                script.write_text(statement + ";\n")
                return command(*psql, *args, "-f", script, cwd=directory, env=pg_env)

            @contextmanager
            def hold_budget(account):
                with subprocess.Popen([*psql, "-q", "-v", f"account={account}"], cwd=directory, env=pg_env,
                                      stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True) as owner:
                    try:
                        owner.stdin.write("SET statement_timeout = '5s';\nBEGIN;\n"
                                          "SELECT 'held' FROM pg_advisory_xact_lock("
                                          "hashtextextended('gateway_monthly_budget:' || :'account', 0));\n")
                        owner.stdin.flush()
                        assert owner.stdout.readline().strip() == "held"
                        yield
                    finally:
                        owner.stdin.close()
                        owner.wait(timeout=5)

            worker_port = None

            def start_worker():
                nonlocal worker_port
                worker_port = loopback_port()
                workers.enter_context(service(f"worker-{worker_port}", [gateway], directory,
                    dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port)),
                    lambda: ready(worker_port)))

            start_worker()

            def check_rollout(sibling_project, digest):
                verify_tool_rollout(gateway, directory, settings, registry, sibling_project, digest)
                verify_worker_binding_scope(gateway, directory, settings, registry, api_port, session, account,
                                            project, sibling_project, query_db)

            def submit_trial(path, body):
                return test_during_tool_pause(api_port, session, path, body, delivery, query_db)

            test_path, trial = verify_workflow(
                api_port, peer_port, session, account, project, workers.close, start_worker, receiver,
                check_rollout, submit_trial)
            verify_project_catalog(api_port, session, account, project)

            def check_generation(step_cap):
                verify_generation_rollout(gateway, directory, settings, step_cap)

            @contextmanager
            def hold_worker_slots():
                delivery.send_signal(signal.SIGSTOP)
                try:
                    receipts = []
                    for _ in range(2):
                        receipt, _ = call(api_port, test_path, 202, method="POST", body=trial,
                                          headers={**session, "Idempotency-Key": token_hex(16)})
                        receipts.append(receipt)
                    start_worker()
                    first, second = (receipt["id"] for receipt in receipts)
                    until = monotonic() + 5
                    while monotonic() < until:
                        if query_db("SELECT count(*) FROM workflow_runs WHERE id IN (:'first', :'second') "
                                    "AND status = 'queued' AND lease = 1", first=first, second=second) == "2":
                            break
                        sleep(0.05)
                    else:
                        raise AssertionError("Both tool calls failed to occupy worker slots")
                    yield first, second
                finally:
                    delivery.send_signal(signal.SIGCONT)
                for receipt in receipts:
                    wait_for_run(api_port, receipt, session)

            verify_budget_reservations(api_port, peer_port, workers.close, start_worker, query_db, hold_budget, receiver,
                                       check_generation, hold_worker_slots)
            verify_operations(directory, env, psql, pg_env, api_port, worker_port, account, project, capacity_token,
                              session, profile, compute)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path, help="compiled api-gateway binary")
    parser.add_argument("admin", type=Path, help="compiled admin-cli binary from the same checkout")
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-workflow-") as temp:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(temp))


if __name__ == "__main__":
    main()
