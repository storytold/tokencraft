"""Verify pinned credential rotation and revocation across queued connector calls."""

import argparse
from contextlib import ExitStack
import json
from pathlib import Path
from secrets import token_hex
from tempfile import TemporaryDirectory
from threading import Event
from urllib.parse import urlencode

from smoke_cell_faults import Connector, ConnectorHandler, PIXELS, serve
from smoke_connector_recovery import wait_for_status
from smoke_workflow_contract import HOST, ROOT, call, loopback_port, ready, rehearse, service


class Gate:
    def __init__(self, operation):
        self.operation = operation
        self.entered = Event()
        self.release = Event()


class TenantConnector(ConnectorHandler):
    def credential(self):
        return self.server.labels.get(self.headers.get("X-TokenCraft-Upstream-Key"))

    def do_POST(self):
        key = self.credential()
        if self.path != "/v1/jobs" or not self.authorized() or key is None:
            self.reply(403, {})
            return
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        request = payload["request_id"]
        with self.server.lock:
            self.server.jobs[request] += 1
            self.server.inputs[request] = (key, payload["input"]["prompt"])
            self.server.calls.append((request, "submit", key))
        self.reply(200, {"version": 1, "request_id": request, "outcome": {"status": "accepted", "handle": request}})

    def do_GET(self):
        segments = self.path.split("/")
        if len(segments) not in (4, 6) or segments[1:3] != ["v1", "jobs"] or not self.authorized():
            self.reply(404, {})
            return
        request = segments[3]
        key = self.credential()
        with self.server.lock:
            pinned, name = self.server.inputs.get(request, (None, None))
            operation = "poll" if len(segments) == 4 else f"artifact-{segments[-1]}"
            self.server.calls.append((request, operation, key))
        if key is None or key != pinned:
            self.reply(403, {})
            return
        gate = self.server.gates.get(name)
        if gate is not None and operation == gate.operation:
            gate.entered.set()
            if not gate.release.wait(3):
                self.reply(503, {})
                return
        if operation == "poll":
            self.reply(200, {"version": 1, "status": "succeeded", "handle": request,
                             "provider_charge": {"unit": "credit", "quantity": "1.25"}})
        else:
            self.reply(200, PIXELS, "image/png")


def verify(gateway, directory, settings, session, account, project, case):
    port, worker_port = int(settings["API_GATEWAY_PORT"]), loopback_port()
    account_query = urlencode({"account_id": account})
    keys = {name: token_hex(24) for name in ("organization", "original", "rotated", "replacement")}
    connector = Connector()
    connector.RequestHandlerClass = TenantConnector
    connector.labels = {value: name for name, value in keys.items()}
    connector.calls, connector.inputs, connector.gates = [], {}, {}
    with ExitStack() as stack:
        stack.enter_context(serve(connector))
        settings = dict(settings, TOKENCRAFT_PROVIDER_CREDENTIAL_KEY=token_hex(32),
                        TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE=connector.token,
                        TOKENCRAFT_CORE_CATALOG=str(ROOT / "examples/connector/catalog.json"))
        settings.pop("TOKENCRAFT_LOCAL_CHAT_BINDINGS", None)
        bindings = directory / "tenant-connector.json"
        bindings.write_text(json.dumps({"version": 1, "bindings": [{
            "name": "example", "revision": "tenant-key-fixture", "tenant_keys": True,
            "origin": f"http://{HOST}:{connector.server_port}/", "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE",
        }]}))
        settings["TOKENCRAFT_CONNECTORS"] = str(bindings)
        stack.enter_context(service("key-api", [gateway], directory, settings, lambda: ready(port)))
        worker = dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port))
        stack.enter_context(service("key-worker", [gateway], directory, worker, lambda: ready(worker_port)))

        def principal(target):
            result, _ = call(port, f"/v1/projects/{target}/service-principals?{account_query}", 201,
                             method="POST", headers=session, body={
                                 "name": "Provider credential fixture", "scopes": ["requests:submit", "requests:read"],
                             })
            return {"Authorization": "Bearer " + result["token"]}

        binding = "/v1/provider-credentials/example"
        project_query = account_query + "&" + urlencode({"project_id": project})

        def save(label, expected, query=project_query):
            call(port, binding + "?" + query, 204, method="PUT", headers=session,
                 body={"expected_version": expected, "key": keys[label]})

        def revoke(version):
            call(port, binding + f"/{version}?{project_query}", 204, method="DELETE", headers=session)

        def submit(auth, name, intent, expected=202, *, count=2):
            result, _ = call(port, "/v1/image", expected, method="POST",
                             headers={**auth, "Idempotency-Key": intent}, body={
                                 "model": "example_image", "task": "text_to_image", "prompt": name,
                                 "count": count, "aspect_ratio": "1:1",
                             })
            return result

        def begin(auth, stage, pause):
            gate = Gate(pause)
            connector.gates[stage] = gate
            stack.callback(gate.release.set)
            intent = token_hex(16)
            receipt = submit(auth, stage, intent)
            assert gate.entered.wait(15), f"connector did not reach {pause}"
            return receipt, intent, gate

        def operations(receipt):
            return [(operation, key) for request, operation, key in connector.calls if request == receipt["id"]]

        auth = principal(project)
        save("organization", 0, account_query)
        save("original", 0)
        receipt, intent, gate = begin(auth, "rotate_during_poll", "poll")
        try:
            save("rotated", 1)
        finally:
            gate.release.set()
        wait_for_status(port, receipt, auth, "succeeded", 10)
        assert operations(receipt) == [(operation, "original") for operation in
                                       ("submit", "poll", "artifact-0", "artifact-1")]
        assert submit(auth, "rotate_during_poll", intent, 200) == receipt
        print("PASS an in-flight poll and both artifacts kept the original key after rotation")

        version = 2
        key_label = "rotated"
        scenarios = [("poll", ["submit", "poll"]), ("artifact-0", ["submit", "poll", "artifact-0"])]
        for stage, expected in scenarios:
            if case not in ("both", stage):
                continue
            name = "revoke_at_" + stage
            current, token, gate = begin(auth, name, stage)
            try:
                revoke(version)
            finally:
                gate.release.set()
            wait_for_status(port, current, auth, "awaiting_reconciliation", 10)
            assert operations(current) == [(operation, key_label) for operation in expected]
            native = f"/v1/requests/{current['id']}"
            result, _ = call(port, native, 200, headers=auth)
            assert result["provider_handle"] == current["id"]
            call(port, native + "/result", 404, headers=auth)
            upstream = result["upstream"]
            assert upstream["error_code"] == "provider_credential_unavailable"
            assert upstream["provider_charge"]["unit"] == "credit"
            assert upstream["provider_charge"]["quantity"] == "1.25"
            assert upstream["provider_charge"]["observed_at"]
            assert submit(auth, name, token, 200) == current
            print(f"PASS revocation during {stage} stopped later downloads and preserved the charge and receipt")
            if case == "both" and stage == "poll":
                save("replacement", version)
                version += 1
                key_label = "replacement"

        denied = submit(auth, "revoked_project", token_hex(16), 409)
        assert denied["error"]["code"] == "provider_credential_unavailable"
        metadata, _ = call(port, binding + "?" + project_query, 200, headers=session)
        assert all(key not in json.dumps(metadata) for key in keys.values())
        assert metadata["versions"][0]["status"] == "revoked"
        sibling, _ = call(port, f"/v1/projects?{account_query}", 201, method="POST", headers=session,
                          body={"name": "Inherited provider key"})
        sibling_auth = principal(sibling["id"])
        inherited = submit(sibling_auth, "organization_default", token_hex(16), count=1)
        wait_for_status(port, inherited, sibling_auth, "succeeded", 10)
        assert operations(inherited) == [(operation, "organization") for operation in ("submit", "poll", "artifact-0")]
        assert all(count == 1 for count in connector.jobs.values())
        print("PASS a sibling project retained organization inheritance and every job submitted once")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    parser.add_argument("--case", choices=("both", "poll", "artifact-0"), default="both")
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-provider-credentials-") as temporary:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(temporary),
                 verification=lambda *parameters: verify(*parameters, args.case))


if __name__ == "__main__":
    main()
