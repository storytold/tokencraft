"""Verify backpressure and isolation for warm and retained media downloads."""

import argparse
from contextlib import ExitStack
from hashlib import sha256
from io import BytesIO
import json
from pathlib import Path
from secrets import token_hex
from tempfile import TemporaryDirectory
from urllib.parse import urlsplit
import wave

from postgres_tools import postgres_bin
from smoke_cell_faults import Connector, ConnectorHandler, eventually, serve
from smoke_connector_recovery import wait_for_status
from smoke_event_archives import slow_download
from smoke_workflow_contract import HOST, call, command, loopback_port, ready, rehearse, service

DURATION = 30
MAX_AUDIO_BYTES = 8 * 1024 * 1024


class AudioConnectorHandler(ConnectorHandler):
    def do_GET(self):
        if not self.path.endswith("/artifacts/0"):
            return super().do_GET()
        request = self.path.split("/")[3]
        with self.server.lock:
            known = request in self.server.jobs
        if known and self.authorized():
            return self.reply(200, self.server.audio, "audio/wav")
        return self.reply(404, {})


def make_audio():
    output = BytesIO()
    with wave.open(output, "wb") as data:
        data.setnchannels(2)
        data.setsampwidth(2)
        data.setframerate(48000)
        data.writeframes(bytes(48000 * 4 * DURATION))
    return output.getvalue()


def verify(gateway, directory, settings, session, account, project):
    port, worker_port = int(settings["API_GATEWAY_PORT"]), loopback_port()
    with ExitStack() as stack:
        connector = Connector()
        connector.RequestHandlerClass = AudioConnectorHandler
        connector.audio = make_audio()
        stack.enter_context(serve(connector))
        bindings = directory / "audio-bindings.json"
        bindings.write_text(json.dumps({"version": 1, "bindings": [{
            "name": "audio", "revision": "transfer-fixture",
            "origin": f"http://{HOST}:{connector.server_port}/", "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_AUDIO",
        }]}))
        catalog = directory / "audio-catalog.json"
        catalog.write_text(json.dumps({"version": 1, "offers": [{
            "id": "fixture_audio", "provider": "self_hosted", "model": "fixture_audio", "adapter": "connector_queue",
            "upstream_model": "fixture-audio", "local_binding": "audio",
            "pricing": {"kind": "audio", "usd_nanos_per_second": 0, "durations_seconds": [DURATION], "format": "wav"},
        }]}))
        archive = directory / "content-archive"
        archive.mkdir(mode=0o700)
        settings = dict(settings, TOKENCRAFT_CONTENT_ARCHIVE_DIR=str(archive), TOKENCRAFT_CORE_CATALOG=str(catalog),
                        TOKENCRAFT_CONNECTORS=str(bindings), TOKENCRAFT_CONNECTOR_TOKEN_AUDIO=connector.token)
        settings.pop("TOKENCRAFT_LOCAL_CHAT_BINDINGS", None)
        stack.enter_context(service("artifact-api", [gateway], directory, settings, lambda: ready(port)))
        worker = dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port))
        stack.enter_context(service("artifact-worker", [gateway], directory, worker, lambda: ready(worker_port)))

        def key(client):
            row, _ = call(port, f"/v1/projects/{project}/service-principals?account_id={account}", 201,
                          method="POST", headers=session, body={"name": client, "client_ref": client,
                                                               "scopes": ["requests:submit", "requests:read"]})
            return {"Authorization": "Bearer " + row["token"]}

        auth, sibling = key("client-a"), key("client-b")

        def submit():
            return call(port, "/v1/audio", 202, method="POST",
                        headers={**auth, "Idempotency-Key": token_hex(16)}, body={
                            "task": "text_to_music", "model": "fixture_audio", "prompt": "Synthetic audio fixture",
                            "duration_seconds": DURATION,
                        })[0]

        def read(path, status=200):
            return call(port, path, status, headers=auth, max_bytes=MAX_AUDIO_BYTES)

        def saturated(path):
            body, headers = read(path, (200, 503))
            assert isinstance(body, dict), "An extra artifact download started while responses were held"
            assert body["error"]["code"] == "artifact_download_capacity"
            assert headers["Retry-After"] == "2"
            assert headers["Cache-Control"] == "private, no-store"

        def exact(path, status=200):
            body, headers = read(path, status)
            if isinstance(body, dict):
                return False
            assert body == connector.audio
            assert headers["Content-Type"] == "audio/wav"
            assert headers["X-TokenCraft-Content-Sha256"] == sha256(body).hexdigest()
            return True

        first = submit()
        wait_for_status(port, first, auth, "succeeded", 18)
        base = f"/v1/requests/{first['id']}"
        warm = base + "/artifacts/0"
        assert exact(warm)
        with ExitStack() as readers:
            for _ in range(7):
                readers.enter_context(slow_download(port, warm, auth))
            with slow_download(port, warm, auth):
                saturated(warm)
                saturated(f"/v1/model-requests/{first['id']}/artifacts/0")
                call(port, warm, 401)
                second = submit()
                call(port, second["status_url"], 200, headers=auth)
            eventually(lambda: exact(warm, (200, 503)), "download after one reader disconnects", timeout=5)
        wait_for_status(port, second, auth, "succeeded", 18)
        call(port, warm, 404, headers=sibling)
        print("PASS warm responses held their download slots and left new requests available")

        db = urlsplit(settings["DATABASE_URL"])
        env = dict(PATH=settings["PATH"], PGHOST=db.hostname, PGPORT=str(db.port), PGUSER=db.username,
                   PGPASSWORD=db.password, PGDATABASE=db.path.lstrip("/"))
        sql = directory / "expire-warm-window.sql"
        sql.write_text("UPDATE gateway_content_history SET archive_at = now() - interval '1 second' "
                       "WHERE request_id = :'request';\n")
        command(postgres_bin() / "psql", "-XqAtw", "-v", "ON_ERROR_STOP=1", "-v", f"request={first['id']}",
                "-f", sql, cwd=directory, env=env)
        history = base + "/content"
        eventually(lambda: call(port, history, 200, headers=auth)[0]["phase"] == "archived",
                   "media archived on the worker", timeout=18)
        archived = history + "/artifacts/0"
        with slow_download(port, archived, auth):
            with slow_download(port, archived, auth):
                saturated(archived)
                call(port, archived, 404, headers=sibling)
                assert exact(f"/v1/requests/{second['id']}/artifacts/0")
            eventually(lambda: exact(archived, (200, 503)), "retained download after disconnect", timeout=5)
        assert connector.jobs == {first["id"]: 1, second["id"]: 1}
    print("PASS retained downloads had their own slots and preserved bytes and client scope")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-artifact-downloads-") as directory:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(directory), verification=verify)


if __name__ == "__main__":
    main()
