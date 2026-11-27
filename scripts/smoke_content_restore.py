"""Exercise scoped content retrieval through a stalled and unconfirmed bucket response."""

import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor, TimeoutError
from contextlib import ExitStack
from hashlib import sha256
import json
from pathlib import Path
from secrets import token_hex
from tempfile import TemporaryDirectory
from threading import Event
from urllib.parse import urlencode, urlsplit

from postgres_tools import postgres_bin
import smoke_artifact_downloads as media
from smoke_cell_faults import BucketHandler, Connector, HeldBucket, eventually, serve
from smoke_connector_recovery import wait_for_status
from smoke_workflow_contract import HOST, call, command, loopback_port, ready, rehearse, service


class RestoreBucketHandler(BucketHandler):
    def do_GET(self):
        path = self.signed_path()
        if not path:
            self.reply(403)
            return
        with self.server.lock:
            data = self.server.objects.get(path)
            cold = path in self.server.cold
            hold = self.server.hold_get == path
            if hold:
                self.server.hold_get = None
        if hold:
            self.server.entered.set()
            self.server.release_get.wait(12)
            self.server.checked.set()
        if cold:
            self.reply(403, b"<Error><Code>InvalidObjectState</Code></Error>")
        else:
            self.reply(404 if data is None else 200, data or b"")

    def do_POST(self):
        req = urlsplit(self.path)
        body = self.rfile.read(min(4096, int(self.headers.get("Content-Length", "0"))))
        if (req.query != "restore"
                or not self.headers.get("Authorization", "").startswith("AWS4-HMAC-SHA256 Credential=fixture-access/")
                or self.headers.get("X-Amz-Content-Sha256") != sha256(body).hexdigest()
                or b"<Days>1</Days>" not in body or b"<Tier>Standard</Tier>" not in body):
            self.reply(400)
            return
        with self.server.lock:
            self.server.posts[req.path] += 1
        # Simulate accepted retrieval with a lost acknowledgement.
        self.reply(503)

    def do_HEAD(self):
        req = urlsplit(self.path)
        with self.server.lock:
            if (req.path not in self.server.objects
                    or not self.headers.get("Authorization", "").startswith("AWS4-HMAC-SHA256 Credential=fixture-access/")):
                self.reply(404)
                return
            self.server.heads[req.path] += 1
            restoring = self.server.posts[req.path] > 0
        self.send_response(200)
        if restoring:
            self.send_header("x-amz-restore", 'ongoing-request="true"')
        self.send_header("Content-Length", "0")
        self.end_headers()


def verify(gateway, directory, settings, session, account, project):
    media.DURATION = 1
    port, worker_port = int(settings["API_GATEWAY_PORT"]), loopback_port()
    with ExitStack() as stack:
        bucket = HeldBucket()
        bucket.RequestHandlerClass = RestoreBucketHandler
        bucket.cold, bucket.posts, bucket.heads = set(), Counter(), Counter()
        bucket.hold_get = None
        bucket.entered, bucket.release_get, bucket.checked = Event(), Event(), Event()
        stack.enter_context(serve(bucket))
        connector = Connector()
        connector.RequestHandlerClass = media.AudioConnectorHandler
        connector.audio = media.make_audio()
        stack.enter_context(serve(connector))
        bindings = directory / "restore-bindings.json"
        bindings.write_text(json.dumps({"version": 1, "bindings": [{
            "name": "audio", "revision": "restore-fixture",
            "origin": f"http://{HOST}:{connector.server_port}/", "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_AUDIO",
        }]}))
        catalog = directory / "restore-catalog.json"
        catalog.write_text(json.dumps({"version": 1, "offers": [{
            "id": "fixture_audio", "provider": "self_hosted", "model": "fixture_audio",
            "adapter": "connector_queue", "upstream_model": "fixture-audio", "local_binding": "audio",
            "pricing": {"kind": "audio", "usd_nanos_per_second": 0, "durations_seconds": [1], "format": "wav"},
        }]}))
        settings = dict(settings, TOKENCRAFT_CORE_CATALOG=str(catalog), TOKENCRAFT_CONNECTORS=str(bindings),
                        TOKENCRAFT_CONNECTOR_TOKEN_AUDIO=connector.token,
                        TOKENCRAFT_CONTENT_ARCHIVE_S3_ENDPOINT=f"http://{HOST}:{bucket.server_port}",
                        TOKENCRAFT_CONTENT_ARCHIVE_S3_BUCKET="fixture-bucket",
                        TOKENCRAFT_CONTENT_ARCHIVE_S3_REGION="us-east-1",
                        TOKENCRAFT_CONTENT_ARCHIVE_S3_ACCESS_KEY="fixture-access",
                        TOKENCRAFT_CONTENT_ARCHIVE_S3_SECRET_KEY=bucket.secret,
                        TOKENCRAFT_CONTENT_ARCHIVE_S3_AUTO_RESTORE="1")
        settings.pop("TOKENCRAFT_LOCAL_CHAT_BINDINGS", None)
        stack.enter_context(service("restore-api", [gateway], directory, settings, lambda: ready(port)))
        worker = dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port))
        stack.enter_context(service("restore-worker", [gateway], directory, worker, lambda: ready(worker_port)))
        db = urlsplit(settings["DATABASE_URL"])
        db_env = dict(PATH=settings["PATH"], PGHOST=db.hostname, PGPORT=str(db.port), PGUSER=db.username,
                      PGPASSWORD=db.password, PGDATABASE=db.path.lstrip("/"))
        psql = postgres_bin() / "psql"
        scope = "?" + urlencode({"account_id": account, "project_id": project})
        admin_scope = "?" + urlencode({"account_id": account})

        def query(request, sql):
            statement = directory / "restore-fixture.sql"
            statement.write_text(sql + "\n")
            return command(psql, "-XqAtw", "-v", "ON_ERROR_STOP=1", "-v", f"request={request}",
                           "-f", statement, cwd=directory, env=db_env)

        def auth_for(client):
            row, _ = call(port, f"/v1/projects/{project}/service-principals" + admin_scope, 201,
                          method="POST", headers=session, body={"name": client, "client_ref": client,
                                                               "scopes": ["requests:submit", "requests:read"]})
            return {"Authorization": "Bearer " + row["token"]}

        auth = auth_for("client-a")
        sibling = auth_for("client-b")

        def archived_request():
            receipt, _ = call(port, "/v1/audio", 202, method="POST",
                              headers={**auth, "Idempotency-Key": token_hex(16)},
                              body={"task": "text_to_music", "model": "fixture_audio",
                                    "prompt": "Restore test audio", "duration_seconds": 1})
            wait_for_status(port, receipt, auth, "succeeded", 18)
            rid = receipt["id"]
            query(rid, "UPDATE gateway_content_history SET archive_at = clock_timestamp() - interval '1 second' "
                       "WHERE request_id = :'request';")
            base = f"/v1/requests/{rid}/content"
            eventually(lambda: call(port, base, 200, headers=auth)[0]["phase"] == "archived",
                       "the archive is sealed", timeout=16)
            manifest = f"/fixture-bucket/tokencraft-content/v1/{rid}.tch"
            artifact = f"/fixture-bucket/tokencraft-content/v1/{rid}-0.tcm"
            with bucket.lock:
                assert all(path in bucket.objects for path in (manifest, artifact))
                bucket.cold.update((manifest, artifact))
            return rid, base, (manifest, artifact)

        removed, history, removed_paths = archived_request()
        bucket.hold_get = removed_paths[0]
        call(port, history + "/restore" + scope, 202, method="POST", headers=session, body={})
        assert bucket.entered.wait(8), "restore worker did not request the manifest"
        with ThreadPoolExecutor(max_workers=1) as requests:
            remove = requests.submit(call, port, f"/v1/projects/{project}/requests/{removed}/content" + admin_scope,
                                     200, method="DELETE", headers=session)
            try:
                remove.result(timeout=2.5)
            except TimeoutError as error:
                raise AssertionError("Request removal waited on the stalled archive GET") from error
            finally:
                bucket.release_get.set()
        assert bucket.checked.wait(4)
        assert call(port, history, 200, headers=auth)[0]["phase"] == "removed"
        call(port, history + "/artifacts/0", 404, headers=auth)
        assert query(removed, "SELECT count(*) FROM gateway_content_restores WHERE request_id = :'request';") == "0"
        print("PASS request removal committed while the archive read remained stalled")

        kept, history, kept_paths = archived_request()
        bucket.entered.clear()
        bucket.release_get.clear()
        bucket.checked.clear()
        bucket.hold_get = kept_paths[0]
        call(port, history + "/restore", 403, method="POST", headers=auth, body={})
        first, _ = call(port, history + "/restore" + scope, 202, method="POST", headers=session, body={})
        repeat, _ = call(port, history + "/restore" + scope, 202, method="POST", headers=session, body={})
        assert first["requested_at"] == repeat["requested_at"] and first["total_objects"] == 2
        call(port, history, 404, headers=sibling)
        assert bucket.entered.wait(8), "the original worker did not reach the manifest"
        due = "UPDATE gateway_content_restores SET check_at = clock_timestamp() - interval '1 second' "
        replacement_port = loopback_port()
        replacement = dict(worker, API_GATEWAY_PORT=str(replacement_port))
        reserved = "SELECT count(*) FROM gateway_content_restores WHERE request_id = :'request'"
        try:
            stack.enter_context(service("replacement-worker", [gateway], directory, replacement,
                                        lambda: ready(replacement_port)))
            eventually(lambda: bucket.posts[kept_paths[1]] == 1, "another slot progressed during the read", timeout=6)
            assert bucket.posts[kept_paths[0]] == 0, "two workers claimed the same object"
            # Advance the stalled manifest's lease in the fixture instead of waiting 45 seconds.
            query(kept, due + "WHERE request_id = :'request' AND slot = -1;")
            eventually(lambda: query(kept, reserved + " AND retrieval_submissions = 1;") == "2",
                       "the replacement reserved each object once", timeout=14)
            eventually(lambda: all(bucket.posts[path] == 1 for path in kept_paths),
                       "each restore POST arrived", timeout=6)
        finally:
            bucket.release_get.set()
        assert bucket.checked.wait(4)
        query(kept, due + "WHERE request_id = :'request';")
        eventually(lambda: query(kept, reserved + " AND retrieval_confirmed;") == "2",
                   "HEAD reconciled the two unconfirmed submissions", timeout=14)
        with bucket.lock:
            assert all(bucket.posts[path] == 1 and bucket.heads[path] == 1 for path in kept_paths)
            assert all(bucket.posts[path] == 0 for path in removed_paths)
            bucket.cold.difference_update(kept_paths)
        query(kept, due + "WHERE request_id = :'request';")
        eventually(lambda: call(port, history, 200, headers=auth)[0]["restore"]["state"] == "ready",
                   "both restored ciphertext checks passed", timeout=14)
        file, _ = call(port, history + "/artifacts/0", 200, headers=auth)
        assert file == connector.audio
        call(port, history + "/artifacts/0", 404, headers=sibling)
        print("PASS the replacement recovered the expired claim, reconciled both POSTs, and restored exact scoped media")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-content-restore-") as folder:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(folder), verification=verify)


if __name__ == "__main__":
    main()
