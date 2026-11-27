"""Verify staged request content encryption rotation across API and worker replicas."""

import argparse
from contextlib import ExitStack
from hashlib import sha256
import json
from pathlib import Path
from secrets import token_hex
from tempfile import TemporaryDirectory
from urllib.parse import urlsplit

import smoke_workflow_contract as fixture
from postgres_tools import postgres_bin
from smoke_cell_faults import Connector, ConnectorHandler, PIXELS, eventually, serve
from smoke_connector_recovery import wait_for_status


class ContentConnector(ConnectorHandler):
    def do_POST(self):
        if self.path != "/v1/jobs" or not self.authorized():
            return self.reply(404, {})
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        request = payload["request_id"]
        assert self.headers["Idempotency-Key"] == request
        with self.server.lock:
            self.server.jobs[request] += 1
            self.server.grants[request] = payload["completion"]
        self.reply(200, {"version": 1, "request_id": request, "outcome": {"status": "accepted", "handle": request}})


def verify(gateway, directory, settings, session, account, project):
    port = int(settings["API_GATEWAY_PORT"])
    archive_dir = directory / "archive"
    archive_dir.mkdir(mode=0o700)
    active_key = token_hex(32)
    previous_key = settings["TOKENCRAFT_DEMO_PAYLOAD_KEY"]
    content_key = "TOKENCRAFT_CONTENT_ENCRYPTION_KEY"
    read_keys = "TOKENCRAFT_CONTENT_ENCRYPTION_READ_KEYS"
    db = urlsplit(settings["DATABASE_URL"])
    database = dict(PATH=settings["PATH"], PGHOST=db.hostname, PGPORT=str(db.port), PGUSER=db.username,
                    PGPASSWORD=db.password, PGDATABASE=db.path.lstrip("/"))
    archive_early = directory / "archive-early.sql"
    archive_early.write_text("UPDATE gateway_content_history SET archive_at = now() - interval '1 second' "
                             "WHERE request_id = :'request';\n")
    media = {"model": "example_image", "task": "text_to_image", "count": 1, "aspect_ratio": "1:1"}

    with ExitStack() as apps:
        connector = Connector()
        connector.RequestHandlerClass = ContentConnector
        connector.grants = {}
        apps.enter_context(serve(connector))
        binding = directory / "bindings.json"
        binding.write_text(json.dumps({"version": 1, "bindings": [{
            "name": "example", "revision": "rotation-fixture", "origin": f"http://{fixture.HOST}:{connector.server_port}/",
            "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE", "completion_webhooks": True,
        }]}))
        options = dict(settings, TOKENCRAFT_CORE_CATALOG=str(fixture.ROOT / "examples/connector/catalog.json"),
                       TOKENCRAFT_CONNECTORS=str(binding), TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE=connector.token,
                       TOKENCRAFT_CONTENT_ARCHIVE_DIR=str(archive_dir))
        options.pop("TOKENCRAFT_LOCAL_CHAT_BINDINGS", None)

        def spawn(stack, name, binary, listen, role="api", **keys):
            env = dict(options, API_GATEWAY_PORT=str(listen), APP_BASE_URL=f"http://localhost:{listen}",
                       TOKENCRAFT_PROCESS_ROLE=role, **keys)
            stack.enter_context(fixture.service(name, [binary], directory, env, lambda: fixture.ready(listen)))

        def grant(client):
            row, _ = fixture.call(port, f"/v1/projects/{project}/service-principals?account_id={account}",
                201, method="POST", headers=session, body={"name": client, "client_ref": client,
                    "scopes": ["requests:submit", "requests:read"]})
            return {"Authorization": "Bearer " + row["token"]}

        def submit(api, label, status):
            return fixture.call(api, "/v1/image", status, method="POST",
                headers={**auth, "Idempotency-Key": f"rotation-{label}"}, body={**media, "prompt": label})[0]

        def request_path(receipt):
            return f"/v1/requests/{receipt['id']}"

        def assert_media(api, receipt, label, archived=False):
            path = request_path(receipt)
            route = path + "/content"
            view, _ = fixture.call(api, route + "/retrieve" if archived else route,
                                   200, method="POST" if archived else "GET", headers=auth)
            assert view["input"]["prompt"] == label
            assert view["phase"] == ("archived" if archived else "available")
            assert view["result"]["images"][0]["artifact"]["sha256"] == sha256(PIXELS).hexdigest()
            path = (route if archived else path) + "/artifacts/0"
            output, headers = fixture.call(api, path, 200, headers=auth)
            assert output == PIXELS and headers["X-TokenCraft-Content-Sha256"] == sha256(PIXELS).hexdigest()

        def archive(api, receipt):
            fixture.command(postgres_bin() / "psql", "-XqAtw", "-v", "ON_ERROR_STOP=1", "-v",
                            f"request={receipt['id']}", "-f", archive_early, cwd=directory, env=database)
            eventually(lambda: fixture.call(api, request_path(receipt) + "/content", 200, headers=auth)[0]["phase"]
                       == "archived", "the archive seal", timeout=15)

        with ExitStack() as old:
            spawn(old, "initial-api", gateway, port)
            auth, sibling = grant("studio-a"), grant("studio-b")
            with ExitStack() as workers:
                spawn(workers, "initial-worker", gateway, fixture.loopback_port(), "worker")
                old_archive = submit(port, "old-archive", 202)
                old_warm = submit(port, "old-warm", 202)
                for receipt in (old_archive, old_warm):
                    wait_for_status(port, receipt, auth, "succeeded", 15)
                archive(port, old_archive)
                assert_media(port, old_archive, "old-archive", archived=True)
                assert_media(port, old_warm, "old-warm")
                old_callback = connector.grants[old_warm["id"]]
            pending = submit(port, "pending-old-input", 202)
            assert fixture.call(port, pending["status_url"], 200, headers=auth)[0]["status"] == "queued"

        spawn(apps, "staged-api", gateway, port, **{read_keys: active_key})
        peer = fixture.loopback_port()
        promoted = {content_key: active_key, read_keys: previous_key}
        spawn(apps, "promoted-api", gateway, peer, **promoted)
        spawn(apps, "promoted-worker", gateway, fixture.loopback_port(), "worker", **promoted)
        assert submit(peer, "pending-old-input", 200)["id"] == pending["id"]
        fixture.call(peer, old_callback["path"], 204, method="POST", body={"version": 1},
                     headers={"Authorization": "Bearer " + old_callback["token"]})
        wait_for_status(peer, pending, auth, "succeeded", 15)
        assert_media(peer, pending, "pending-old-input")
        print("PASS old queued work, receipt replay, and an issued callback survived promotion", flush=True)

        new = submit(peer, "new-write-key", 202)
        wait_for_status(port, new, auth, "succeeded", 15)
        for api in (port, peer):
            assert_media(api, old_warm, "old-warm")
            assert_media(api, new, "new-write-key")
            assert_media(api, old_archive, "old-archive", archived=True)
            fixture.call(api, request_path(new) + "/artifacts/0", 404, headers=sibling)
        archive(peer, pending)
        assert_media(port, pending, "pending-old-input", archived=True)
        assert_media(peer, pending, "pending-old-input", archived=True)
        print("PASS both rollout configurations read old, new, and mixed-key archives with client isolation", flush=True)

        canary = fixture.loopback_port()
        spawn(apps, "new-only-api", gateway, canary, **{content_key: active_key})
        assert_media(canary, new, "new-write-key")
        fixture.call(canary, request_path(old_warm) + "/artifacts/0", 500, headers=auth)
        assert connector.jobs == {receipt["id"]: 1 for receipt in (old_archive, old_warm, pending, new)}
        print("PASS the new key wrote fresh content and each admitted job dispatched once", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-content-rotation-") as temp:
        fixture.rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(temp),
                         verification=verify)


if __name__ == "__main__":
    main()
