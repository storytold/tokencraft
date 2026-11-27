"""Rehearse a supported release upgrade and restore with a disposable PostgreSQL cluster."""

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
from collections import Counter
from contextlib import ExitStack, closing
from hashlib import sha256
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from secrets import token_hex
from socket import create_connection, socket
from tempfile import TemporaryDirectory
from threading import Event, Lock
from time import monotonic, sleep
from urllib.parse import quote

import smoke_workflow_contract as smoke
from smoke_cell_faults import serve

BASELINE_SCHEMA = 70
EXAMPLE_CONNECTOR_PORT = 4019


class ConnectorRelay(ThreadingHTTPServer):
    def __init__(self):
        super().__init__((smoke.HOST, 0), ConnectorRequest)
        self.hold_polling = Event()
        self.held_poll = Event()
        self.submissions = Counter()
        self.lock = Lock()

    def assert_submissions(self, *receipts):
        with self.lock:
            assert self.submissions == Counter(receipt["id"] for receipt in receipts)


class ConnectorRequest(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        assert self.path == "/v1/jobs"
        size = int(self.headers["Content-Length"])
        assert 0 < size <= 16 * 1024
        body = self.rfile.read(size)
        request_id = json.loads(body)["request_id"]
        assert self.headers["Idempotency-Key"] == request_id
        with self.server.lock:
            self.server.submissions[request_id] += 1
        self.forward(body)

    def do_GET(self):
        self.forward()

    def forward(self, body=None):
        headers = {name: self.headers[name] for name in ("Authorization", "Content-Type", "Idempotency-Key")
                   if name in self.headers}
        with closing(HTTPConnection(smoke.HOST, EXAMPLE_CONNECTOR_PORT, timeout=5)) as client:
            client.request(self.command, self.path, body, headers)
            response = client.getresponse()
            status, content_type, data = response.status, response.headers["Content-Type"], response.read()
        if (self.command == "GET" and status == 200 and self.server.hold_polling.is_set()
                and self.path.startswith("/v1/jobs/") and self.path.count("/") == 3):
            job = json.loads(data)
            assert job["status"] == "succeeded"
            job["status"] = "running"
            data = json.dumps(job).encode()
            self.server.held_poll.set()
        self.send_response(status)
        if content_type:
            self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def read_image(port, receipt, auth):
    path = receipt["status_url"]
    deadline = monotonic() + 20
    while monotonic() < deadline:
        state, _ = smoke.call(port, path, 200, headers=auth)
        if state["status"] == "succeeded":
            break
        assert state["status"] in {"queued", "running"}, state["status"]
        sleep(0.2)
    else:
        raise RuntimeError("Native request did not finish")
    result, _ = smoke.call(port, path + "/result", 200, headers=auth)
    image, = result["result"]["images"]
    data, headers = smoke.call(port, image["url"], 200, headers=auth)
    assert headers.get_content_type() == "image/png"
    assert sha256(data).hexdigest() == image["artifact"]["sha256"]
    return data


def verify_binding_rollout(gateway, folder, settings):
    catalog = Path(settings["TOKENCRAFT_CORE_CATALOG"])
    bindings = Path(settings["TOKENCRAFT_CONNECTORS"])
    binding, = json.loads(bindings.read_bytes())["bindings"]
    candidate_bindings = folder / "next-connectors.json"
    candidate_catalog = folder / "next-catalog.json"

    def check(catalog_file, binding_file, expected):
        result = subprocess.run([gateway, "catalog", "check-drain", "--catalog", catalog_file],
            cwd=folder, env=dict(settings, TOKENCRAFT_CONNECTORS=str(binding_file)),
            capture_output=True, text=True, timeout=12)
        assert result.returncode == int(bool(expected)), (result.stdout, result.stderr)
        report = json.loads(result.stdout)
        assert report["ready"] is (not expected) and report["checked_requests"] == 2, report
        blockers = {entry["phase"]: (entry["reason"], entry["requests"]) for entry in report["blockers"]}
        assert blockers == expected and len(report["blockers"]) == len(expected), report

    check(catalog, bindings, {})
    candidate_bindings.write_text(json.dumps({"version": 1, "bindings": [dict(binding, revision="next")]}))
    check(catalog, candidate_bindings, {
        "in_flight": ("binding_unavailable", 1), "queued": ("binding_unavailable", 1),
    })

    next_catalog = json.loads(catalog.read_bytes())
    next_catalog["offers"][0]["local_binding"] = "example_next"
    candidate_catalog.write_text(json.dumps(next_catalog))
    candidate_bindings.write_text(json.dumps({"version": 1, "bindings": [
        dict(binding, drain_only=True), dict(binding, name="example_next", revision="next"),
    ]}))
    check(candidate_catalog, candidate_bindings, {"queued": ("queued_offer_changed", 1)})
    print("PASS rollout preflight distinguished an accepted provider job from an undispatched request", flush=True)


def artifact_files(directory, receipt):
    return list(directory.glob(f"*/{receipt['id']}-*.tcm"))


def rehearse(args, folder, stack):
    pg = smoke.postgres_bin()
    root = smoke.ROOT
    expected = sorted(
        int(path.name.split("_", 1)[0])
        for path in (root / "crates/db/migrations").glob("*.sql")
        if not path.name.endswith(".down.sql")
    )
    latest = expected[-1]
    pending = ",".join(str(version) for version in expected if version > BASELINE_SCHEMA)
    env = {"PATH": os.environ["PATH"], "LANG": "C", "RUST_LOG": "warn"}
    with socket() as check:
        if check.connect_ex((smoke.HOST, EXAMPLE_CONNECTOR_PORT)) == 0:
            raise RuntimeError(f"Close the listener on port {EXAMPLE_CONNECTOR_PORT} before this rehearsal")

    def run(*args, settings=env):
        return smoke.command(*args, cwd=folder, env=settings)

    ca, key, certificate = (folder / name for name in ("ca.pem", "server.key", "server.pem"))
    run("openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        "-subj", "/CN=Disposable TokenCraft cutover CA", "-addext", "basicConstraints=critical,CA:TRUE",
        "-keyout", folder / "ca.key", "-out", ca)
    run("openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=localhost",
        "-keyout", key, "-out", folder / "server.csr")
    extensions = folder / "extensions"
    extensions.write_text("subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\n")
    run("openssl", "x509", "-req", "-in", folder / "server.csr", "-CA", ca, "-CAkey", folder / "ca.key",
        "-CAcreateserial", "-days", "1", "-extfile", extensions, "-out", certificate)
    password, runtime_password = token_hex(24), token_hex(24)
    password_file = folder / "password"
    password_file.write_text(password + "\n")
    cluster, pg_port = folder / "db", smoke.loopback_port()
    run(pg / "initdb", "-D", cluster, "--wal-segsize=1", "--auth=scram-sha-256", "--no-locale",
        "--username=tokencraft", "--pwfile", password_file)
    password_file.unlink()
    pg_env = dict(env, PGHOST="localhost", PGPORT=str(pg_port), PGUSER="tokencraft", PGPASSWORD=password,
                  PGSSLMODE="verify-full", PGSSLROOTCERT=str(ca))

    def postgres_service(data, port):
        def ready():
            return subprocess.run([pg / "pg_isready", "-q", "-t", "1", "-d", "postgres"],
                                  env=dict(pg_env, PGPORT=str(port)), check=False).returncode == 0

        return smoke.service(f"postgres-{data.name}", [
            pg / "postgres", "-D", data, "-h", smoke.HOST, "-p", str(port), "-c", "unix_socket_directories=",
            "-c", "ssl=on", "-c", f"ssl_cert_file={certificate}", "-c", f"ssl_key_file={key}",
        ], folder, env, ready, shutdown=signal.SIGINT)

    def cli(user, secret, database="postgres", environment="staging", *, port=pg_port):
        root_cert = quote(str(ca), safe="")
        url = f"postgresql://{user}:{secret}@localhost:{port}/{database}?sslmode=verify-full&sslrootcert={root_cert}"
        return dict(env, FOUNDATION_ENV=environment, DATABASE_URL=url)

    def psql(sql, *, database="postgres", port=pg_port):
        return run(pg / "psql", "-XqAtw", "-d", database, "-v", "ON_ERROR_STOP=1", "-c", sql,
                   settings=dict(pg_env, PGPORT=str(port)))

    stack.enter_context(postgres_service(cluster, pg_port))
    owner = cli("tokencraft", password)
    runtime = cli("tokencraft_runtime", runtime_password)

    def check_schema(settings, expected_state, *, database, pending=None):
        result = subprocess.run([args.candidate_admin, "schema-status"], cwd=folder, env=settings,
                                capture_output=True, text=True, timeout=15)
        assert result.returncode == (0 if expected_state == "current" else 1), result.stderr
        lines = dict(line.split(": ", 1) for line in result.stdout.strip().splitlines())
        assert lines.get("schema status") == expected_state and lines["binary schema"] == str(latest), lines
        assert lines.get("database schema") == database and lines.get("pending migrations") == pending, lines

    def grant():
        run(pg / "psql", "-Xqw", "-d", "postgres", "-v", "runtime_role=tokencraft_runtime",
            "-f", root / "deploy/postgres/grant-runtime.sql", settings=pg_env)

    def ledger():
        return psql("SELECT string_agg(version || ':' || encode(checksum, 'hex') || ':' || success, ',' ORDER BY version) "
                    "FROM public._sqlx_migrations")

    run(args.baseline_admin, "migrate", settings=owner)
    before = ledger()
    assert psql("SELECT max(version) FROM public._sqlx_migrations") == str(BASELINE_SCHEMA)
    run(pg / "psql", "-Xqw", "-d", "postgres", "-f", smoke.FIXTURES/"create-runtime-role.sql",
        settings=dict(pg_env, TOKENCRAFT_RUNTIME_APP_PASSWORD=runtime_password))
    grant()
    psql("ALTER ROLE tokencraft_runtime SET default_transaction_read_only = on")
    check_schema(runtime, "upgrade_required", database=str(BASELINE_SCHEMA), pending=pending)
    assert ledger() == before
    psql("ALTER ROLE tokencraft_runtime RESET default_transaction_read_only")
    print(f"PASS schema {BASELINE_SCHEMA} reported pending versions through {latest} using the read-only runtime role over TLS", flush=True)

    receiver = stack.enter_context(smoke.webhook_receiver())
    relay = stack.enter_context(serve(ConnectorRelay()))
    fixture = folder / "webhooks.json"
    fixture.write_text(json.dumps({"destinations": {"fixture": f"http://{smoke.HOST}:{receiver.server_port}/events"}}))
    webhook_key = token_hex(32)
    connector_key = token_hex(32)

    def connector_ready():
        with create_connection((smoke.HOST, EXAMPLE_CONNECTOR_PORT), timeout=1):
            return True

    connector_env = dict(env, TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE=connector_key)
    connector_auth = {"Authorization": "Bearer " + connector_key}

    def connector_result(handle):
        saved, response = smoke.call(
            EXAMPLE_CONNECTOR_PORT, f"/v1/jobs/{handle}/artifacts/0", 200, headers=connector_auth,
        )
        assert response.get_content_type() == "image/png"
        return saved

    def start_connector(binary, state, processes):
        processes.enter_context(smoke.service(
            "connector-" + state.name, [binary, "example-connector", state],
            folder, connector_env, connector_ready,
        ))

    jobs = folder / "jobs"
    artifacts, saved_artifacts = folder / "artifacts", folder / "before-upgrade-artifacts"
    artifacts.mkdir(mode=0o700)
    bindings = json.loads((root / "examples/connector/bindings.json").read_bytes())
    bindings["bindings"][0]["origin"] = f"http://{smoke.HOST}:{relay.server_port}/"
    binding_path = folder / "example-connectors.json"
    binding_path.write_text(json.dumps(bindings))
    common = dict(COOKIE_DOMAIN="localhost", COOKIE_SECURE="false", SESSION_SECRET=token_hex(32),
                  TOKENCRAFT_CORE_CATALOG=str(root / "examples/connector/catalog.json"),
                  TOKENCRAFT_CONNECTORS=str(binding_path),
                  TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE=connector_key,
                  TOKENCRAFT_DEMO_PAYLOAD_KEY=token_hex(32), TOKENCRAFT_WORKFLOW_KEY=token_hex(32))

    def gateway_config(role, port, database, delivery):
        settings = dict(cli("tokencraft_runtime", runtime_password, database, "development"), **common,
                        TOKENCRAFT_PROCESS_ROLE=role, API_GATEWAY_PORT=str(port), APP_BASE_URL=f"http://localhost:{port}")
        settings["TOKENCRAFT_DEMO_ARTIFACT_DIR"] = str(artifacts if database == "postgres" else saved_artifacts)
        if delivery:
            settings.update(TOKENCRAFT_DEMO_WEBHOOK_CONFIG=str(fixture), TOKENCRAFT_DEMO_WEBHOOK_KEY=webhook_key)
        return settings

    def gateway(binary, role, processes, *, database="postgres", delivery=True):
        port = smoke.loopback_port()
        settings = gateway_config(role, port, database, delivery)
        processes.enter_context(smoke.service(f"{role}-{port}", [binary], folder, settings, lambda: smoke.ready(port)))
        return port

    handle_query = (
        "SELECT p.upstream_handle FROM public.gateway_provider_runs p "
        "JOIN public.gateway_requests r ON r.id = p.request_id WHERE p.accepted_at IS NOT NULL AND r.app_ref = "
    )
    with ExitStack() as upstream:
        start_connector(args.baseline_connector, jobs, upstream)
        api = gateway(args.baseline_gateway, "api", upstream)
        session, account, project = smoke.provision(api)
        base = f"/v1/projects/{project}"
        query = f"?account_id={account}"
        subscription, _ = smoke.call(api, base + "/webhooks" + query, 201, method="POST", headers=session,
                                     body={"name": "Legacy notification", "destination_id": "fixture"})
        principal, _ = smoke.call(
            api, base + "/service-principals" + query, 201, method="POST", headers=session,
            body={"name": "Legacy application", "scopes": ["requests:submit", "requests:read", "usage:read"]},
        )
        auth = {"Authorization": "Bearer " + principal["token"]}

        def prepare_submission(port, app_ref, *, notify=False):
            quote, _ = smoke.call(
                port, "/v1/quotes/scoped", 200, method="POST", headers=auth,
                body={"model": "example_image", "workload": {"kind": "image", "count": 1, "aspect_ratio": "4:3"}},
            )
            body = {
                "quote_id": quote["id"], "input": quote["input"], "app_ref": app_ref,
                "image_prompt": "Synthetic release migration",
            }
            if notify:
                body["delivery"] = {"webhook_subscription_id": subscription["id"]}
            headers = {**auth, "Idempotency-Key": token_hex(16)}
            return headers, body

        def submit(port, app_ref, *, notify=False):
            headers, body = prepare_submission(port, app_ref, notify=notify)
            receipt, _ = smoke.call(port, "/v1/requests", 202, method="POST", headers=headers, body=body)
            return receipt, headers, body

        with ExitStack() as run_old:
            gateway(args.baseline_gateway, "worker", run_old, delivery=False)
            completed, completed_headers, completed_body = submit(api, "before-upgrade", notify=True)
            contents = read_image(api, completed, auth)
            relay.hold_polling.set()
            running, running_headers, running_body = submit(api, "accepted-before-upgrade")
            assert relay.held_poll.wait(10), "The prior worker never polled the accepted provider job"
            assert smoke.call(api, running["status_url"], 200, headers=auth)[0]["status"] == "running"
            running_handle = psql(handle_query + "'accepted-before-upgrade'")
            assert running_handle
            running_contents = connector_result(running_handle)
        relay.hold_polling.clear()
        queued, queued_headers, queued_body = submit(api, "pending-upgrade")
        assert smoke.call(api, queued["status_url"], 200, headers=auth)[0]["status"] == "queued"
        payload = (
            "SELECT encode(convert_to(payload, 'UTF8'), 'hex') "
            "FROM public.gateway_webhook_deliveries WHERE request_id IS NOT NULL"
        )
        legacy_hex = psql(payload)
        handle = psql(handle_query + "'before-upgrade'")
        assert legacy_hex and handle and receiver.received.empty()

    relay.assert_submissions(completed, running)
    original_artifact, = artifact_files(artifacts, completed)
    assert not artifact_files(artifacts, running)
    assert psql("SELECT count(*) FROM public.gateway_artifacts WHERE object_key IS NOT NULL AND payload IS NULL") == "1"

    psql("ALTER ROLE tokencraft_runtime SET default_transaction_read_only = on")
    verify_binding_rollout(args.candidate_gateway, folder, dict(runtime, **common))
    psql("ALTER ROLE tokencraft_runtime RESET default_transaction_read_only")

    dump = folder / "before-upgrade.dump"
    saved_jobs = folder / "before-upgrade-jobs"
    shutil.copytree(jobs, saved_jobs)
    shutil.copytree(artifacts, saved_artifacts)
    saved_artifact, = artifact_files(saved_artifacts, completed)
    assert saved_artifact.read_bytes() == original_artifact.read_bytes()
    run(pg / "pg_dump", "--format=custom", "--file", dump, "postgres", settings=pg_env)
    migration = run(args.candidate_admin, "migrate", "--pause-admissions", settings=owner)
    assert migration.endswith("\nadmissions: paused"), migration
    grant()
    check_schema(runtime, "current", database=str(latest))
    assert run(args.candidate_admin, "admissions", "status", "--expect", "paused",
               settings=runtime) == "admissions: paused"
    mismatch = subprocess.run([args.candidate_admin, "admissions", "status", "--expect", "open"],
                              cwd=folder, env=runtime, capture_output=True, text=True, timeout=15)
    assert mismatch.returncode == 1 and mismatch.stdout.strip() == "admissions: paused"
    assert "expected admissions to be open, observed paused" in mismatch.stderr
    print("PASS the runtime role verified paused admissions and the opposite expectation failed closed", flush=True)

    psql("CREATE ROLE tokencraft_cutover_monitor LOGIN")
    run(pg / "psql", "-Xqw", "-d", "postgres", "-v", "monitor_role=tokencraft_cutover_monitor",
        "-f", root / "deploy/monitoring/install.sql", settings=pg_env)
    monitor_query = """BEGIN READ ONLY; SET LOCAL ROLE tokencraft_cutover_monitor;
        SELECT tokencraft_monitor.sample('["mock"]', '[]', '{"local":[],"connector":[]}', 0, NULL, NULL); COMMIT;"""
    assert json.loads(psql(monitor_query))["maintenance"]["paused"] is True
    standby, standby_port = folder / "standby", smoke.loopback_port()
    run(pg / "pg_basebackup", "-w", "-D", standby, "-X", "stream", "-R", "-c", "fast", settings=pg_env)
    with postgres_service(standby, standby_port):
        assert psql("SELECT pg_is_in_recovery() AND paused FROM public.gateway_admission_control WHERE singleton",
                    port=standby_port) == "t"
        standby_runtime = cli("tokencraft_runtime", runtime_password, port=standby_port)
        assertion = subprocess.run([args.candidate_admin, "admissions", "status", "--expect", "paused"],
                                   cwd=folder, env=standby_runtime, capture_output=True, text=True, timeout=15)
        assert assertion.returncode == 1 and not assertion.stdout
        assert "admission status requires the cell's primary database" in assertion.stderr
        assert json.loads(psql(monitor_query, port=standby_port))["maintenance"] is None
    print("PASS a PostgreSQL standby cannot confirm the pause through the CLI or Operations", flush=True)

    versions = psql(
        "SELECT string_agg(version::text, ',' ORDER BY version) FROM public._sqlx_migrations WHERE success"
    )
    assert versions == ",".join(map(str, expected))
    assert psql(payload) == legacy_hex
    old_settings = gateway_config("api", smoke.loopback_port(), "postgres", True)
    blocked = subprocess.run([args.baseline_gateway], cwd=folder, env=old_settings, capture_output=True, text=True, timeout=15)
    assert blocked.returncode == 1 and "apply the matching TokenCraft migrations" in blocked.stderr
    print(f"PASS explicit migration and grants reached schema {latest} and preserved the queued native delivery", flush=True)

    def replay(port, receipt, headers, body):
        returned, _ = smoke.call(port, "/v1/requests", 200, method="POST", headers=headers, body=body)
        assert returned == receipt

    def verify_connector_state():
        assert connector_result(handle) == contents
        assert connector_result(running_handle) == running_contents

    def verify_legacy_delivery(port):
        delivery = smoke.verify_signed_delivery(
            port, base + "/webhook-deliveries", query, session, receiver, subscription, "request.succeeded",
            {"request_id": completed["id"], "project_id": project, "status": "succeeded",
             "fetch_url": completed["status_url"]},
            expected_payload=bytes.fromhex(legacy_hex),
        )
        assert delivery["request_id"] == completed["id"]

    def verify_usage(port, *references):
        for reference in references:
            usage, _ = smoke.call(port, "/v1/usage?app_ref=" + reference, 200, headers=auth)
            assert usage["totals"]["requests"] == usage["totals"]["succeeded"] == 1

    with ExitStack() as upgraded:
        start_connector(args.candidate_connector, jobs, upgraded)
        verify_connector_state()
        api = gateway(args.candidate_gateway, "api", upgraded)
        gateway(args.candidate_gateway, "worker", upgraded)
        upgraded_headers, upgraded_body = prepare_submission(api, "after-upgrade")
        rejection, retry = smoke.call(api, "/v1/requests", 503, method="POST",
                                      headers=upgraded_headers, body=upgraded_body)
        assert rejection["error"]["code"] == "admissions_paused" and retry.get("Retry-After") == "5"
        assert psql("SELECT count(*) FROM public.gateway_requests WHERE app_ref = 'after-upgrade'") == "0"
        replay(api, completed, completed_headers, completed_body)
        replay(api, running, running_headers, running_body)
        replay(api, queued, queued_headers, queued_body)
        assert read_image(api, completed, auth) == contents
        assert read_image(api, running, auth) == running_contents
        assert psql(handle_query + "'accepted-before-upgrade'") == running_handle
        queued_contents = read_image(api, queued, auth)
        verify_usage(api, "before-upgrade", "accepted-before-upgrade", "pending-upgrade")
        reader, _ = smoke.call(api, base + "/service-principals" + query, 201, method="POST", headers=session,
                               body={"name": "Read-only release smoke", "scopes": ["requests:read", "usage:read"]})
        print(run(
            sys.executable, "-B", root / "scripts/smoke_authenticated_gateway.py",
            "--api", f"http://{smoke.HOST}:{api}", "--request-id", completed["id"],
            settings=dict(env, TOKENCRAFT_SMOKE_API_KEY=reader["token"]),
        ), flush=True)
        verify_legacy_delivery(api)
        assert psql(payload) == legacy_hex
        assert run(args.candidate_admin, "admissions", "resume", settings=owner) == "admissions: open"
        assert run(args.candidate_admin, "admissions", "status", "--expect", "open",
                   settings=runtime) == "admissions: open"
        upgraded_only, _ = smoke.call(api, "/v1/requests", 202, method="POST",
                                      headers=upgraded_headers, body=upgraded_body)
        read_image(api, upgraded_only, auth)
        upgraded_handle = psql(handle_query + "'after-upgrade'")
        assert upgraded_handle and upgraded_handle != handle
        replay(api, upgraded_only, upgraded_headers, upgraded_body)
        verify_usage(api, "after-upgrade")
    print("PASS the migrated cell rejected fresh admission before resume and then accepted the same intent and key",
          flush=True)
    relay.assert_submissions(completed, running, queued, upgraded_only)
    upgraded_artifact, = artifact_files(artifacts, upgraded_only)
    assert not artifact_files(saved_artifacts, upgraded_only)
    print("PASS the upgraded fleet resumed the accepted job without resubmission, completed queued and new work, "
          "and delivered the original signed event", flush=True)

    run(pg / "createdb", "rollback_rehearsal", settings=pg_env)
    run(pg / "pg_restore", "--exit-on-error", "--single-transaction", "-d", "rollback_rehearsal", dump, settings=pg_env)
    rollback_runtime = cli("tokencraft_runtime", runtime_password, "rollback_rehearsal")
    check_schema(rollback_runtime, "upgrade_required", database=str(BASELINE_SCHEMA), pending=pending)
    assert receiver.received.empty()
    receiver.seen.clear()

    with ExitStack() as restored:
        start_connector(args.baseline_connector, saved_jobs, restored)
        verify_connector_state()
        smoke.call(EXAMPLE_CONNECTOR_PORT, f"/v1/jobs/{upgraded_handle}", 404, headers=connector_auth)
        api = gateway(args.baseline_gateway, "api", restored, database="rollback_rehearsal")
        replay(api, completed, completed_headers, completed_body)
        replay(api, running, running_headers, running_body)
        replay(api, queued, queued_headers, queued_body)
        assert read_image(api, completed, auth) == contents
        assert smoke.call(api, running["status_url"], 200, headers=auth)[0]["status"] == "running"
        assert smoke.call(api, queued["status_url"], 200, headers=auth)[0]["status"] == "queued"
        smoke.call(api, upgraded_only["status_url"], 404, headers=auth)

        gateway(args.baseline_gateway, "worker", restored, database="rollback_rehearsal")
        assert read_image(api, running, auth) == running_contents
        assert psql(handle_query + "'accepted-before-upgrade'", database="rollback_rehearsal") == running_handle
        assert read_image(api, queued, auth) == queued_contents
        restored_only, restored_headers, restored_body = submit(api, "after-rollback")
        read_image(api, restored_only, auth)
        replay(api, restored_only, restored_headers, restored_body)
        verify_usage(api, "before-upgrade", "accepted-before-upgrade", "pending-upgrade", "after-rollback")
        verify_legacy_delivery(api)
    # The undispatched request is submitted once in each independent job store.
    relay.assert_submissions(completed, running, queued, upgraded_only, queued, restored_only)
    restored_artifact, = artifact_files(saved_artifacts, restored_only)
    assert restored_artifact.is_file() and upgraded_artifact.is_file()
    assert not artifact_files(artifacts, restored_only)
    assert psql("SELECT count(*) FROM public.gateway_requests "
                "WHERE app_ref = 'after-upgrade' AND succeeded_at IS NOT NULL") == "1"
    assert psql("SELECT count(*) FROM public.gateway_requests WHERE app_ref = 'after-rollback'") == "0"
    print("PASS the restored prior fleet replayed receipts, read the saved artifact, resumed the accepted job without "
          "resubmission, and delivered the original signed event", flush=True)
    print("PASS the independent database, connector, and filesystem snapshots excluded the post-cutover request", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-admin", type=Path, required=True)
    parser.add_argument("--baseline-gateway", type=Path, required=True)
    parser.add_argument("--candidate-admin", type=Path, required=True)
    parser.add_argument("--candidate-gateway", type=Path, required=True)
    parser.add_argument("--baseline-connector", type=Path, required=True)
    parser.add_argument("--candidate-connector", "--connector", type=Path, required=True)
    args = argparse.Namespace(**{key: value.resolve(strict=True) for key, value in vars(parser.parse_args()).items()})
    os.umask(0o077)
    with TemporaryDirectory(prefix="tokencraft-release-cutover-") as temp, ExitStack() as stack:
        rehearse(args, Path(temp), stack)


if __name__ == "__main__":
    main()
