"""Verify signed delivery under receiver stalls and terminal backlog on disposable services."""

import argparse
from base64 import b64decode, b64encode
from contextlib import ExitStack
from hashlib import sha256
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
from queue import Empty, Queue
from secrets import token_hex
from tempfile import TemporaryDirectory
from threading import Event, Lock
from time import monotonic, time
from urllib.parse import urlsplit

from postgres_tools import postgres_bin
from smoke_cell_faults import eventually, serve
from smoke_workflow_contract import HOST, call, command, loopback_port, provision, ready, rehearse, service


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        fields = {key.lower(): value for key, value in self.headers.items()}
        raw = self.rfile.read(int(fields["content-length"]))
        with self.server.lock:
            event_id = fields["webhook-id"]
            first = event_id not in self.server.seen
            self.server.seen.add(event_id)
            self.server.received_at[event_id] = monotonic()
            self.server.received.put((self.path, fields, raw))
        if first and self.server.slow:
            self.server.release.wait(8)
            self.send_response(503)
            self.send_header("Retry-After", self.server.retry_after)
        else:
            self.send_response(204)
        self.end_headers()


class Receiver(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, *, slow=False):
        super().__init__((HOST, 0), Handler)
        self.slow = slow
        self.retry_after = "1"
        self.release = Event()
        self.lock = Lock()
        self.seen = set()
        self.received_at = {}
        self.received = Queue()

    def url(self, path):
        return f"http://{HOST}:{self.server_port}{path}"


def verify_event(sample, subscription, project, request):
    _, headers, raw = sample
    event_id = headers["webhook-id"]
    timestamp = headers["webhook-timestamp"]
    secret = b64decode(subscription["signing_secret"].removeprefix("whsec_"))
    signature = hmac.digest(secret, f"{event_id}.{timestamp}.".encode() + raw, sha256)
    assert hmac.compare_digest(headers["webhook-signature"], "v1," + b64encode(signature).decode())
    assert abs(time() - int(timestamp)) < 60
    event = json.loads(raw)
    assert event["id"] == event_id and event["type"] == "request.canceled"
    assert event["data"] == {"project_id": project, "request_id": request, "status": "canceled",
                             "fetch_url": f"/v1/requests/{request}"}
    return event_id, raw


def verify(gateway, directory, settings, session, account, project, case):
    api = int(settings["API_GATEWAY_PORT"])
    worker_port = loopback_port()
    with ExitStack() as stack:
        slow = stack.enter_context(serve(Receiver(slow=True)))
        fast = stack.enter_context(serve(Receiver()))
        config = directory / "webhook-routes.json"
        config.write_text(json.dumps({"destinations": {
            "slow_a": slow.url("/a"), "slow_b": slow.url("/b"), "same_url": slow.url("/a"),
            "fast": fast.url("/events"),
        }}))
        settings = dict(settings, TOKENCRAFT_DEMO_WEBHOOK_CONFIG=str(config), TOKENCRAFT_DEMO_WEBHOOK_KEY=token_hex(32))
        stack.enter_context(service("webhook-api", [gateway], directory, settings, lambda: ready(api)))
        healthy_session, healthy_account, healthy_project = provision(api)

        def subscription(owner, org, project_id, destination):
            base = f"/v1/projects/{project_id}"
            query = f"?account_id={org}"
            row, _ = call(api, base + "/webhooks" + query, 201, method="POST", headers=owner,
                          body={"name": destination, "destination_id": destination})
            principal, _ = call(api, base + "/service-principals" + query, 201, method="POST", headers=owner,
                                body={"name": destination, "scopes": ["requests:submit", "requests:cancel"]})
            return row, {"Authorization": "Bearer " + principal["token"]}

        def enqueue(target):
            row, auth = target
            quote, _ = call(api, "/v1/quotes/scoped", 200, method="POST", headers=auth, body={
                "model": "runtime_image", "workload": {"kind": "image", "count": 1, "aspect_ratio": "4:3"},
            })
            receipt, _ = call(api, "/v1/requests", 202, method="POST",
                              headers={**auth, "Idempotency-Key": token_hex(16)}, body={
                                  "quote_id": quote["id"], "input": quote["input"],
                                  "delivery": {"webhook_subscription_id": row["id"]},
                              })
            canceled, _ = call(api, receipt["status_url"] + "/cancel", 200, method="POST", headers=auth, body={})
            assert canceled["status"] == "canceled"
            return receipt["id"]

        healthy = subscription(healthy_session, healthy_account, healthy_project, "fast")
        destinations = {path: subscription(session, account, project, "slow_" + path[1:])
                        for path in ("/a", "/b")}
        worker = dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port))

        if case in ("both", "expired"):
            expired_session, expired_account, expired_project = provision(api)
            expired_target = subscription(expired_session, expired_account, expired_project, "slow_a")
            expired = [enqueue(expired_target) for _ in range(16)]
            db = urlsplit(settings["DATABASE_URL"])
            sql_env = dict(PATH=settings["PATH"], PGHOST=db.hostname, PGPORT=str(db.port),
                           PGUSER=db.username, PGPASSWORD=db.password, PGDATABASE=db.path.lstrip("/"))
            sql = directory / "age-webhook-fixture.sql"
            sql.write_text("UPDATE gateway_webhook_deliveries SET deadline_at = now() - interval '1 second' "
                           "WHERE request_id = ANY (string_to_array(:'expired', ','));")
            command(postgres_bin() / "psql", "-Xqw", "-v", "ON_ERROR_STOP=1", "-v", "expired=" + ",".join(expired),
                    "-f", sql, cwd=directory, env=sql_env)
            request = enqueue(healthy)
            with service("webhook-expiry-worker", [gateway], directory, worker, lambda: ready(worker_port)):
                verify_event(fast.received.get(timeout=2.5), healthy[0], healthy_project, request)
                assert slow.received.empty()
                path = f"/v1/projects/{expired_project}/webhook-deliveries?account_id={expired_account}"
                items, _ = call(api, path, 200, headers=expired_session)
                assert len(items["items"]) == len(expired)
                assert all(row["state"] == "dead" and row["last_error"] == "retry_window_expired"
                           and row["attempt_count"] == 0 for row in items["items"])
            print("PASS healthy delivery advanced past the expired backlog without a receiver call for dead events")

        if case in ("both", "stall"):
            pending = {enqueue(destinations[path]): path for path in ("/a", "/b", "/a", "/b", "/a", "/b")}
            healthy_request = enqueue(healthy)
            with service("webhook-stall-worker", [gateway], directory, worker, lambda: ready(worker_port)):
                try:
                    samples = [slow.received.get(timeout=3) for _ in range(3)]
                    assert {sample[0] for sample in samples} == set(destinations)
                    verify_event(fast.received.get(timeout=2.5), healthy[0], healthy_project, healthy_request)
                    assert slow.received.empty(), "aliases of the stalled receiver consumed reserved capacity"
                finally:
                    slow.release.set()
                by_request = {}
                for sample in samples + [slow.received.get(timeout=12) for _ in range(2 * len(pending) - len(samples))]:
                    request = json.loads(sample[2])["data"]["request_id"]
                    path = pending[request]
                    assert sample[0] == path
                    by_request.setdefault(request, []).append(verify_event(sample, destinations[path][0], project, request))
                assert set(by_request) == set(pending)
                assert all(len(attempts) == 2 and attempts[0] == attempts[1] for attempts in by_request.values())
                list_path = f"/v1/projects/{project}/webhook-deliveries?account_id={account}"

                def recovered():
                    rows, _ = call(api, list_path, 200, headers=session)
                    return {row["request_id"] for row in rows["items"] if row["state"] == "delivered"
                            and row["attempt_count"] == 2} == set(pending)

                eventually(recovered, "stalled receiver recovery")
            print("PASS the healthy receiver delivered during the stall and all retries kept their signed bytes")

        if case == "cooldown":
            slow.retry_after = "8"
            slow.release.set()
            original = enqueue(destinations["/a"])
            path = f"/v1/projects/{project}/webhook-deliveries?account_id={account}"
            with service("webhook-first-worker", [gateway], directory, worker, lambda: ready(worker_port)):
                signed = verify_event(slow.received.get(timeout=3), destinations["/a"][0], project, original)
                first_received = slow.received_at[signed[0]]

                def retry_recorded():
                    rows, _ = call(api, path, 200, headers=session)
                    return any(row["request_id"] == original and row["state"] == "pending"
                               and row["attempt_count"] == 1 and row["last_error"] == "http_error"
                               for row in rows["items"])

                eventually(retry_recorded, "the receiver's retry delay", timeout=4)

            slow.slow = False
            alias = subscription(session, account, project, "same_url")
            queued = enqueue(alias)
            independent = enqueue(destinations["/b"])
            healthy_request = enqueue(healthy)
            with service("webhook-replacement-worker", [gateway], directory, worker, lambda: ready(worker_port)):
                sample = slow.received.get(timeout=3)
                assert sample[0] == "/b", "the replacement sent to a receiver still in cooldown"
                verify_event(sample, destinations["/b"][0], project, independent)
                verify_event(fast.received.get(timeout=2.5), healthy[0], healthy_project, healthy_request)
                try:
                    slow.received.get(timeout=0.6)
                except Empty:
                    pass
                else:
                    raise AssertionError("a receiver alias received work before the requested delay elapsed")

                resume = {}
                for _ in range(2):
                    sample = slow.received.get(timeout=11)
                    assert sample[0] == "/a"
                    request = json.loads(sample[2])["data"]["request_id"]
                    sub = destinations["/a"][0] if request == original else alias[0]
                    resume[request] = verify_event(sample, sub, project, request)
                assert set(resume) == {original, queued} and resume[original] == signed
                assert all(slow.received_at[event[0]] - first_received >= 8 for event in resume.values())
            print("PASS receiver delay survived worker replacement, held its alias, and preserved signed event replay")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    parser.add_argument("--case", choices=("both", "expired", "stall", "cooldown"), default="both")
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-webhook-backlog-") as temporary:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(temporary),
                 verification=lambda *params: verify(*params, args.case))


if __name__ == "__main__":
    main()
