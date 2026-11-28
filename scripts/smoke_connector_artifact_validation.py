"""Check terminal connector validation, durable charges, and retrieval retries."""

import argparse
from collections import Counter
from contextlib import ExitStack
from decimal import Decimal
import json
from pathlib import Path
from secrets import token_hex
from tempfile import TemporaryDirectory
from threading import Event
from time import monotonic, sleep

from smoke_cell_faults import Connector, ConnectorHandler, PIXELS, eventually, serve
from smoke_connector_recovery import fixture_png, read_image, wait_for_status
from smoke_workflow_contract import (
    HOST, call, loopback_port, ready, rehearse, service, verify_signed_delivery, webhook_receiver,
)

LANDSCAPE = fixture_png((35, 95, 210), 16, 12)
INTERRUPTED_OUTPUT = "Output is still becoming available"
UNKNOWN_CHARGE = "Output has no confirmed charge"
LATE_CHARGE = "Output has a charge on the next poll"


class ArtifactConnector(ConnectorHandler):
    def do_POST(self):
        if self.path != "/v1/jobs" or not self.authorized():
            return self.reply(404, {})
        job = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        request = job["request_id"]
        assert self.headers["Idempotency-Key"] == request
        with self.server.lock:
            self.server.jobs[request] += 1
            self.server.prompts[request] = job["input"]["prompt"]
        return self.reply(200, {
            "version": 1, "request_id": request, "outcome": {"status": "accepted", "handle": request},
        })

    def do_GET(self):
        fields = self.path.strip("/").split("/")
        if len(fields) == 3 and fields[:2] == ["v1", "jobs"] and self.authorized():
            request = fields[2]
            with self.server.lock:
                prompt = self.server.prompts.get(request)
                self.server.polls[request] += 1
                attempt = self.server.polls[request]
            if prompt in {INTERRUPTED_OUTPUT, UNKNOWN_CHARGE, LATE_CHARGE}:
                if attempt > 1 and prompt != LATE_CHARGE:
                    return self.reply(503, {})
                reply = {"version": 1, "status": "succeeded", "handle": request}
                if prompt == INTERRUPTED_OUTPUT or (prompt == LATE_CHARGE and attempt > 1):
                    reply["provider_charge"] = {"unit": "credit", "quantity": "1.25"}
                return self.reply(200, reply)
        if len(fields) != 5 or fields[3] != "artifacts":
            return super().do_GET()
        if not self.authorized():
            return self.reply(401, {})
        request, slot = fields[2], int(fields[4])
        with self.server.lock:
            prompt = self.server.prompts.get(request)
            self.server.fetches[request, slot] += 1
            attempt = self.server.fetches[request, slot]
        if prompt == "Wrong type on second slot" and slot == 1:
            return self.reply(200, PIXELS, "image/jpeg")
        if prompt == "Output exceeds its limit":
            self.send_response(200)
            self.send_header("Content-Type", "image/png")
            self.send_header("Content-Length", str(2 * 1024 * 1024 + 1))
            self.end_headers()
            return
        if prompt == INTERRUPTED_OUTPUT:
            if attempt == 1:
                self.server.fetch_started.set()
                self.server.release_fetch.wait(8)
                return self.reply(503, {})
            if attempt == 2:
                self.send_response(200)
                self.send_header("Content-Type", "image/png")
                self.send_header("Content-Length", str(len(LANDSCAPE)))
                self.end_headers()
                self.wfile.write(LANDSCAPE[:8])
                return
            return self.reply(200, LANDSCAPE, "image/png")
        if prompt in {UNKNOWN_CHARGE, LATE_CHARGE} and attempt == 1:
            return self.reply(503, {})
        return self.reply(200, PIXELS, "image/png")


def check_failed_artifact(port, receipt, headers):
    request = f"/v1/requests/{receipt['id']}"
    _, separator, scope = receipt["status_url"].partition("?")
    failed = wait_for_status(port, {**receipt, "status_url": request + separator + scope}, headers, "failed", 8)
    upstream = failed["upstream"]
    assert upstream["error_code"] == "provider_artifact_invalid"
    assert upstream["queue_state"] == "completed" and upstream["result_observed_at"]
    charge = upstream["provider_charge"]
    assert charge["unit"] == "credit" and Decimal(charge["quantity"]) == Decimal("1.25")
    call(port, request + "/result" + separator + scope, 404, headers=headers)
    call(port, request + "/artifacts/0" + separator + scope, 404, headers=headers)


def verify_workflow(port, session, account, project):
    query = f"?account_id={account}"
    project_path = f"/v1/projects/{project}"
    draft, _ = call(port, project_path + "/workflows" + query, 201, method="POST", headers=session, body={
        "schema": 1, "title": "Strict media validation", "max_usd_nanos": 1_000, "inputs": {},
        "nodes": [{
            "id": "generate", "title": "Generate", "kind": "image", "model": "fixture_image",
            "max_usd_nanos": 1_000, "settings": {
                "prompt": {"source": "fixed", "value": "Square data for a 4:3 request"},
                "aspect_ratio": {"source": "fixed", "value": "4:3"},
            },
        }],
        "outputs": {"image": "generate"},
    })
    base = project_path + f"/workflows/{draft['id']}"
    trial, _ = call(port, base + "/test-runs" + query, 202, method="POST",
                    headers={**session, "Idempotency-Key": token_hex(16)},
                    body={"revision": draft["revision"], "inputs": {}})
    deadline = monotonic() + 8
    while monotonic() < deadline:
        result, _ = call(port, trial["status_url"], 200, headers=session)
        if result["status"] == "failed":
            break
        assert result["status"] not in ("succeeded", "canceled", "expired")
        sleep(0.1)
    else:
        raise AssertionError(f"Invalid workflow image remained {result['status']}")
    step, = result["steps"]
    assert result["error"] == "model_request_failed" and not result.get("outputs")
    assert step["status"] == "failed"
    check_failed_artifact(port, {
        "id": step["request"],
        "status_url": f"/v1/requests/{step['request']}{query}&project_id={project}",
    }, session)
    return step["request"]


def configure(directory, settings, connector, receiver):
    bindings = directory / "validation-bindings.json"
    bindings.write_text(json.dumps({"version": 1, "bindings": [{
        "name": "validation", "revision": "v1", "origin": f"http://{HOST}:{connector.server_port}/",
        "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_VALIDATION",
    }]}))
    catalog = directory / "validation-catalog.json"
    catalog.write_text(json.dumps({"version": 1, "offers": [{
        "id": "fixture_image", "model": "fixture_image", "provider": "self_hosted",
        "adapter": "connector_queue", "local_binding": "validation", "upstream_model": "fixture",
        "pricing": {"kind": "image", "usd_nanos_per_image": 100, "aspect_ratios": ["1:1", "4:3"]},
    }]}))
    webhooks = directory / "validation-hooks.json"
    webhooks.write_text(json.dumps({"destinations": {"fixture": f"http://{HOST}:{receiver.server_port}/events"}}))
    settings = dict(settings, TOKENCRAFT_CORE_CATALOG=str(catalog), TOKENCRAFT_CONNECTORS=str(bindings),
                    TOKENCRAFT_CONNECTOR_TOKEN_VALIDATION=connector.token, TOKENCRAFT_DEMO_WEBHOOK_CONFIG=str(webhooks),
                    TOKENCRAFT_DEMO_WEBHOOK_KEY=token_hex(32))
    settings.pop("TOKENCRAFT_LOCAL_CHAT_BINDINGS", None)
    return settings


def verify(gateway, directory, settings, session, account, project):
    port, worker_port = int(settings["API_GATEWAY_PORT"]), loopback_port()
    query, project_path = f"?account_id={account}", f"/v1/projects/{project}"
    with ExitStack() as stack:
        connector = Connector()
        connector.RequestHandlerClass = ArtifactConnector
        connector.prompts, connector.fetches = {}, Counter()
        connector.polls = Counter()
        connector.fetch_started, connector.release_fetch = Event(), Event()
        stack.enter_context(serve(connector))
        receiver = stack.enter_context(webhook_receiver())
        settings = configure(directory, settings, connector, receiver)
        stack.enter_context(service("validation-api", [gateway], directory, settings, lambda: ready(port)))
        worker = dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port))
        first_worker = stack.enter_context(service("validation-worker", [gateway], directory, worker,
                                                   lambda: ready(worker_port)))

        child = verify_workflow(port, session, account, project)
        print("PASS invalid connector image failed the original request and its workflow with the observed charge")
        key, _ = call(port, project_path + "/service-principals" + query, 201, method="POST", headers=session,
                      body={"name": "Artifact validation", "scopes": ["requests:submit", "requests:read", "usage:read"]})
        token = {"Authorization": "Bearer " + key["token"]}
        hook, _ = call(port, project_path + "/webhooks" + query, 201, method="POST", headers=session,
                       body={"name": "Artifact validation", "destination_id": "fixture"})

        def submit(prompt, count, delivery=False, **shape):
            headers = {**token, "Idempotency-Key": token_hex(16)}
            if delivery:
                headers["X-TokenCraft-Webhook-Subscription-Id"] = hook["id"]
            body = {"task": "text_to_image", "model": "fixture_image", "prompt": prompt,
                    "count": count, "aspect_ratio": "1:1"}
            body.update(shape)
            receipt, _ = call(port, "/v1/image", 202, method="POST", headers=headers, body=body)
            return receipt, headers, body

        mismatch, headers, body = submit("Wrong type on second slot", 2, delivery=True)
        oversize, _, _ = submit("Output exceeds its limit", 1)
        for receipt in (mismatch, oversize):
            check_failed_artifact(port, receipt, token)
        call(port, f"/v1/requests/{mismatch['id']}/artifacts/1", 404, headers=token)
        cached, _ = call(port, "/v1/image", 200, method="POST", headers=headers, body=body)
        assert cached == mismatch
        verify_signed_delivery(port, project_path + "/webhook-deliveries", query, session, receiver, hook,
                               "request.failed", {"request_id": mismatch["id"], "project_id": project,
                                                  "status": "failed", "fetch_url": f"/v1/requests/{mismatch['id']}"})
        print("PASS invalid type and size stopped publication, kept idempotent replay, and sent a signed failure")

        deferred, _, _ = submit(INTERRUPTED_OUTPUT, 1,
                                aspect_ratio="7:5", max_relative_error_bps=500)
        deferred["status_url"] = f"/v1/requests/{deferred['id']}"

        def pending_with_charge():
            state, _ = call(port, deferred["status_url"], 200, headers=token)
            return state["status"] == "running" and state["upstream"].get("provider_charge")

        eventually(pending_with_charge, "observed charge during artifact retrieval", timeout=5)
        assert connector.fetch_started.wait(5), "The worker did not start retrieval before the interruption"
        first_worker.kill()
        first_worker.wait(timeout=5)
        connector.release_fetch.set()
        stack.enter_context(service("validation-worker-restarted", [gateway], directory, worker,
                                    lambda: ready(worker_port)))
        delivered = wait_for_status(port, deferred, token, "succeeded", 30)
        assert delivered["quote"]["candidates"][0]["applied_aspect_ratio"] == "4:3"
        assert read_image(port, deferred, token) == LANDSCAPE
        print("PASS the restarted worker used the stored completion and recovered the interrupted artifact transfer")

        unknown, _, _ = submit(UNKNOWN_CHARGE, 1)
        late, _, _ = submit(LATE_CHARGE, 1)
        for receipt, has_charge in ((unknown, False), (late, True)):
            receipt["status_url"] = f"/v1/requests/{receipt['id']}"
            completed = wait_for_status(port, receipt, token, "succeeded", 10)
            assert bool(completed["upstream"].get("provider_charge")) == has_charge
            assert read_image(port, receipt, token) == PIXELS
        usage, _ = call(port, "/v1/usage", 200, headers=token)
        totals = usage["totals"]
        assert (totals["failed"], totals["succeeded"], totals["in_progress"], totals["completed_outputs"]) == (3, 3, 0, 3)
        charge, = totals["provider_charges"]
        assert charge["unit"] == "credit" and charge["observations"] == 5 and Decimal(charge["quantity"]) == Decimal("6.25")
        with connector.lock:
            assert connector.jobs == Counter((child, mismatch["id"], oversize["id"], deferred["id"], unknown["id"], late["id"]))
            assert connector.fetches == Counter({
                (child, 0): 1, (mismatch["id"], 0): 1, (mismatch["id"], 1): 1,
                (oversize["id"], 0): 1, (deferred["id"], 0): 3, (unknown["id"], 0): 2, (late["id"], 0): 2,
            })
            assert connector.polls[deferred["id"]] == 1
            assert connector.polls[unknown["id"]] == connector.polls[late["id"]] == 2
        print("PASS retrieval tolerated a missing charge and still captured a late charge exactly once")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-artifact-validation-") as directory:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(directory), verification=verify)


if __name__ == "__main__":
    main()
