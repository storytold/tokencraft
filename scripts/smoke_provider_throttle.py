"""Check connector cooldowns, retries, and callback recovery with a disposable database."""

import argparse
from contextlib import contextmanager, ExitStack
import json
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
from urllib.parse import urlsplit

from postgres_tools import postgres_bin
from smoke_cell_faults import Connector, ConnectorHandler, eventually, serve
from smoke_connector_recovery import wait_for_status
from smoke_workflow_contract import HOST, ROOT, call, command, loopback_port, ready, rehearse, service


class ThrottledConnector(ConnectorHandler):
    def do_POST(self):
        if self.path != "/v1/jobs" or not self.authorized():
            self.reply(404, {})
            return
        job = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        request = job["request_id"]
        assert self.headers["Idempotency-Key"] == request
        with self.server.lock:
            self.server.jobs[request] += 1
            attempt = self.server.jobs[request]
        if job["model"] == "callback-image":
            callback = job["completion"]
            call(self.server.gateway_port, callback["path"], 204, method="POST", body={"version": 1},
                 headers={"Authorization": "Bearer " + callback["token"]})
        elif attempt == 1:
            self.server.started.set()
            if not self.server.release.wait(timeout=10):
                self.reply(503, {})
                return
        self.reply(200, {"version": 1, "request_id": request,
                         "outcome": {"status": "unavailable", "retry_after_ms": 2_000}})


@contextmanager
def hold_provider_run(psql, directory, env, request):
    with subprocess.Popen([*psql, "-v", f"request={request}"], cwd=directory, env=env,
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True) as holder:
        try:
            holder.stdin.write("SET statement_timeout = '5s';\nBEGIN;\n"
                               "SELECT pg_backend_pid() FROM gateway_provider_runs "
                               "WHERE request_id = :'request' FOR UPDATE;\n")
            holder.stdin.flush()
            yield int(holder.stdout.readline())
        finally:
            holder.stdin.close()
            holder.wait(timeout=5)


def verify(gateway, directory, settings, session, account, project):
    port, worker_port = int(settings["API_GATEWAY_PORT"]), loopback_port()
    db = urlsplit(settings["DATABASE_URL"])
    pg_env = dict(settings, PGHOST=db.hostname, PGPORT=str(db.port), PGUSER=db.username,
                  PGPASSWORD=db.password, PGDATABASE=db.path.lstrip("/"), PGOPTIONS="-c statement_timeout=5000")
    psql = (postgres_bin() / "psql", "-XqwAt", "-v", "ON_ERROR_STOP=1")

    def query(statement, **values):
        variables = [arg for key, value in values.items() for arg in ("-v", f"{key}={value}")]
        script = directory / "verify-throttle.sql"
        script.write_text(statement + ";\n")
        return command(*psql, *variables, "-f", script, cwd=directory, env=pg_env)

    connector = Connector()
    connector.RequestHandlerClass = ThrottledConnector
    connector.gateway_port = port
    bindings = directory / "connector.json"
    bindings.write_text(json.dumps({"version": 1, "bindings": [{
        "name": "example", "revision": "throttle-v1", "origin": f"http://{HOST}:{connector.server_port}/",
        "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE", "completion_webhooks": True,
    }]}))
    catalog = directory / "catalog.json"
    config = json.loads((ROOT / "examples/connector/catalog.json").read_bytes())
    offer = config["offers"][0]
    price = 1_250_000
    offer["pricing"]["usd_nanos_per_image"] = price
    config["offers"].append({**offer, "id": "callback_image", "model": "callback_image",
                             "upstream_model": "callback-image"})
    catalog.write_text(json.dumps(config))
    settings = dict(settings, TOKENCRAFT_CORE_CATALOG=str(catalog), TOKENCRAFT_CONNECTORS=str(bindings),
                    TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE=connector.token)
    settings.pop("TOKENCRAFT_LOCAL_CHAT_BINDINGS", None)
    worker = dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port))

    with ExitStack() as stack:
        stack.enter_context(serve(connector))
        stack.enter_context(service("throttle-api", [gateway], directory, settings, lambda: ready(port)))
        stack.enter_context(service("throttle-worker", [gateway], directory, worker, lambda: ready(worker_port)))
        key, _ = call(port, f"/v1/projects/{project}/service-principals?account_id={account}", 201,
                      method="POST", headers=session,
                      body={"name": "Throttle fixture", "scopes": ["requests:submit", "requests:read"]})
        auth = {"Authorization": "Bearer " + key["token"]}

        def submit(model):
            return call(port, "/v1/image", (200, 202), method="POST",
                        headers={**auth, "Idempotency-Key": "throttle-" + model},
                        body={"model": model, "task": "text_to_image", "prompt": "fixture",
                              "count": 1, "aspect_ratio": "1:1"})[0]

        receipt = submit("example_image")
        assert connector.started.wait(timeout=8), "Worker did not start the connector submission"
        with hold_provider_run(psql, directory, pg_env, receipt["id"]) as holder:
            connector.release.set()
            eventually(lambda: query(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE :'holder'::integer = ANY(pg_blocking_pids(pid)) "
                "AND query LIKE '%UPDATE gateway_requests r SET dispatched_at = NULL%')", holder=holder,
            ) == "t", "the retry write to wait on the held provider run", timeout=5)
            assert query("SELECT count(*) FROM gateway_queue_cooldowns") == "0", (
                "Cooldown became visible while the retry was still blocked"
            )
        eventually(lambda: query(
            "SELECT EXISTS (SELECT 1 FROM gateway_provider_runs p "
            "JOIN gateway_queue_cooldowns c USING (upstream_model, binding_digest) "
            "WHERE p.request_id = :'request' AND jsonb_array_length(p.route_retries) = 1 "
            "AND c.available_after > statement_timestamp())", request=receipt["id"],
        ) == "t", "the committed cooldown and retry", timeout=5)
        wait_for_status(port, receipt, auth, "failed", 8)
        run = json.loads(query(
            "SELECT json_build_object('error', error_code, 'retries', route_retries) "
            "FROM gateway_provider_runs WHERE request_id = :'request'", request=receipt["id"],
        ))
        assert run["error"] == "provider_rate_limited"
        retry, = run["retries"]
        assert retry["decision"] == {"offer_id": "example_image", "next_offer_id": "example_image",
                                     "estimated_usd_nanos": price, "delay_ms": 2_000}
        assert connector.jobs[receipt["id"]] == 2
        print("PASS cooldown and retry became visible together; one retry kept the admitted offer and price")

        receipt = submit("callback_image")
        wait_for_status(port, receipt, auth, "succeeded", 8)
        assert connector.jobs[receipt["id"]] == 1
        assert query(
            "SELECT count(*) FROM gateway_queue_cooldowns WHERE upstream_model = 'callback-image'"
        ) == "0", "A conflicting throttle response paused a model after its callback was recorded"
        print("PASS callback recovery completed with one submission and no spurious cooldown")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-provider-throttle-") as directory:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(directory), verification=verify)


if __name__ == "__main__":
    main()
