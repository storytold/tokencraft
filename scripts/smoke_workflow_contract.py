"""Exercise a published workflow contract with disposable gateway and delivery worker processes."""

import argparse
from contextlib import ExitStack, contextmanager
from hashlib import sha256
from http.client import HTTPConnection
import json
import os
from pathlib import Path
from secrets import token_hex
import signal
import socket
import subprocess
import sys
from tempfile import TemporaryDirectory
from time import monotonic, sleep


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / ".github/fixtures/container"
MEDIA = ROOT / "plugins/tools/media"
HOST = "127.0.0.1"


def loopback_port():
    with socket.socket() as listener:
        listener.bind((HOST, 0))
        return listener.getsockname()[1]


def command(*args, cwd, env):
    result = subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise RuntimeError(f"{Path(args[0]).name} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def call(port, path, expected, *, method="GET", body=None, headers=None):
    fields = {"Origin": f"http://localhost:{port}", **(headers or {})}
    if body is not None and not isinstance(body, bytes):
        fields["Content-Type"] = "application/json"
        body = json.dumps(body).encode()
    connection = HTTPConnection(HOST, port, timeout=5)
    try:
        connection.request(method, path, body, fields)
        result = connection.getresponse()
        if result.status != expected:
            raise RuntimeError(f"{method} {path.split('?')[0]} returned {result.status}, expected {expected}")
        data = result.read(2 * 1024 * 1024 + 1)
        if len(data) > 2 * 1024 * 1024:
            raise RuntimeError("The fixture response exceeded the expected size")
        if result.headers.get_content_type() == "application/json":
            data = json.loads(data)
        return data, result.headers
    finally:
        connection.close()


def ready(port):
    return call(port, "/ready", 200)[0]["ready"] is True


@contextmanager
def service(name, args, directory, env, probe, *, shutdown=signal.SIGTERM):
    with (directory / f"{name}.log").open("w+") as log:
        child = subprocess.Popen(args, cwd=directory, env=env, stdin=subprocess.DEVNULL,
                                 stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = monotonic() + 30
            while child.poll() is None and monotonic() < deadline:
                try:
                    if probe():
                        break
                except (OSError, RuntimeError):
                    pass
                sleep(0.1)
            else:
                raise RuntimeError(f"{name} failed to become ready")
            yield
        except Exception:
            log.seek(0)
            print(log.read()[-6000:], file=sys.stderr)
            raise
        finally:
            if child.poll() is None:
                child.send_signal(shutdown)
                try:
                    child.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()


def provision(port):
    _, headers = call(port, "/v1/auth/signup", 201, method="POST", body={
        "email": f"workflow-smoke-{token_hex(8)}@example.test",
        "password": token_hex(24),
        "display_name": "Workflow contract fixture",
    })
    session = {"Cookie": headers["Set-Cookie"].split(";", 1)[0]}
    accounts, _ = call(port, "/v1/accounts", 200, headers=session)
    account = accounts["accounts"][0]["account_id"]
    project, _ = call(port, f"/v1/projects?account_id={account}", 201, method="POST",
                      body={"name": "Workflow contract check"}, headers=session)
    return session, account, project["id"]


def verify_output(port, receipt, headers, digest, size):
    deadline = monotonic() + 20
    while monotonic() < deadline:
        state, result_headers = call(port, receipt["status_url"], 200, headers=headers)
        if state["status"] == "succeeded":
            break
        if state["status"] in ("failed", "canceled", "expired"):
            raise RuntimeError(f"Workflow reached {state['status']}")
        sleep(0.2)
    else:
        raise RuntimeError("Workflow did not complete before the deadline")
    output = state["outputs"]["fingerprint"]
    path, separator, query = receipt["status_url"].partition("?")
    assert output == {
        "type": "media", "kind": "metadata", "download_url": f"{path}/outputs/fingerprint{separator}{query}",
    }
    record, response = call(port, output["download_url"], 200, headers=headers)
    assert response.get_content_type() == "application/json"
    assert record == {"content_type": "image/png", "size_bytes": size, "sha256": digest}
    return state, result_headers


def verify_json_response(spec, operation, code, payload, headers, expected_headers=()):
    response = operation["responses"][str(code)]
    if "$ref" in response:
        response = spec["components"]["responses"][response["$ref"].rsplit("/", 1)[1]]
    schema = response["content"]["application/json"]["schema"]
    if "$ref" in schema:
        schema = spec["components"]["schemas"][schema["$ref"].rsplit("/", 1)[1]]
    assert set(schema["required"]) <= payload.keys()
    for field in expected_headers:
        assert field in response["headers"] and headers[field]


def verify_workflow(port, session, account, project):
    project_path = f"/v1/projects/{project}"
    query = f"?account_id={account}"
    tools, _ = call(port, project_path + "/workflow-tools" + query + "&verify=fixture-delivery", 200, headers=session)
    (binding,) = tools
    assert binding["reachable"] and binding["manifest"]["plugin"] == "tokencraft-delivery"
    draft, _ = call(port, project_path + "/workflows" + query, 201, method="POST", headers=session, body={
        "schema": 1, "title": "Workflow contract fixture", "max_usd_nanos": 0,
        "inputs": {"image": {"title": "Source image", "kind": "image"}},
        "outputs": {"fingerprint": "fingerprint"},
        "nodes": [{
            "id": "fingerprint", "title": "Fingerprint image", "kind": "tool",
            "model": "delivery.fingerprint", "max_usd_nanos": 0,
            "settings": {"source": {"source": "request", "field": "image"}},
            "tool": {"binding_revision": binding["revision"], "revision": 1, "inputs": {"source": "media"},
                     "output": "metadata", "parameters": {}},
        }],
    })
    manage = project_path + f"/workflows/{draft['id']}"
    endpoint = project_path + f"/workflow-endpoints/{draft['id']}"
    png = (ROOT / "frontend/public/brand/tokencraft-mark.png").read_bytes()
    digest = sha256(png).hexdigest()
    file_headers = {"Content-Type": "image/png", "X-TokenCraft-Content-Sha256": digest}
    uploaded, _ = call(port, endpoint + "/uploads" + query, 201, method="POST",
                       body=png, headers={**session, **file_headers})
    checked, _ = call(port, manage + "/test-runs" + query, 202, method="POST",
                      headers={**session, "Idempotency-Key": token_hex(16)}, body={
                          "revision": draft["revision"], "inputs": {"image": {"upload_id": uploaded["upload_id"]}},
                      })
    verify_output(port, checked, session, digest, len(png))
    released, _ = call(port, manage + "/versions" + query, 200, method="POST", headers=session,
                       body={"revision": draft["revision"], "active_version": None})
    assert released["active_version"] == 1
    app, _ = call(port, manage + "/apps" + query, 201, method="POST", headers=session, body={
        "name": "Contract fixture app", "days": 1,
        "scopes": ["runs:submit", "runs:read", "outputs:read", "runs:cancel"],
    })
    auth = {"Authorization": "Bearer " + app["secret"]}
    spec, _ = call(port, endpoint + "/openapi.json", 200, headers=auth)
    paths = spec["paths"]
    uploads = paths[endpoint + "/uploads"]["post"]
    assert spec["openapi"] == "3.1.0" and spec["info"]["version"] == "1"
    assert list(spec["x-workflow-upload-inputs"].items()) == [("image", "image")]
    assert set(uploads["requestBody"]["content"]) == {"image/png", "image/jpeg", "image/webp", "image/gif"}
    assert uploads["x-workflow-required-scope"] == "runs:submit"
    paths_run = endpoint + "/runs/{run_id}"
    assert paths[paths_run + "/cancel"]["post"]["x-workflow-required-scope"] == "runs:cancel"
    assert paths[paths_run + "/outputs/{name}"]["get"]["x-workflow-required-scope"] == "outputs:read"

    uploaded, upload_headers = call(port, endpoint + "/uploads", 201, method="POST", body=png, headers={**auth, **file_headers})
    verify_json_response(spec, uploads, 201, uploaded, upload_headers)
    body = {"inputs": {"image": {"upload_id": uploaded["upload_id"]}}}
    invoke = {**auth, "Idempotency-Key": token_hex(16)}
    receipt, accepted = call(port, endpoint + "/runs", 202, method="POST", body=body, headers=invoke)
    replay, repeated = call(port, endpoint + "/runs", 200, method="POST", body=body, headers=invoke)
    for code, payload, headers in ((202, receipt, accepted), (200, replay, repeated)):
        verify_json_response(spec, paths[endpoint + "/runs"]["post"], code, payload, headers,
                             expected_headers=("Location", "Retry-After"))
        assert payload["status_url"] == headers["Location"] == f"{endpoint}/runs/{payload['id']}"
        assert int(headers["Retry-After"]) > 0
    assert replay == {**receipt, "replayed": True} and receipt["replayed"] is False
    state, status_headers = verify_output(port, receipt, auth, digest, len(png))
    status_operation = paths[paths_run]["get"]
    verify_json_response(spec, status_operation, 200, state, status_headers, expected_headers=("ETag",))
    assert "304" in status_operation["responses"] and "ETag" in status_operation["responses"]["304"]["headers"]
    tag = status_headers["ETag"]
    empty, cached = call(port, receipt["status_url"], 304, headers={**auth, "If-None-Match": tag})
    assert empty == b"" and cached["ETag"] == tag
    error, rejected = call(port, receipt["status_url"] + "/cancel", 409, method="POST", headers=auth)
    verify_json_response(spec, paths[paths_run + "/cancel"]["post"], 409, error, rejected)
    assert error["error"]["code"] == "conflict"

    reader, _ = call(port, manage + "/apps" + query, 201, method="POST", headers=session, body={
        "name": "Separate read-only app", "days": 1, "scopes": ["runs:read"],
    })
    read_only = {"Authorization": "Bearer " + reader["secret"]}
    call(port, endpoint + "/openapi.json", 403, headers=read_only)
    call(port, receipt["status_url"], 404, headers=read_only)
    print("PASS published workflow contract, upload, digest, replay, conditional polling, cancellation, and app isolation")


def rehearse(gateway, admin, directory):
    env = {"PATH": os.environ["PATH"], "LANG": "C", "RUST_LOG": "warn"}
    pg = Path(command("pg_config", "--bindir", cwd=directory, env=env))
    data, password_file = directory / "pg", directory / "password"
    password, runtime_password = token_hex(24), token_hex(24)
    password_file.write_text(password + "\n")
    command(pg / "initdb", "-D", data, "--no-locale", "--encoding=UTF8", "--wal-segsize=1",
            "--auth=scram-sha-256", "--username=tokencraft", "--pwfile", password_file, cwd=directory, env=env)
    password_file.unlink()
    db_port = loopback_port()
    pg_env = dict(env, PGHOST=HOST, PGPORT=str(db_port), PGUSER="tokencraft", PGPASSWORD=password)

    def db_ready():
        return subprocess.run([pg / "pg_isready", "-q", "-t", "1", "-d", "postgres"],
                              env=pg_env, timeout=3, check=False).returncode == 0

    with ExitStack() as stack:
        stack.enter_context(service("postgres", [pg / "postgres", "-D", data, "-h", HOST, "-p", str(db_port),
            "-c", "unix_socket_directories="], directory, env, db_ready, shutdown=signal.SIGINT))
        command(pg / "createdb", "-w", "--maintenance-db=postgres", "tokencraft", cwd=directory, env=pg_env)
        command(admin, "migrate", cwd=directory, env=dict(env, FOUNDATION_ENV="development",
            DATABASE_URL=f"postgresql://tokencraft:{password}@{HOST}:{db_port}/tokencraft"))
        command(pg / "psql", "-Xqw", "-d", "tokencraft", "-v", "runtime_role=tokencraft_runtime",
                "-f", FIXTURES / "create-runtime-role.sql", "-f", ROOT / "deploy/postgres/grant-runtime.sql",
                cwd=directory, env=dict(pg_env, TOKENCRAFT_RUNTIME_APP_PASSWORD=runtime_password))
        api_port = loopback_port()
        settings = dict(env, FOUNDATION_ENV="development", TOKENCRAFT_PROCESS_ROLE="api",
            DATABASE_URL=f"postgresql://tokencraft_runtime:{runtime_password}@{HOST}:{db_port}/tokencraft",
            APP_BASE_URL=f"http://localhost:{api_port}", API_GATEWAY_PORT=str(api_port),
            COOKIE_DOMAIN="localhost", COOKIE_SECURE="false", SESSION_SECRET=token_hex(32),
            TOKENCRAFT_CORE_CATALOG=str(FIXTURES / "runtime-catalog.json"), TOKENCRAFT_WORKFLOW_KEY=token_hex(32))
        with service("setup-api", [gateway], directory, settings, lambda: ready(api_port)):
            session, account, project = provision(api_port)

        media_port, token = loopback_port(), token_hex(32)
        stack.enter_context(service("delivery", [sys.executable, "-B", MEDIA / "server.py", "--pack", "delivery",
            "--host", HOST, "--port", str(media_port), "--state", directory / "delivery"], directory,
            dict(env, TOKENCRAFT_MEDIA_SERVICE_TOKEN=token), lambda: ready(media_port)))
        registry = directory / "bindings.json"
        registry.write_text(json.dumps({"version": 1, "bindings": [{
            "name": "fixture-delivery", "revision": "v1", "origin": f"http://{HOST}:{media_port}/",
            "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_FIXTURE_DELIVERY", "manifest": str(MEDIA / "delivery.manifest.json"),
            "account": account, "projects": [project],
        }]}))
        settings.update(TOKENCRAFT_WORKFLOW_TOOL_BINDINGS=str(registry), TOKENCRAFT_CONNECTOR_TOKEN_FIXTURE_DELIVERY=token)
        worker_port = loopback_port()
        stack.enter_context(service("worker", [gateway], directory,
            dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port)), lambda: ready(worker_port)))
        with service("api", [gateway], directory, settings, lambda: ready(api_port)):
            verify_workflow(api_port, session, account, project)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path, help="compiled api-gateway binary")
    parser.add_argument("admin", type=Path, help="compiled admin-cli binary from the same checkout")
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-workflow-") as temp:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(temp))


if __name__ == "__main__":
    main()
