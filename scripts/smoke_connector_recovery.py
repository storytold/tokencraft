"""Exercise connector receipt recovery across gateway replicas and a worker restart."""

import argparse
from collections import Counter
from contextlib import ExitStack, contextmanager
from datetime import datetime, timedelta, timezone
from functools import partial
from hashlib import sha256
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
from secrets import token_hex
import struct
from tempfile import TemporaryDirectory
from threading import Event, Lock, Thread
from time import monotonic, sleep
from urllib.parse import urlencode
from zlib import compress, crc32

from smoke_workflow_contract import (
    CALLER_CONTEXT, HOST, ROOT, call, loopback_port, provider_traceparent, ready, rehearse, service,
)


def fixture_png(rgb, width, height):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", crc32(kind + data))

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", compress((b"\0" + bytes(rgb) * width) * height)) + chunk(b"IEND", b""))


def square_png(rgb):
    return fixture_png(rgb, 16, 16)


class Connector(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, status, payload, content_type="application/json"):
        data = json.dumps(payload).encode() if content_type == "application/json" else payload
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def trace(self, operation, subject):
        self.server.traces.append((operation, subject, self.headers.get("traceparent"),
                                   self.headers.get("tracestate"), self.headers.get("baggage")))

    def do_POST(self):
        if self.path != "/v1/jobs" or self.headers.get("Authorization") != self.server.authorization:
            self.reply(404, {})
            return
        job = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        request = job["request_id"]
        with self.server.lock:
            self.server.submissions[request] += 1
            self.server.jobs[request] = job
            self.trace("submit", request)
        if job["input"]["prompt"] == "second" and "completion" not in job:
            # The connector persisted the receipt, then the connection closed before acknowledgement.
            self.close_connection = True
            return
        self.reply(200, {"version": 1, "request_id": request,
                         "outcome": {"status": "accepted", "handle": "first"}})

    def do_GET(self):
        if self.headers.get("Authorization") != self.server.authorization:
            self.reply(401, {})
            return
        prefix = "/v1/jobs/by-request/"
        if self.path.startswith(prefix):
            request = self.path.removeprefix(prefix)
            with self.server.lock:
                job = self.server.jobs.get(request)
                self.trace("lookup", request)
            if job is None:
                self.reply(404, {})
                return
            if self.server.release_lookups is not None:
                with self.server.lock:
                    self.server.stalled_lookups.append((job["model"], monotonic()))
                self.server.lookup_started.set()
                self.server.release_lookups.wait(timeout=8)
                self.close_connection = True
                return
            handle = "first"
            if job["input"]["prompt"] == "second":
                self.server.lookup_started.set()
                if self.server.corrected.is_set():
                    handle = "second"
            self.reply(200, {"version": 1, "request_id": request, "handle": handle})
            return
        for handle, pixels in self.server.images.items():
            path = f"/v1/jobs/{handle}"
            if self.path == path:
                with self.server.lock:
                    self.trace("status", handle)
                self.reply(200, {"version": 1, "handle": handle, "status": "succeeded",
                                 "provider_charge": {"unit": "credit", "quantity": self.server.charges[handle]}})
                return
            if self.path == path + "/artifacts/0":
                with self.server.lock:
                    self.server.fetches[handle] += 1
                    self.trace("artifact", handle)
                self.reply(200, pixels, "image/png")
                return
        self.reply(404, {})


@contextmanager
def connector_server(token, *, stall_lookups=False):
    with ThreadingHTTPServer((HOST, 0), Connector) as server:
        server.authorization = f"Bearer {token}"
        server.lock = Lock()
        server.jobs, server.submissions, server.fetches = {}, Counter(), Counter()
        server.traces = []
        server.lookup_started, server.corrected = Event(), Event()
        server.release_lookups = Event() if stall_lookups else None
        server.stalled_lookups = []
        server.images = {"first": square_png((220, 50, 20)), "second": square_png((20, 50, 220))}
        server.charges = {"first": "2.17", "second": "5.21"}
        thread = Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        thread.start()
        try:
            yield server
        finally:
            if server.release_lookups is not None:
                server.release_lookups.set()
            server.shutdown()
            thread.join(timeout=2)


def wait_for_status(port, receipt, auth, status, timeout):
    deadline = monotonic() + timeout
    while monotonic() < deadline:
        state, _ = call(port, receipt["status_url"], 200, headers=auth)
        if state["status"] == status:
            return state
        if state["status"] in ("failed", "canceled", "succeeded"):
            break
        sleep(0.1)
    raise AssertionError(f"Expected {status} within {timeout}s, observed {state['status']}")


def read_image(port, receipt, auth):
    result, _ = call(port, receipt["status_url"] + "/result", 200, headers=auth)
    image, = result["result"]["images"]
    body, _ = call(port, image["url"], 200, headers=auth)
    assert sha256(body).hexdigest() == image["artifact"]["sha256"]
    return body


def verify_read_views(port, peer, session, account, project, first, second, first_client, second_client):
    large_amount = 9_007_199_254_740_995
    path = f"/v1/projects/{project}/requests/{first['id']}/settlements?account_id={account}"
    for revision, amount in enumerate((large_amount + 5, large_amount)):
        call(port, path, 201, method="POST", headers={**session, "Idempotency-Key": f"settlement-fixture-{revision}"},
             body={"expected_revision": revision, "amount_usd_nanos": str(amount), "evidence_ref": "fixture-usage"})

    def history(api, auth, **filters):
        query = "?" + urlencode(filters) if filters else ""
        page, _ = call(api, "/v1/requests" + query, 200, headers=auth)
        assert page["next_after"] is None
        return [row["id"] for row in page["requests"]]

    project_scope = {"account_id": account, "project_id": project}
    assert history(port, session, **project_scope) == [second["id"], first["id"]]
    assert history(peer, session, **project_scope, after=second["id"]) == [first["id"]]
    assert history(peer, first_client) == [first["id"]]
    assert history(peer, first_client, after=second["id"]) == []
    assert history(peer, second_client, after=second["id"]) == []
    trace, _ = call(port, f"/v1/requests/{first['id']}/trace", 200, headers=first_client)
    assert history(peer, session, **project_scope, trace_id=trace["correlation"]["trace_id"]) == [first["id"]]
    today = datetime.now(timezone.utc).date()
    dates = {"from": today - timedelta(days=1), "to": today + timedelta(days=2)}
    assert history(peer, first_client, status="succeeded", cost_state="supplier_settled", **dates,
                   provider="self_hosted", model="example_image", exact_model="true", app_unattributed="true") == [first["id"]]
    assert history(peer, second_client, status="succeeded", cost_state="supplier_settled") == []

    usage, _ = call(peer, "/v1/usage?" + urlencode({**project_scope, "group_by": "client"}), 200, headers=session)
    assert usage["totals"]["requests"] == usage["totals"]["succeeded"] == 2
    assert usage["totals"]["supplier_settled_usd_nanos"] == str(large_amount)
    assert usage["totals"]["supplier_settled_requests"] == 1
    assert usage["totals"]["provider_charges"] == [{
        "provider": "self_hosted", "unit": "credit", "quantity": "7.38", "observations": 2,
    }]
    isolated, _ = call(peer, "/v1/usage", 200, headers=first_client)
    assert isolated["totals"]["supplier_settled_usd_nanos"] == str(large_amount)
    assert isolated["totals"]["provider_charges"][0]["quantity"] == "2.17"
    print("PASS scoped pages, cursor isolation, trace and cost filters, and exact project and client usage")


def verify_recovery(gateway, directory, settings, session, account, project, *, webhooks=True, receipt_lookup=False):
    token = token_hex(32)
    port = int(settings["API_GATEWAY_PORT"])
    peer = loopback_port()
    with ExitStack() as apps:
        connector = apps.enter_context(connector_server(token))
        bindings = directory / "connector.json"
        binding = {
            "name": "example", "revision": "recovery-v1", "origin": f"http://{HOST}:{connector.server_port}/",
            "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE",
        }
        if webhooks:
            binding["completion_webhooks"] = True
        bindings.write_text(json.dumps({"version": 1, "bindings": [binding]}))
        price = 1_250_000
        catalog = directory / "priced-recovery.json"
        config = json.loads((ROOT / "examples/connector/catalog.json").read_bytes())
        config["offers"][0]["pricing"]["usd_nanos_per_image"] = price
        catalog.write_text(json.dumps(config))
        settings = dict(settings, TOKENCRAFT_CORE_CATALOG=str(catalog),
                        TOKENCRAFT_CONNECTORS=str(bindings), TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE=token)
        settings.pop("TOKENCRAFT_LOCAL_CHAT_BINDINGS", None)
        apps.enter_context(service("recovery-api", [gateway], directory, settings, lambda: ready(port)))
        apps.enter_context(service("recovery-peer", [gateway], directory, dict(
            settings, API_GATEWAY_PORT=str(peer), APP_BASE_URL=f"http://localhost:{peer}",
        ), lambda: ready(peer)))
        if receipt_lookup:
            # Enable lookup on the workers without changing the identity already loaded by the APIs.
            binding["receipt_lookup"] = True
            bindings.write_text(json.dumps({"version": 1, "bindings": [binding]}))

        @contextmanager
        def worker(name):
            worker_port = loopback_port()
            with service(name, [gateway], directory,
                         dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port)),
                         lambda: ready(worker_port)):
                yield

        def application(name):
            key, _ = call(port, f"/v1/projects/{project}/service-principals?account_id={account}",
                          201, method="POST", headers=session,
                          body={"name": name, "client_ref": name, "scopes": [
                              "requests:submit", "requests:read", "usage:read",
                          ]})
            return {"Authorization": "Bearer " + key["token"]}

        def submit(api, auth, label):
            return call(api, "/v1/image", (200, 202), method="POST",
                        headers={**auth, **CALLER_CONTEXT, "Idempotency-Key": "fixture-recovery-" + label},
                        body={"model": "example_image", "task": "text_to_image", "prompt": label,
                              "count": 1, "aspect_ratio": "1:1"})[0]

        first_client, second_client = application("first"), application("second")
        project_budget = "/v1/routing-policy?" + urlencode({"account_id": account, "project_id": project})
        client_budget = f"/v1/projects/{project}/client-budgets/second?account_id={account}"
        call(port, project_budget, 200, method="PUT", headers=session, body={
            "expected_revision": 0, "policy": {"monthly_estimated_budget_usd_nanos": price * 2},
        })
        call(port, client_budget, 200, method="PUT", headers=session, body={
            "expected_revision": 0, "limit_usd_nanos": price,
        })

        def prove_reservations():
            project_view, _ = call(peer, project_budget, 200, headers=session)
            client_view, _ = call(peer, client_budget, 200, headers=session)
            assert project_view["monthly_budget"]["reserved_usd_nanos"] == str(price * 2)
            assert client_view["reserved_usd_nanos"] == str(price)

        with worker("first-worker"):
            first = submit(port, first_client, "first")
            wait_for_status(peer, first, first_client, "succeeded", 12)
            assert read_image(peer, first, first_client) == connector.images["first"]
            second = submit(peer, second_client, "second")
            wait_for_status(port, second, second_client, "awaiting_reconciliation", 12)
            assert connector.lookup_started.wait(timeout=8), "Worker did not look up the stored receipt"
            replay = submit(port, second_client, "second")
            assert replay["id"] == second["id"]
            call(port, first["status_url"], 404, headers=second_client)
            call(peer, second["status_url"], 404, headers=first_client)
            usage, _ = call(peer, "/v1/usage", 200, headers=second_client)
            assert usage["totals"]["awaiting_reconciliation"] == 1
            assert usage["totals"].get("provider_charges", []) == []
            prove_reservations()
            blocked, _ = call(peer, "/v1/image", 409, method="POST",
                              headers={**second_client, "Idempotency-Key": "fixture-extra-spend"},
                              body={"model": "example_image", "task": "text_to_image",
                                    "prompt": "Budget check", "count": 1, "aspect_ratio": "1:1"})
            assert blocked["error"]["code"] == "monthly_budget_exceeded"
            assert connector.fetches == {"first": 1}

            if webhooks:
                first_grant = connector.jobs[first["id"]]["completion"]
                grant = connector.jobs[second["id"]]["completion"]
                call(port, grant["path"], 401, method="POST", body={"version": 1},
                     headers={"Authorization": "Bearer " + first_grant["token"]})
                for api in (port, peer):
                    call(api, grant["path"], 204, method="POST", body={"version": 1},
                         headers={"Authorization": "Bearer " + grant["token"]})
            else:
                assert all("completion" not in job for job in connector.jobs.values())

        connector.corrected.set()
        with worker("replacement-worker"):
            wait_for_status(peer, second, second_client, "succeeded", 22)
            assert read_image(port, second, second_client) == connector.images["second"]
        for label, auth in (("first", first_client), ("second", second_client)):
            usage, _ = call(peer, "/v1/usage", 200, headers=auth)
            totals = usage["totals"]
            assert totals["requests"] == totals["succeeded"] == 1
            assert totals["awaiting_reconciliation"] == 0
            assert totals["provider_charges"] == [{
                "provider": "self_hosted", "unit": "credit", "quantity": connector.charges[label],
                "observations": 1,
            }]
        assert connector.submissions == {first["id"]: 1, second["id"]: 1}
        assert connector.fetches == {"first": 1, "second": 1}
        prove_reservations()
        print("PASS an unconfirmed paid request held its project and client estimates through recovery")
        expected = {}
        for label, receipt, auth in (("first", first, first_client), ("second", second, second_client)):
            parent = provider_traceparent(port, receipt["id"], auth)
            expected[label] = parent
            expected[receipt["id"]] = parent
        assert expected["first"] != expected["second"]
        assert {operation for operation, *_ in connector.traces} == {"submit", "lookup", "status", "artifact"}
        for operation, subject, parent, state, baggage in connector.traces:
            assert (parent, state, baggage) == (expected[subject], None, None), operation
        print("PASS connector requests use the scoped OTLP parent across recovery without caller metadata")
        if webhooks:
            print("PASS conflicting handle stayed unconfirmed, then recovered with the right output and charge after restart")
            verify_read_views(port, peer, session, account, project, first, second, first_client, second_client)
        else:
            print("PASS dropped acknowledgement recovered by lookup with no callbacks and one provider submission")


def verify_lookup_isolation(gateway, directory, settings, session, account, project):
    token = token_hex(32)
    port = int(settings["API_GATEWAY_PORT"])
    with ExitStack() as apps:
        stalled = apps.enter_context(connector_server(token, stall_lookups=True))
        healthy = apps.enter_context(connector_server(token))
        healthy.corrected.set()
        bindings = directory / "lookup-bindings.json"
        catalog = directory / "lookup-catalog.json"
        offer = json.loads((ROOT / "examples/connector/catalog.json").read_bytes())["offers"][0]
        destinations = {"slow_primary": stalled, "slow_alias": stalled, "healthy": healthy}
        bindings.write_text(json.dumps({"version": 1, "bindings": [{
            "name": name, "revision": "lookup-isolation", "origin": f"http://{HOST}:{server.server_port}/",
            "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE", "receipt_lookup": True,
        } for name, server in destinations.items()]}))
        catalog.write_text(json.dumps({"version": 1, "offers": [{
            **offer, "id": name, "model": name, "upstream_model": name, "local_binding": name,
        } for name in destinations]}))
        settings = dict(settings, TOKENCRAFT_CORE_CATALOG=str(catalog), TOKENCRAFT_CONNECTORS=str(bindings),
                        TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE=token)
        settings.pop("TOKENCRAFT_LOCAL_CHAT_BINDINGS", None)
        apps.enter_context(service("lookup-api", [gateway], directory, settings, lambda: ready(port)))
        worker_port = loopback_port()
        worker = dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port))
        apps.enter_context(service("lookup-worker", [gateway], directory, worker, lambda: ready(worker_port)))
        principal, _ = call(port, f"/v1/projects/{project}/service-principals?account_id={account}",
                            201, method="POST", headers=session, body={
                                "name": "Lookup isolation", "scopes": ["requests:submit", "requests:read", "usage:read"],
                            })
        auth = {"Authorization": "Bearer " + principal["token"]}

        def submit(model):
            return call(port, "/v1/image", 202, method="POST", body={
                "model": model, "task": "text_to_image", "prompt": "second", "count": 1, "aspect_ratio": "1:1",
            }, headers={**auth, "Idempotency-Key": token_hex(16)})[0]

        submit("slow_primary")
        assert stalled.lookup_started.wait(timeout=10), "The first receipt lookup did not start"
        with stalled.lock:
            first_lookup = stalled.stalled_lookups[0][1]
        submit("slow_alias")
        submit("slow_primary")
        submit("slow_alias")
        receipt = submit("healthy")
        # The old single lookup loop cannot start the healthy lookup before its four-second HTTP timeout.
        deadline = first_lookup + 3.4
        remaining = deadline - monotonic()
        assert remaining > 0, "The fixture did not submit before the first lookup timed out"
        wait_for_status(port, receipt, auth, "succeeded", remaining)
        recovered_in = monotonic() - first_lookup
        assert recovered_in < 3.4
        assert read_image(port, receipt, auth) == healthy.images["second"]
        assert healthy.submissions == {receipt["id"]: 1}
        sleep(max(0, deadline - monotonic()))
        with stalled.lock:
            started = [model for model, _ in stalled.stalled_lookups]
        assert len(started) == 3 and set(started) == {"slow_primary", "slow_alias"}, started
        assert not stalled.release_lookups.is_set()
        print(f"PASS healthy receipt recovered in {recovered_in:.2f}s while two aliases shared three stalled slots")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    gateway = args.gateway.resolve(strict=True)
    admin = args.admin.resolve(strict=True)
    polling = partial(verify_recovery, webhooks=False, receipt_lookup=True)
    for verify in (verify_recovery, polling, verify_lookup_isolation):
        with TemporaryDirectory(prefix="tokencraft-connector-recovery-") as directory:
            rehearse(gateway, admin, Path(directory), verification=verify)


if __name__ == "__main__":
    main()
