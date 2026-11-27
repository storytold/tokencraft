"""Rehearse a supported release upgrade and restore with a disposable PostgreSQL cluster."""

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
from contextlib import ExitStack
from hashlib import sha256
from pathlib import Path
from secrets import token_hex
from socket import create_connection, socket
from tempfile import TemporaryDirectory
from time import monotonic, sleep
from urllib.parse import quote

import smoke_workflow_contract as smoke

BASELINE_SCHEMA = 70
EXAMPLE_CONNECTOR_PORT = 4019


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

    def ready_postgres():
        return subprocess.run([pg / "pg_isready", "-q", "-t", "1", "-d", "postgres"], env=pg_env,
                              check=False).returncode == 0

    def cli(user, secret, database="postgres", environment="staging"):
        root_cert = quote(str(ca), safe="")
        url = f"postgresql://{user}:{secret}@localhost:{pg_port}/{database}?sslmode=verify-full&sslrootcert={root_cert}"
        return dict(env, FOUNDATION_ENV=environment, DATABASE_URL=url)

    def psql(sql, settings=pg_env):
        return run(pg / "psql", "-XqAtw", "-d", "postgres", "-v", "ON_ERROR_STOP=1", "-c", sql, settings=settings)

    stack.enter_context(smoke.service("postgres", [
        pg / "postgres", "-D", cluster, "-h", smoke.HOST, "-p", str(pg_port), "-c", "unix_socket_directories=",
        "-c", "ssl=on", "-c", f"ssl_cert_file={certificate}", "-c", f"ssl_key_file={key}",
    ], folder, env, ready_postgres, shutdown=signal.SIGINT))
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
    fixture = folder / "webhooks.json"
    fixture.write_text(json.dumps({"destinations": {"fixture": f"http://{smoke.HOST}:{receiver.server_port}/events"}}))
    webhook_key = token_hex(32)
    connector_key = token_hex(32)

    def connector_ready():
        with create_connection((smoke.HOST, EXAMPLE_CONNECTOR_PORT), timeout=1):
            return True

    connector_env = dict(env, TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE=connector_key)

    def start_connector(binary, state, processes):
        processes.enter_context(smoke.service(
            "connector-" + state.name, [binary, "example-connector", state],
            folder, connector_env, connector_ready,
        ))

    common = dict(COOKIE_DOMAIN="localhost", COOKIE_SECURE="false", SESSION_SECRET=token_hex(32),
                  TOKENCRAFT_CORE_CATALOG=str(root / "examples/connector/catalog.json"),
                  TOKENCRAFT_CONNECTORS=str(root / "examples/connector/bindings.json"),
                  TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE=connector_key,
                  TOKENCRAFT_DEMO_PAYLOAD_KEY=token_hex(32), TOKENCRAFT_WORKFLOW_KEY=token_hex(32))

    def gateway_config(role, port, database, delivery):
        settings = dict(cli("tokencraft_runtime", runtime_password, database, "development"), **common,
                        TOKENCRAFT_PROCESS_ROLE=role, API_GATEWAY_PORT=str(port), APP_BASE_URL=f"http://localhost:{port}")
        if delivery:
            settings.update(TOKENCRAFT_DEMO_WEBHOOK_CONFIG=str(fixture), TOKENCRAFT_DEMO_WEBHOOK_KEY=webhook_key)
        return settings

    def gateway(binary, role, processes, *, database="postgres", delivery=True):
        port = smoke.loopback_port()
        settings = gateway_config(role, port, database, delivery)
        processes.enter_context(smoke.service(f"{role}-{port}", [binary], folder, settings, lambda: smoke.ready(port)))
        return port

    jobs = folder / "jobs"
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

        def submit(port, app_ref, *, notify=False):
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
            receipt, _ = smoke.call(port, "/v1/requests", 202, method="POST", headers=headers, body=body)
            return receipt, headers, body

        with ExitStack() as run_old:
            gateway(args.baseline_gateway, "worker", run_old, delivery=False)
            completed, completed_headers, completed_body = submit(api, "before-upgrade", notify=True)
            contents = read_image(api, completed, auth)
        queued, queued_headers, queued_body = submit(api, "pending-upgrade")
        assert smoke.call(api, queued["status_url"], 200, headers=auth)[0]["status"] == "queued"
        payload = (
            "SELECT encode(convert_to(payload, 'UTF8'), 'hex') "
            "FROM public.gateway_webhook_deliveries WHERE request_id IS NOT NULL"
        )
        legacy_hex = psql(payload)
        handle_query = (
            "SELECT p.upstream_handle FROM public.gateway_provider_runs p "
            "JOIN public.gateway_requests r ON r.id = p.request_id WHERE r.app_ref = "
        )
        handle = psql(handle_query + "'before-upgrade'")
        assert legacy_hex and handle and receiver.received.empty()

    dump = folder / "before-upgrade.dump"
    saved_jobs = folder / "before-upgrade-jobs"
    shutil.copytree(jobs, saved_jobs)
    run(pg / "pg_dump", "--format=custom", "--file", dump, "postgres", settings=pg_env)
    run(args.candidate_admin, "migrate", settings=owner)
    grant()
    check_schema(runtime, "current", database=str(latest))
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

    connector_auth = {"Authorization": "Bearer " + connector_key}

    def verify_connector_state():
        saved, response = smoke.call(
            EXAMPLE_CONNECTOR_PORT, f"/v1/jobs/{handle}/artifacts/0", 200, headers=connector_auth,
        )
        assert response.get_content_type() == "image/png"
        assert saved == contents

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
        replay(api, completed, completed_headers, completed_body)
        replay(api, queued, queued_headers, queued_body)
        assert read_image(api, completed, auth) == contents
        queued_contents = read_image(api, queued, auth)
        upgraded_only, upgrade_headers, upgrade_body = submit(api, "after-upgrade")
        read_image(api, upgraded_only, auth)
        upgraded_handle = psql(handle_query + "'after-upgrade'")
        assert upgraded_handle and upgraded_handle != handle
        replay(api, upgraded_only, upgrade_headers, upgrade_body)
        verify_usage(api, "before-upgrade", "pending-upgrade", "after-upgrade")
        reader, _ = smoke.call(api, base + "/service-principals" + query, 201, method="POST", headers=session,
                               body={"name": "Read-only release smoke", "scopes": ["requests:read", "usage:read"]})
        print(run(
            sys.executable, "-B", root / "scripts/smoke_authenticated_gateway.py",
            "--api", f"http://{smoke.HOST}:{api}", "--request-id", completed["id"],
            settings=dict(env, TOKENCRAFT_SMOKE_API_KEY=reader["token"]),
        ), flush=True)
        verify_legacy_delivery(api)
        assert psql(payload) == legacy_hex
    print("PASS the upgraded fleet loaded legacy connector state, finished queued and new work, and sent the legacy event", flush=True)

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
        replay(api, queued, queued_headers, queued_body)
        assert read_image(api, completed, auth) == contents
        assert smoke.call(api, queued["status_url"], 200, headers=auth)[0]["status"] == "queued"
        smoke.call(api, upgraded_only["status_url"], 404, headers=auth)

        gateway(args.baseline_gateway, "worker", restored, database="rollback_rehearsal")
        assert read_image(api, queued, auth) == queued_contents
        restored_only, restored_headers, restored_body = submit(api, "after-rollback")
        read_image(api, restored_only, auth)
        replay(api, restored_only, restored_headers, restored_body)
        verify_usage(api, "before-upgrade", "pending-upgrade", "after-rollback")
        verify_legacy_delivery(api)
    assert psql("SELECT count(*) FROM public.gateway_requests "
                "WHERE app_ref = 'after-upgrade' AND succeeded_at IS NOT NULL") == "1"
    assert psql("SELECT count(*) FROM public.gateway_requests WHERE app_ref = 'after-rollback'") == "0"
    print("PASS the restored prior fleet replayed receipts, completed queued and new work, and delivered the original signed event", flush=True)
    print("PASS the rollback snapshot excluded the post-cutover request and connector job while the upgraded database retained it", flush=True)


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
