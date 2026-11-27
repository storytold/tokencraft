"""Exercise snapshot import and continuous sync using a disposable connector demo."""

from contextlib import contextmanager
from hashlib import sha256
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
from secrets import token_hex, token_urlsafe
import subprocess
import sys
from threading import Event, Thread
import time
from urllib.error import HTTPError
from urllib.parse import urlencode, urlsplit
from urllib.request import Request

import export_to_postgres as importer


HERE = Path(__file__).resolve().parent
BASE = os.environ.get("TOKENCRAFT_EXPORT_API_URL", "http://localhost:4000").rstrip("/")
PROMPT = "Synthetic private prompt for the export smoke"


def http(path, expected=200, *, body=None, token=None, cookie=None, idempotent=False, method=None):
    headers = {}
    if body is not None:
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = f"Bearer {token}"
    if cookie:
        headers["Cookie"] = cookie
    if idempotent:
        headers["Idempotency-Key"] = token_hex(16)
    data = json.dumps(body).encode() if body is not None else None
    request = Request(BASE + path, data=data, headers=headers, method=method)
    try:
        response = importer.OPENER.open(request, timeout=10)
    except HTTPError as error:
        response = error
    with response:
        assert response.status == expected, f"{request.get_method()} {path}: HTTP {response.status}"
        return json.loads(response.read()), response.headers


def wait_for(probe, label):
    deadline = time.monotonic() + 25
    while time.monotonic() < deadline:
        if result := probe():
            return result
        time.sleep(0.2)
    raise AssertionError(f"Timed out waiting for {label}")


def execute(token, app):
    quote, _ = http("/v1/quotes/scoped", token=token, body={
        "model": "example_image",
        "workload": {"kind": "image", "count": 1, "aspect_ratio": "4:3"},
    })
    receipt, _ = http("/v1/requests", 202, token=token, idempotent=True, body={
        "input": quote["input"], "quote_id": quote["id"], "image_prompt": PROMPT, "app_ref": app,
    })

    def succeeded():
        result, _ = http(receipt["status_url"], token=token)
        state = result["status"]
        assert state not in {"failed", "canceled", "awaiting_reconciliation"}, state
        return state == "succeeded"

    wait_for(succeeded, "the synthetic connector")
    return receipt["id"]


@contextmanager
def interrupt_live_export(origin):
    armed = Event()
    interrupted = Event()
    retry_started = Event()
    release_retry = Event()
    affected_paths = []

    class Relay(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def forward(self):
            assert self.path.startswith("/v1/exports")
            body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
            headers = {name: value for name, value in self.headers.items()
                       if name.lower() not in {"host", "connection"}}
            upstream = HTTPConnection(origin.hostname, origin.port, timeout=10)
            try:
                upstream.request(self.command, self.path, body=body, headers=headers)
                reply = upstream.getresponse()
                payload = reply.read()
                status = reply.status
                response_headers = reply.getheaders()
            finally:
                upstream.close()
            size = len(payload)
            if armed.is_set() and self.path.startswith("/v1/exports/events?") and status == 200 and payload:
                if not interrupted.is_set():
                    affected_paths.append(self.path)
                    payload = payload[:size // 2]
                    interrupted.set()
                elif not retry_started.is_set():
                    affected_paths.append(self.path)
                    retry_started.set()
                    release_retry.wait(timeout=8)
            self.send_response(status)
            for name, value in response_headers:
                if name.lower() not in {"connection", "content-length", "transfer-encoding", "date", "server"}:
                    self.send_header(name, value)
            self.send_header("Content-Length", str(size))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(payload)

        do_GET = forward
        do_POST = forward

    with ThreadingHTTPServer(("127.0.0.1", 0), Relay) as server:
        thread = Thread(target=server.serve_forever)
        thread.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}", armed, retry_started, release_retry, affected_paths
        finally:
            release_retry.set()
            server.shutdown()
            thread.join(timeout=5)


def main():
    origin = urlsplit(BASE)
    assert __debug__ and origin.scheme == "http" and origin.hostname in ("127.0.0.1", "localhost", "::1")
    assert not (origin.username or origin.password or origin.path or origin.query or origin.fragment)
    assert os.environ.get("PGHOST") in {"127.0.0.1", "localhost", "::1"} and os.environ.get("PGDATABASE")
    assert importer.postgres(
        "SELECT to_regnamespace('tokencraft_export') IS NULL AND to_regclass('gateway_requests') IS NULL;", {}
    ) == "t", "Point the smoke at a fresh disposable destination database."
    with interrupt_live_export(origin) as relay:
        verify_import(*relay)


def verify_import(source, armed, retry_started, release_retry, affected_paths):
    _, headers = http("/v1/auth/signup", 201, body={
        "email": f"export-{token_hex(8)}@example.test",
        "password": token_urlsafe(24),
        "display_name": "Customer data smoke",
    })
    cookie = headers["Set-Cookie"].split(";", 1)[0]
    accounts, _ = http("/v1/accounts", cookie=cookie)
    org = accounts["accounts"][0]["account_id"]
    project, _ = http(f"/v1/projects?account_id={org}", 201, cookie=cookie, body={"name": "Export smoke"})
    project_id = project["id"]

    def key(name, client, scopes):
        principal, _ = http(f"/v1/projects/{project_id}/service-principals?account_id={org}", 201, cookie=cookie,
                            body={"name": name, "client_ref": client, "scopes": scopes})
        return principal["token"]

    art = key("ArtCraft client", "art-client", ["requests:submit", "requests:read", "usage:read"])
    film = key("FilmCraft client", "film-client", ["requests:submit", "requests:read"])
    export_key = key("Import writer", "art-client", ["exports:read"])
    art_request = execute(art, "artcraft")
    film_request = execute(film, "filmcraft")

    def settle(request, amount, revision):
        path = f"/v1/projects/{project_id}/requests/{request}/settlements?account_id={org}"
        http(path, 201, cookie=cookie, idempotent=True, body={
            "amount_usd_nanos": str(amount), "expected_revision": revision, "evidence_ref": "fixture-statement",
        })

    initial = 9_007_199_254_740_995
    corrected = initial - 8
    settle(art_request, initial, 0)
    settle(film_request, 21, 0)
    scope = urlencode({"project_id": project_id, "client_ref": "art-client"})
    http(f"/v1/exports/events?{scope}", 403, token=art)
    http(f"/v1/exports/events?project_id={project_id}&client_ref=film-client", 404, token=export_key)

    env = dict(os.environ, TOKENCRAFT_EXPORT_API_URL=source, TOKENCRAFT_EXPORT_PROJECT=project_id,
               TOKENCRAFT_EXPORT_API_KEY=export_key, TOKENCRAFT_EXPORT_CLIENT_REF="art-client")
    command = [sys.executable, "-B", str(HERE / "export_to_postgres.py")]

    def run(*args, child_env=env):
        return subprocess.check_output(command + list(args), env=child_env, text=True, timeout=25).strip()

    status_env = env.copy()
    del status_env["TOKENCRAFT_EXPORT_API_KEY"]

    def status():
        return json.loads(run("--status", child_env=status_env))

    assert run().startswith("Imported ")
    snapshot = status()
    assert snapshot["after_event_id"].startswith("evt_")
    assert run("--no-init") == "Imported 0 new events."

    for _ in range(2):
        importer.postgres((HERE / "postgres_usage_views.sql").read_text(encoding="utf-8"), {})
    assert status() == snapshot
    local_events = json.loads(importer.postgres('SELECT json_agg(payload) FROM tokencraft_export.events;', {}))
    assert {event["request_id"] for event in local_events} == {art_request}
    assert {event["client_ref"] for event in local_events} == {"art-client"}
    assert export_key not in json.dumps(local_events) and PROMPT not in json.dumps(local_events)
    print("PASS Snapshot import checkpointed only the authorized client and excluded request content.", flush=True)

    watcher = subprocess.Popen(command + ["--no-init", "--watch", "--interval", "1"],
                               env=env, stdout=subprocess.DEVNULL)
    try:
        wait_for(lambda: status()["caught_up_as_of"], "watcher startup")
        assert status()["after_event_id"] == snapshot["after_event_id"]
        armed.set()
        settle(art_request, corrected, 1)

        def retried():
            assert watcher.poll() is None, "Watcher exited after the interrupted page"
            return retry_started.is_set()

        wait_for(retried, "retry of the interrupted event page")
        assert len(affected_paths) == 2 and affected_paths[0] == affected_paths[1]
        assert status()["after_event_id"] == snapshot["after_event_id"]
        assert importer.postgres("SELECT count(*) FROM tokencraft_export.events;", {}) == str(len(local_events))
        release_retry.set()

        def checkpoint_advanced():
            value = status()
            assert watcher.poll() is None, "Watcher exited"
            if value["after_event_id"] != snapshot["after_event_id"] and value["has_more"] is False:
                return value
            return None

        final = wait_for(checkpoint_advanced, "settlement checkpoint")
    finally:
        release_retry.set()
        watcher.terminate()
        watcher.wait(timeout=5)
    assert run("--no-init") == "Imported 0 new events."
    postings = json.loads(importer.postgres("""
        SELECT json_agg(json_build_array(revision, amount_usd_nanos, delta_usd_nanos) ORDER BY revision)
        FROM tokencraft_export.supplier_settlement_postings;
    """, {}))
    assert postings == [[1, initial, initial], [2, corrected, -8]]
    usage, = json.loads(importer.postgres("SELECT json_agg(u) FROM tokencraft_export.request_usage u;", {}))
    live = http("/v1/usage", token=art)[0]["totals"]
    assert usage["app_ref"] == "artcraft" and usage["client_ref"] == "art-client"
    assert usage["supplier_settlement_revision"] == 2
    assert usage["supplier_settled_usd_nanos"] == int(live["supplier_settled_usd_nanos"]) == corrected
    assert usage["estimated_usd_nanos"] == int(live["estimated_usd_nanos"])
    assert usage["completed_outputs"] == live["completed_outputs"] == 1
    print("PASS Watcher retried the interrupted page from the same cursor and reconciled the correction once.", flush=True)

    binding = {"source": source, "project": project_id, "client": "art-client"}
    cursor = final["after_event_id"]
    current = status()
    count = importer.import_page([], binding, cursor, cursor, as_of="2000-01-01T00:00:00Z",
                                 more="false", pending="true")
    assert count == 0 and status() == current
    try:
        importer.import_page([], binding, snapshot["after_event_id"], snapshot["after_event_id"])
    except importer.SyncError as error:
        assert error.retryable
    else:
        raise AssertionError("A stale writer was allowed to change the committed checkpoint.")
    assert status() == current
    print("PASS Stale writes preserved the committed cursor and the latest feed status.", flush=True)

    original = next(event for event in local_events if event["id"] != cursor)
    project_feed = {**binding, "client": ""}
    assert importer.load_checkpoint(project_feed, importer.REGISTER) == ""
    assert importer.import_page([original], project_feed, "", original["id"]) == 0
    assert importer.load_checkpoint(project_feed) == original["id"]

    def read_page(events):
        payload = b"".join(json.dumps(event).encode() + b"\n" for event in events)
        return importer.read_page(payload, {
            "X-TokenCraft-Content-Sha256": sha256(payload).hexdigest(),
            "X-TokenCraft-Has-More": "false",
            "X-TokenCraft-Next-After": events[-1]["id"],
        }, binding, cursor)

    try:
        read_page([original, original])
    except importer.SyncError as error:
        assert "repeats an event ID" in str(error)
    else:
        raise AssertionError("A page with a repeated event ID passed validation.")

    fresh = {**original, "id": "evt_" + token_hex(16)}
    conflicting = {**original, "data": {**original["data"], "fixture_conflict": True}}
    records, next_after, more = read_page([conflicting, fresh])
    before = importer.postgres("SELECT count(*) FROM tokencraft_export.events;", {})
    try:
        importer.import_page(records, binding, cursor, next_after,
                             as_of="2099-01-01T00:00:00Z", more=more, pending="false")
    except importer.SyncError as error:
        assert not error.retryable and "different content" in str(error)
    else:
        raise AssertionError("The importer accepted changed content for a committed event.")
    assert status() == current
    assert importer.postgres("SELECT count(*) FROM tokencraft_export.events;", {}) == before
    print("PASS Overlapping feeds deduplicated identical events and a changed event rolled back the page.", flush=True)

    source_database = os.environ.get("TOKENCRAFT_SMOKE_EXPIRY_DATABASE")
    if source_database:
        assert source_database != os.environ["PGDATABASE"], "Use a disposable gateway database separate from the destination."
        manifest, _ = http(f"/v1/requests/{art_request}/result", token=art)
        artifact = manifest["result"]["images"][0]["url"]
        subprocess.run(
            ["psql", "-Xwq", "-v", "ON_ERROR_STOP=1", "-v", f"request_id={art_request}"],
            env={**os.environ, "PGDATABASE": source_database}, text=True, check=True, timeout=10,
            input="""UPDATE gateway_provider_runs
                     SET result_expires_at = now() - interval '1 second'
                     WHERE request_id = :'request_id' AND response_payload IS NOT NULL;""",
        )
        http(f"/v1/requests/{art_request}/result", 404, token=art)
        http(artifact, 404, token=art)
        http(f"/v1/requests/{film_request}/result", token=film)

        def removed():
            snapshot, _ = http(f"/v1/requests/{art_request}", token=art)
            assert snapshot["status"] == "succeeded"
            return snapshot.get("content_removed_at")

        removed_at = wait_for(removed, "scheduled content removal")
        result, _ = http(f"/v1/projects/{project_id}/requests/{art_request}/content?account_id={org}",
                         method="DELETE", cookie=cookie)
        assert result["content_removed_at"] == removed_at
        assert run("--no-init") == "Imported 1 new events."
        assert run("--no-init") == "Imported 0 new events."
        removal = json.loads(importer.postgres("""
            SELECT json_agg(payload) FROM tokencraft_export.events
            WHERE payload->>'type' = 'request.content_removed';
        """, {}))
        event, = removal
        assert event["request_id"] == art_request and event["client_ref"] == "art-client"
        assert http("/v1/usage", token=art)[0]["totals"] == live
        print("PASS Expiry denied content reads and exported one scoped removal while preserving usage.", flush=True)


if __name__ == "__main__":
    main()
