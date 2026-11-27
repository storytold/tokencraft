"""Verify signed delivery retries and redrive after a receiver loses its acknowledgment."""

import argparse
from base64 import b64decode, b64encode
from contextlib import ExitStack, closing
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
from queue import Queue
from secrets import token_hex
import sqlite3
from tempfile import TemporaryDirectory
from threading import Event
from time import time
from urllib.parse import urlsplit

from postgres_tools import postgres_bin
from smoke_cell_faults import eventually, serve
from smoke_connector_recovery import wait_for_status
from smoke_workflow_contract import HOST, call, command, loopback_port, ready, rehearse, service


class Receiver(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        event_id = self.headers["webhook-id"]
        timestamp = self.headers["webhook-timestamp"]
        raw = self.rfile.read(int(self.headers["content-length"]))
        signature = hmac.digest(self.server.key, f"{event_id}.{timestamp}.".encode() + raw, "sha256")
        expected = "v1," + b64encode(signature).decode()
        if abs(time() - int(timestamp)) >= 60 or not hmac.compare_digest(expected, self.headers["webhook-signature"]):
            self.send_error(401)
            return

        with closing(sqlite3.connect(self.server.database)) as db, db:
            recorded = db.execute("INSERT INTO receipts VALUES (?, ?) ON CONFLICT (id) DO NOTHING", (event_id, raw))
            if recorded.rowcount == 1:
                db.execute("UPDATE side_effect SET count = count + 1")
        self.server.received.put((event_id, raw))
        if self.server.acknowledge.is_set():
            self.send_response(204)
            self.end_headers()
        else:
            self.close_connection = True


def verify(gateway, directory, settings, session, account, project):
    api, worker_port = int(settings["API_GATEWAY_PORT"]), loopback_port()
    database = directory / "receiver.sqlite"
    with closing(sqlite3.connect(database)) as db, db:
        db.execute("CREATE TABLE receipts (id TEXT PRIMARY KEY NOT NULL, payload BLOB NOT NULL)")
        db.execute("CREATE TABLE side_effect (count INTEGER NOT NULL)")
        db.execute("INSERT INTO side_effect VALUES (0)")

    def verify_side_effect(event):
        with closing(sqlite3.connect(database)) as db:
            assert db.execute("SELECT id, payload FROM receipts").fetchall() == [event]
            assert db.execute("SELECT count FROM side_effect").fetchone() == (1,)

    with ExitStack() as stack:
        receiver = stack.enter_context(serve(ThreadingHTTPServer((HOST, 0), Receiver)))
        receiver.database, receiver.received, receiver.acknowledge = database, Queue(), Event()
        registry = directory / "receivers.json"
        registry.write_text(json.dumps({"destinations": {"receipt": f"http://{HOST}:{receiver.server_port}/events"}}))
        settings = dict(settings, TOKENCRAFT_DEMO_WEBHOOK_CONFIG=str(registry),
                        TOKENCRAFT_DEMO_WEBHOOK_KEY=token_hex(32))
        worker = dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port))
        stack.enter_context(service("receipt-api", [gateway], directory, settings, lambda: ready(api)))
        base, query = f"/v1/projects/{project}", f"?account_id={account}"
        subscription, _ = call(api, base + "/webhooks" + query, 201, method="POST", headers=session,
                               body={"name": "Receipt recovery fixture", "destination_id": "receipt"})
        receiver.key = b64decode(subscription["signing_secret"].removeprefix("whsec_"))
        key, _ = call(api, base + "/service-principals" + query, 201, method="POST", headers=session,
                      body={"name": "Receipt fixture", "scopes": ["requests:submit", "requests:read"]})
        auth = {"Authorization": "Bearer " + key["token"]}
        quote, _ = call(api, "/v1/quotes/scoped", 200, method="POST", headers=auth, body={
            "model": "runtime_image", "workload": {"kind": "image", "count": 1, "aspect_ratio": "4:3"},
        })
        receipt, _ = call(api, "/v1/requests", 202, method="POST",
                          headers={**auth, "Idempotency-Key": token_hex(16)},
                          body={"quote_id": quote["id"], "input": quote["input"],
                                "delivery": {"webhook_subscription_id": subscription["id"]}})

        with service("receipt-first-worker", [gateway], directory, worker, lambda: ready(worker_port)):
            original = receiver.received.get(timeout=15)
            event_id, raw = original
            event = json.loads(raw)
            assert event["id"] == event_id and event["type"] == "request.succeeded"
            assert event["data"] == {"request_id": receipt["id"], "project_id": project, "status": "succeeded",
                                     "fetch_url": receipt["status_url"]}
            verify_side_effect(original)
            completed = wait_for_status(api, receipt, auth, "succeeded", 5)
            assert completed["mock_result"] == {"kind": "image", "count": 1, "aspect_ratio": "4:3"}
            path = base + "/webhook-deliveries/" + event_id

            def detail():
                return call(api, path + query, 200, headers=session)[0]

            def retried():
                current = detail()
                return current["delivery"]["state"] == "pending" and [
                    attempt["outcome"] for attempt in current["attempts"]
                ] == ["network_error", "network_error"]

            assert receiver.received.get(timeout=6) == original
            eventually(retried, "two lost acknowledgments")
            verify_side_effect(original)

        # Advance only this fixture's retry deadline to avoid a fifteen-minute test.
        db = urlsplit(settings["DATABASE_URL"])
        env = dict(PATH=settings["PATH"], PGHOST=db.hostname, PGPORT=str(db.port),
                   PGUSER=db.username, PGPASSWORD=db.password, PGDATABASE=db.path.lstrip("/"))
        sql = directory / "expire-delivery.sql"
        sql.write_text("UPDATE gateway_webhook_deliveries SET deadline_at = now() - interval '1 second', "
                       "available_at = now() - interval '1 second' WHERE id = :'delivery';")
        command(postgres_bin() / "psql", "-Xqw", "-v", "ON_ERROR_STOP=1", "-v", "delivery=" + event_id,
                "-f", sql, cwd=directory, env=env)

        with service("receipt-replacement-worker", [gateway], directory, worker, lambda: ready(worker_port)):
            eventually(lambda: detail()["delivery"]["state"] == "dead", "the expired delivery")
            assert detail()["delivery"]["last_error"] == "retry_window_expired"
            assert receiver.received.empty()
            wait_for_status(api, receipt, auth, "succeeded", 5)
            receiver.acknowledge.set()
            redrive = path + "/redrive" + query
            call(api, redrive, 401, method="POST", headers=auth)
            call(api, redrive, 204, method="POST", headers=session)
            assert receiver.received.get(timeout=6) == original
            eventually(lambda: detail()["delivery"]["state"] == "delivered", "acknowledged redrive")
            assert [attempt["outcome"] for attempt in detail()["attempts"]] == [
                "delivered", "network_error", "network_error",
            ]
            call(api, redrive, 409, method="POST", headers=session)
            recovered = wait_for_status(api, receipt, auth, "succeeded", 5)
            assert recovered == completed
            verify_side_effect(original)
        print("PASS lost acknowledgments, retry, and redrive retained signed bytes and one receiver effect")
        print("PASS inference remained succeeded with the same request result")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-webhook-recovery-") as temporary:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(temporary), verification=verify)


if __name__ == "__main__":
    main()
