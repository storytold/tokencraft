"""Verify scheduled event retirement and scoped recovery with a disposable bucket and gateway."""

import argparse
from contextlib import ExitStack
from hashlib import sha256
import json
from pathlib import Path
from secrets import token_hex
from tempfile import TemporaryDirectory
from threading import Event
from urllib.parse import urlencode, urlsplit

from postgres_tools import postgres_bin
from smoke_cell_faults import BucketHandler, Connector, HeldBucket, eventually, serve
from smoke_connector_recovery import wait_for_status
from smoke_workflow_contract import HOST, ROOT, call, command, loopback_port, ready, rehearse, service


class ColdArchiveHandler(BucketHandler):
    def do_GET(self):
        if not self.server.cold.is_set():
            return super().do_GET()
        path = self.signed_path()
        with self.server.lock:
            if not path or path not in self.server.objects:
                self.reply(404)
                return
        if self.server.hold_verification.is_set():
            self.server.verification_started.set()
            if not self.server.release_verification.wait(7):
                self.reply(503)
                return
        self.reply(403, b"<Error><Code>InvalidObjectState</Code></Error>")

    def do_POST(self):
        request = urlsplit(self.path)
        body = self.rfile.read(min(4096, int(self.headers.get("Content-Length", "0"))))
        with self.server.lock:
            if (request.query != "restore" or request.path not in self.server.objects
                    or not self.headers.get("Authorization", "").startswith("AWS4-HMAC-SHA256 Credential=fixture-access/")
                    or self.headers.get("X-Amz-Content-Sha256") != sha256(body).hexdigest()
                    or b"<Days>1</Days>" not in body or b"<Tier>Standard</Tier>" not in body):
                self.reply(400)
                return
            self.server.restore_requests.append(request.path)
        self.reply(202)


def verify(gateway, directory, settings, session, account, project):
    port, worker_port = int(settings["API_GATEWAY_PORT"]), loopback_port()
    scope = urlencode({"account_id": account, "project_id": project})
    archive_list = "/v1/archives?" + scope
    principal_path = f"/v1/projects/{project}/service-principals?account_id={account}"
    feed = "/v1/exports/events"
    database = urlsplit(settings["DATABASE_URL"])
    pg_env = dict(PATH=settings["PATH"], PGHOST=database.hostname, PGPORT=str(database.port),
                  PGDATABASE=database.path.lstrip("/"), PGUSER=database.username, PGPASSWORD=database.password)
    psql = postgres_bin() / "psql"

    def advance_fixture_clock(statement, **bindings):
        script = directory / "fixture-clock.sql"
        script.write_text(statement)
        args = [piece for key, value in bindings.items() for piece in ("-v", f"{key}={value}")]
        command(psql, "-XqwAt", "-v", "ON_ERROR_STOP=1", *args, "-f", script,
                cwd=directory, env=pg_env)

    with ExitStack() as stack:
        bucket = HeldBucket()
        bucket.RequestHandlerClass = ColdArchiveHandler
        bucket.cold, bucket.hold_verification, bucket.verification_started, bucket.release_verification = (
            Event() for _ in range(4)
        )
        bucket.restore_requests = []
        stack.enter_context(serve(bucket))
        connector = stack.enter_context(serve(Connector()))
        bindings = directory / "archive-connector.json"
        bindings.write_text(json.dumps({"version": 1, "bindings": [{
            "name": "example", "revision": "archive-fixture", "origin": f"http://{HOST}:{connector.server_port}/",
            "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE",
        }]}))
        settings = dict(settings, TOKENCRAFT_CORE_CATALOG=str(ROOT / "examples/connector/catalog.json"),
                        TOKENCRAFT_CONNECTORS=str(bindings), TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE=connector.token,
                        TOKENCRAFT_DEMO_ARCHIVE_KEY=token_hex(32), TOKENCRAFT_DEMO_ARCHIVE_AUTO_AFTER_DAYS="1",
                        TOKENCRAFT_DEMO_ARCHIVE_RETIRE_AFTER_DAYS="1")
        settings.pop("TOKENCRAFT_LOCAL_CHAT_BINDINGS", None)
        settings.update({f"TOKENCRAFT_DEMO_ARCHIVE_S3_{suffix}": value for suffix, value in {
            "ENDPOINT": f"http://{HOST}:{bucket.server_port}", "BUCKET": "fixture-bucket", "REGION": "us-east-1",
            "ACCESS_KEY": "fixture-access", "SECRET_KEY": bucket.secret, "AUTO_RESTORE": "1",
        }.items()})
        stack.enter_context(service("archive-api", [gateway], directory, settings, lambda: ready(port)))
        worker = dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port))
        stack.enter_context(service("archive-worker", [gateway], directory, worker, lambda: ready(worker_port)))
        stack.callback(bucket.release_verification.set)

        def key(name, scopes, client=None):
            row, _ = call(port, principal_path, 201, method="POST", headers=session,
                          body={"name": name, "client_ref": client, "scopes": scopes})
            return {"Authorization": "Bearer " + row["token"]}

        project_reader = key("Project archive reader", ["exports:read"])
        clients = [key(label, ["requests:submit", "requests:read", "exports:read"], label)
                   for label in ("studio-first", "studio-second")]
        receipts = []
        for auth in clients:
            receipt, _ = call(port, "/v1/image", 202, method="POST",
                              headers={**auth, "Idempotency-Key": token_hex(16)},
                              body={"model": "example_image", "task": "text_to_image", "prompt": "Private fixture",
                                    "count": 1, "aspect_ratio": "1:1"})
            wait_for_status(port, receipt, auth, "succeeded", 18)
            receipts.append(receipt["id"])

        def read_feed(auth, **query):
            return call(port, feed + "?" + urlencode({"project_id": project, **query}), 200, headers=auth)

        def published():
            body, _ = read_feed(project_reader)
            completed = {event["request_id"] for event in map(json.loads, body.splitlines())
                         if event["type"] == "request.succeeded"}
            return completed == set(receipts)

        eventually(published, "event publication")
        advance_fixture_clock("UPDATE gateway_export_events SET occurred_at = now() - interval '3 days' "
                              "WHERE project_id = :'project';", project=project)
        expected, expected_headers = read_feed(clients[0])
        assert {event["request_id"] for event in map(json.loads, expected.splitlines())} == {receipts[0]}
        assert receipts[1].encode() not in expected and b"Private fixture" not in expected

        def archives():
            return call(port, archive_list, 200, headers=session)[0]["archives"]

        eventually(lambda: any(row["automatic"] and row["stored_at"] for row in archives()), "automatic seal")
        manifest, = archives()
        assert manifest["can_request_retrieval"]
        archive_id = manifest["id"]
        advance_fixture_clock("UPDATE gateway_archives SET stored_at = now() - interval '2 days' "
                              "WHERE id = :'archive';", archive=archive_id)
        eventually(lambda: any(row.get("retired_at") for row in archives()), "automatic retirement")
        assert read_feed(clients[0])[0] == expected
        print("PASS the automatic archive preserved the exact client feed after PostgreSQL retirement")

        archive = f"/v1/archives/{archive_id}"
        restore = archive + "/restore?" + scope + "&retrieve=true"
        bucket.cold.set()
        response, _ = call(port, feed + f"?project_id={project}", 409, headers=clients[0])
        assert response["error"]["code"] == "archive_retrieval_required" and archive_id not in json.dumps(response)
        call(port, archive, 404, headers=clients[0])
        call(port, restore, 403, method="POST", body={}, headers=project_reader)
        assert bucket.restore_requests == []

        bucket.hold_verification.set()
        call(port, archive + "/restore", 202, method="POST", headers=project_reader)
        assert bucket.verification_started.wait(8), "read-only archive verification did not start"
        verifying, _ = call(port, archive, 200, headers=project_reader)
        assert verifying["state"] == "restoring"
        for _ in range(2):
            call(port, restore, 202, method="POST", body={}, headers=session)
        bucket.hold_verification.clear()
        bucket.release_verification.set()
        eventually(lambda: len(bucket.restore_requests) == 1, "administrator's storage retrieval", timeout=12)
        eventually(lambda: call(port, archive, 200, headers=project_reader)[0]["restore_check_at"], "restore poll")
        bucket.cold.clear()
        advance_fixture_clock("UPDATE gateway_archives SET restore_check_at = now() WHERE id = :'archive';",
                              archive=archive_id)
        eventually(lambda: call(port, archive, 200, headers=project_reader)[0]["state"] == "available",
                   "verified cold restore")
        archive_data, archive_headers = call(port, archive + "/data", 200, headers=project_reader)
        assert archive_headers["X-TokenCraft-Content-Sha256"] == sha256(archive_data).hexdigest()
        assert {event["request_id"] for event in map(json.loads, archive_data.splitlines())} == set(receipts)
        call(port, archive + "/data", 404, headers=clients[0])
        recovered, headers = read_feed(clients[0])
        assert recovered == expected
        assert headers["X-TokenCraft-Content-Sha256"] == sha256(expected).hexdigest()
        cursor = headers["X-TokenCraft-Next-After"]
        assert cursor == expected_headers["X-TokenCraft-Next-After"]
        assert read_feed(clients[0], after=cursor)[0] == b""
        assert len(bucket.restore_requests) == 1
        assert connector.jobs == {identifier: 1 for identifier in receipts}
        print("PASS the administrator's retrieval superseded in-flight verification and the original cursor resumed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-event-archives-") as directory:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(directory), verification=verify)


if __name__ == "__main__":
    main()
