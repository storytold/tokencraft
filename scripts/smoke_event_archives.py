"""Verify scheduled event retirement and scoped recovery with a disposable bucket and gateway."""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack, contextmanager
from hashlib import sha256
from http.client import HTTPConnection
import json
from pathlib import Path
from secrets import token_hex
import socket
import subprocess
from tempfile import TemporaryDirectory
from threading import Event
from urllib.parse import urlencode, urlsplit

from postgres_tools import postgres_bin
from smoke_cell_faults import BucketHandler, Connector, HeldBucket, eventually, serve
from smoke_connector_recovery import wait_for_status
from smoke_workflow_contract import HOST, ROOT, call, command, loopback_port, ready, rehearse, service


MAX_ARCHIVE_BYTES = 8 * 1024 * 1024


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


@contextmanager
def slow_download(port, path, auth):
    with socket.socket() as connection:
        connection.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 16 * 1024)
        connection.settimeout(5)
        connection.connect((HOST, port))
        client = HTTPConnection(HOST, port)
        client.sock = connection
        try:
            client.request("GET", path, headers={"Origin": f"http://localhost:{port}", **auth})
            with client.getresponse() as response:
                assert response.status == 200
                assert int(response.headers["Content-Length"]) >= 5 * 1024 * 1024
                yield
        finally:
            client.close()


def verify_archive_download_capacity(port, path, reader, client, project, expected_archive, first_event):
    def retry_download():
        body, headers = call(port, path, (200, 503), headers=reader, max_bytes=MAX_ARCHIVE_BYTES)
        if isinstance(body, dict):
            return False
        assert body == expected_archive
        assert headers["X-TokenCraft-Content-Sha256"] == sha256(body).hexdigest()
        return True

    with slow_download(port, path, reader):
        with slow_download(port, path, reader):
            busy, headers = call(port, path, (200, 503), headers=reader, max_bytes=MAX_ARCHIVE_BYTES)
            assert isinstance(busy, dict), "A third archive download succeeded while two responses were stalled"
            assert busy["error"]["code"] == "export_capacity"
            assert headers["Retry-After"] == "2"
            assert headers["Cache-Control"] == "private, no-store"
            call(port, path, 401)
            call(port, path, 404, headers=client)
            feed = f"/v1/exports/events?project_id={project}&limit=1"
            assert call(port, feed, 200, headers=client)[0] == first_event
            receipt, _ = call(port, "/v1/image", 202, method="POST",
                              headers={**client, "Idempotency-Key": token_hex(16)},
                              body={"model": "example_image", "task": "text_to_image",
                                    "prompt": "Archive contention fixture", "count": 1, "aspect_ratio": "1:1"})
            call(port, receipt["status_url"], 200, headers=client)
        eventually(retry_download, "archive download after one disconnect", timeout=5)

    wait_for_status(port, receipt, client, "succeeded", 18)
    print("PASS stalled archive downloads applied backpressure and left scoped exports and admission available")


def verify_export_backpressure(port, directory, psql, pg_env, project, auth):
    snapshot, _ = call(port, "/v1/exports", 201, method="POST", headers=auth, body={"project_id": project})
    path = f"/v1/exports/{snapshot['id']}/data"
    expected, _ = call(port, path, 200, headers=auth)
    with ThreadPoolExecutor(max_workers=10) as reads, subprocess.Popen(
        [psql, "-XqAtw", "-v", "ON_ERROR_STOP=1"],
        cwd=directory, env=pg_env, text=True, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
    ) as holder:
        try:
            holder.stdin.write("SET lock_timeout = '3s';\nBEGIN;\n"
                               "LOCK TABLE gateway_export_snapshots IN ACCESS EXCLUSIVE MODE;\nSELECT 'held';\n")
            holder.stdin.flush()
            assert holder.stdout.readline().strip() == "held"

            def waiting_for_snapshot():
                return int(command(psql, "-XqAtw", "-c",
                    "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' "
                    "AND query LIKE '%FROM gateway_export_snapshots%'",
                    cwd=directory, env=pg_env))

            admitted = [reads.submit(call, port, path, 200, headers=auth) for _ in range(4)]
            eventually(lambda: waiting_for_snapshot() == 4, "four held export reads", timeout=3)
            excess = [reads.submit(call, port, path, 503, headers=auth) for _ in range(6)]

            def backpressure_ready():
                count = waiting_for_snapshot()
                assert count < 10, "Export downloads occupied all ten gateway database connections"
                return all(task.done() for task in excess)

            eventually(backpressure_ready, "immediate backpressure for excess exports", timeout=3)
            assert waiting_for_snapshot() == 4
            responses = [task.result() for task in excess]
            responses.append(call(port, f"/v1/exports/events?project_id={project}", 503, headers=auth))
            responses.append(call(port, "/v1/exports", 503, method="POST", headers=auth, body={"project_id": project}))
            for payload, headers in responses:
                assert payload["error"]["code"] == "export_capacity"
                assert headers.get("Retry-After") == "2"
                assert headers.get("Cache-Control") == "private, no-store"

            receipt, _ = call(port, "/v1/image", 202, method="POST", headers={**auth, "Idempotency-Key": token_hex(16)},
                              body={"model": "example_image", "task": "text_to_image", "prompt": "Export contention fixture",
                                    "count": 1, "aspect_ratio": "1:1"})
            call(port, receipt["status_url"], 200, headers=auth)
            assert waiting_for_snapshot() == 4
        finally:
            holder.stdin.close()
            holder.wait(timeout=5)
        assert all(task.result(timeout=5)[0] == expected for task in admitted)
    wait_for_status(port, receipt, auth, "succeeded", 18)
    assert call(port, path, 200, headers=auth)[0] == expected
    print("PASS export backpressure capped blocked reads and preserved admission, status and exact snapshot bytes")


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
        workers = stack.enter_context(ExitStack())
        workers.enter_context(service("archive-worker", [gateway], directory, worker, lambda: ready(worker_port)))
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
            return call(port, feed + "?" + urlencode({"project_id": project, **query}),
                        200, headers=auth, max_bytes=MAX_ARCHIVE_BYTES)

        def published():
            body, _ = read_feed(project_reader)
            completed = {event["request_id"] for event in map(json.loads, body.splitlines())
                         if event["type"] == "request.succeeded"}
            return completed == set(receipts)

        eventually(published, "event publication")
        advance_fixture_clock("UPDATE gateway_export_events "
                              "SET data = data || jsonb_build_object('fixture_padding', repeat('t', 5 * 1024 * 1024)) "
                              "WHERE request_id = :'request' AND event_type = 'request.succeeded';",
                              request=receipts[1])
        advance_fixture_clock("UPDATE gateway_export_events SET occurred_at = now() - interval '3 days' "
                              "WHERE project_id = :'project';", project=project)
        expected, expected_headers = read_feed(clients[0])
        assert {event["request_id"] for event in map(json.loads, expected.splitlines())} == {receipts[0]}
        assert receipts[1].encode() not in expected and b"Private fixture" not in expected
        expected_project, _ = read_feed(project_reader)
        first_page, first_headers = read_feed(clients[0], limit=1)
        after_first = first_headers["X-TokenCraft-Next-After"]
        second_page, _ = read_feed(clients[0], after=after_first)
        assert first_headers["X-TokenCraft-Has-More"] == "true"
        assert first_page + second_page == expected

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
        assert read_feed(project_reader)[0] == expected_project
        archived_first, archived_headers = read_feed(clients[0], limit=1)
        assert archived_first == first_page
        for name in ("X-TokenCraft-Has-More", "X-TokenCraft-Next-After", "X-TokenCraft-Content-Sha256"):
            assert archived_headers[name] == first_headers[name]
        assert read_feed(clients[0], after=after_first)[0] == second_page
        print("PASS the automatic archive preserved the exact client feed after PostgreSQL retirement")

        archive = f"/v1/archives/{archive_id}"
        with bucket.lock:
            object_path, = (path for path in bucket.objects if archive_id in path)
            sealed_object = bucket.objects[object_path]
            bucket.objects[object_path] = sealed_object[:-1] + bytes([sealed_object[-1] ^ 1])
        try:
            response, headers = call(port, feed + f"?project_id={project}&limit=1", 500, headers=clients[0])
            assert response["error"]["code"] == "internal_error"
            assert "X-TokenCraft-Next-After" not in headers
        finally:
            with bucket.lock:
                bucket.objects[object_path] = sealed_object
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
        archive_data, archive_headers = call(port, archive + "/data", 200, headers=project_reader,
                                             max_bytes=MAX_ARCHIVE_BYTES)
        assert archive_headers["X-TokenCraft-Content-Sha256"] == sha256(archive_data).hexdigest()
        assert archive_data == expected_project
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
        next_key = token_hex(32)
        read_keys = "TOKENCRAFT_DEMO_ARCHIVE_READ_KEYS"
        write_key = "TOKENCRAFT_DEMO_ARCHIVE_KEY"
        old_data = archive + "/data"
        rotated = {write_key: next_key, read_keys: settings[write_key]}

        def additional_reader(name, keys):
            api_port = loopback_port()
            stack.enter_context(service(name, [gateway], directory,
                                        {**settings, **keys, "API_GATEWAY_PORT": str(api_port)},
                                        lambda: ready(api_port)))
            return api_port

        preloaded = additional_reader("archive-api-preloaded", {read_keys: next_key})
        promoted = additional_reader("archive-api-promoted", rotated)
        historical, _ = call(promoted, old_data, 200, headers=project_reader, max_bytes=MAX_ARCHIVE_BYTES)
        assert historical == expected_project

        workers.close()
        workers.enter_context(service("archive-worker-promoted", [gateway], directory, {**worker, **rotated},
                                      lambda: ready(worker_port)))
        archive_scope = {"account_id": account, "project_id": project}
        snapshot, _ = call(port, "/v1/exports", 201, method="POST", headers=session,
                           body={**archive_scope, "client_ref": "studio-first"})
        queued, _ = call(port, "/v1/archives", 202, method="POST", headers=session,
                         body={**archive_scope, "export_id": snapshot["id"]})
        new_archive = f"/v1/archives/{queued['id']}"
        eventually(lambda: call(port, new_archive, 200, headers=clients[0])[0]["stored_at"],
                   "seal scoped history with the promoted key")
        call(port, new_archive + "/restore", 202, method="POST", headers=clients[0])
        eventually(lambda: call(port, new_archive, 200, headers=clients[0])[0]["state"] == "available",
                   "verify the new archive under the promoted worker")
        new_data = new_archive + "/data"
        for reader_port in (preloaded, promoted):
            body, headers = call(reader_port, new_data, 200, headers=clients[0])
            assert body == expected
            assert headers["X-TokenCraft-Content-Sha256"] == sha256(expected).hexdigest()
            call(reader_port, new_data, 404, headers=clients[1])
        call(port, new_data, 500, headers=clients[0])
        print("PASS staged archive rotation preserved retired history and scoped reads across both key assignments")
        for setting in (write_key, read_keys):
            invalid = {**settings, setting: b"fixture-secret-marker\xff", "API_GATEWAY_PORT": str(loopback_port())}
            rejected = subprocess.run([gateway], cwd=directory, env=invalid, stdin=subprocess.DEVNULL,
                                      capture_output=True, timeout=10)
            assert rejected.returncode != 0 and setting.encode() in rejected.stderr
            assert b"fixture-secret-marker" not in rejected.stdout + rejected.stderr
        print("PASS malformed archive keys failed startup without exposing the supplied bytes")
        verify_archive_download_capacity(port, old_data, project_reader, clients[0], project, archive_data, first_page)
        verify_export_backpressure(port, directory, psql, pg_env, project, clients[0])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-event-archives-") as directory:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(directory), verification=verify)


if __name__ == "__main__":
    main()
