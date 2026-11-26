"""Exercise a published workflow contract with disposable gateway and delivery worker processes."""

import argparse
from base64 import b64decode, b64encode
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack, contextmanager
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
HOST = "127.0.0.1"


def loopback_port():
    with socket.socket() as listener:
        listener.bind((HOST, 0))
        return listener.getsockname()[1]


def command(*args, cwd, env):
    result = subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise RuntimeError(f"{Path(args[0]).name} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def call(port, path, expected, *, method="GET", body=None, headers=None):
    statuses = (expected,) if isinstance(expected, int) else expected
    fields = {"Origin": f"http://localhost:{port}", **(headers or {})}
    if body is not None and not isinstance(body, bytes):
        fields["Content-Type"] = "application/json"
        body = json.dumps(body).encode()
    connection = HTTPConnection(HOST, port, timeout=5)
    try:
        connection.request(method, path, body, fields)
        result = connection.getresponse()
        if result.status not in statuses:
            raise RuntimeError(f"{method} {path.split('?')[0]} returned {result.status}, expected {expected}")
        data = result.read(2 * 1024 * 1024 + 1)
        if len(data) > 2 * 1024 * 1024:
            raise RuntimeError("The fixture response exceeded the expected size")
        if result.headers.get_content_type() == "application/json":
            data = json.loads(data)
        return data, result.headers
    finally:
        connection.close()


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
            yield
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


def verify_signed_delivery(port, listing, query, session, receiver, subscription, event_type, data):
    secret = b64decode(subscription["signing_secret"].removeprefix("whsec_"))
    original = None
    for _ in range(2):
        headers, raw = receiver.received.get(timeout=10)
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


def verify_request_webhook(port, session, project_path, query, receiver, workflow_subscription):
    registration = {"name": "Native receipt fixture", "destination_id": "fixture"}
    native, _ = call(port, project_path + "/webhooks" + query, 201, method="POST", headers=session, body=registration)
    principal, _ = call(port, project_path + "/service-principals" + query, 201, method="POST", headers=session,
                        body={"name": "Native webhook receipt", "scopes": ["requests:submit", "requests:read"]})
    auth = {"Authorization": "Bearer " + principal["token"]}
    quote, _ = call(port, "/v1/quotes/scoped", 200, method="POST", headers=auth,
                    body={"model": "runtime_image", "workload": {"kind": "image", "count": 1, "aspect_ratio": "4:3"}})
    body = {"quote_id": quote["id"], "input": quote["input"],
            "delivery": {"webhook_subscription_id": workflow_subscription["id"]}}
    invoke = {**auth, "Idempotency-Key": token_hex(16)}
    call(port, "/v1/requests", 404, method="POST", body=body, headers=invoke)
    body["delivery"]["webhook_subscription_id"] = native["id"]
    accepted, _ = call(port, "/v1/requests", 202, method="POST", body=body, headers=invoke)
    delivery = verify_signed_delivery(port, project_path + "/webhook-deliveries", query, session, receiver,
        native, "request.succeeded",
        {"request_id": accepted["id"], "project_id": project_path.rsplit("/", 1)[1],
         "status": "succeeded", "fetch_url": accepted["status_url"]})
    assert delivery["request_id"] == accepted["id"] and "workflow_run_id" not in delivery
    print("PASS ordinary request webhooks retain their signed payload and cannot use an app-only subscription")


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


def verify_workflow(port, peer, session, account, project, stop_worker, restart_worker, receiver, check_rollout):
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
    checked, _ = call(port, test_path, 202, method="POST",
                      headers={**session, "Idempotency-Key": token_hex(16)}, body=trial)
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
    assert paths[paths_run + "/outputs/{name}"]["get"]["x-workflow-required-scope"] == "outputs:read"

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
    verify_request_webhook(port, session, project_path, query, receiver, subscription)
    print("PASS app-scoped terminal webhook signature and retry retain the original metadata-only event")
    status_operation = paths[paths_run]["get"]
    verify_json_response(spec, status_operation, 200, state, status_headers, expected_headers=("ETag",))
    assert "304" in status_operation["responses"] and "ETag" in status_operation["responses"]["304"]["headers"]
    tag = status_headers["ETag"]
    empty, cached = call(port, receipt["status_url"], 304, headers={**auth, "If-None-Match": tag})
    assert empty == b"" and cached["ETag"] == tag
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


def verify_budget_reservations(port, peer, stop_worker, start_worker, query_db, hold_budget, receiver):
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
            call(port, path, 200, method="PUT", headers=session, body=policy)
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
    call(port, "/v1/requests", 202, method="POST", body={"quote_id": quote["id"], "input": quote["input"]},
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


def rehearse(gateway, admin, directory):
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
        command(*psql, "-c", "CREATE TABLE public.existing_fixture (id integer)", cwd=directory, env=pg_env)
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
        settings = dict(env, FOUNDATION_ENV="development", TOKENCRAFT_PROCESS_ROLE="api",
            DATABASE_URL=f"postgresql://tokencraft_runtime:{runtime_password}@{HOST}:{db_port}/postgres",
            APP_BASE_URL=f"http://localhost:{api_port}", API_GATEWAY_PORT=str(api_port),
            COOKIE_DOMAIN="localhost", COOKIE_SECURE="false", SESSION_SECRET=token_hex(32),
            TOKENCRAFT_CORE_CATALOG=str(FIXTURES / "runtime-catalog.json"), TOKENCRAFT_WORKFLOW_KEY=token_hex(32))
        compatible = command(admin, "initialize-demo", cwd=directory, env=settings)
        assert compatible == "existing demo schema is compatible"
        print("PASS startup initializes an empty schema and verifies an existing schema with the runtime login")
        with service("setup-api", [gateway], directory, settings, lambda: ready(api_port)):
            session, account, project = provision(api_port)
            contract, _ = call(api_port, "/openapi.json", 200)
            for path in ("/v1/setup/status", "/v1/operations/queues", "/v1/operations/artifact-publication"):
                expected = 200 if path in contract["paths"] else 404
                call(api_port, f"{path}?account_id={account}", expected, headers=session)

        settings["TOKENCRAFT_ENABLE_LEGACY_OPERATIONS"] = "true"
        media_port, token = loopback_port(), token_hex(32)
        stack.enter_context(service("delivery", [sys.executable, "-B", MEDIA / "server.py", "--pack", "delivery",
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

            def start_worker():
                worker_port = loopback_port()
                workers.enter_context(service(f"worker-{worker_port}", [gateway], directory,
                    dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port)),
                    lambda: ready(worker_port)))

            start_worker()

            def check_rollout(sibling_project, digest):
                verify_tool_rollout(gateway, directory, settings, registry, sibling_project, digest)

            verify_workflow(api_port, peer_port, session, account, project, workers.close, start_worker, receiver,
                            check_rollout)
            verify_budget_reservations(api_port, peer_port, workers.close, start_worker, query_db, hold_budget, receiver)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path, help="compiled api-gateway binary")
    parser.add_argument("admin", type=Path, help="compiled admin-cli binary from the same checkout")
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-workflow-") as temp:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(temp))


if __name__ == "__main__":
    main()
