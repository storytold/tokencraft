"""Exercise a published workflow through the native package's backup and restore."""

from contextlib import contextmanager
from dataclasses import dataclass
from hashlib import sha256
import json
from secrets import token_hex
import subprocess
import sys

from smoke_workflow_contract import (
    call, command, loopback_port, provision, ready, service, verify_output, verify_signed_delivery, wait_for_run,
)


@dataclass(frozen=True)
class PublishedRun:
    endpoint: str
    receipt: dict
    auth: dict
    idempotency_key: str
    content: bytes


def prepare_webhook(port, web_port, receiver):
    session, account, project = provision(port, origin=f"http://localhost:{web_port}")
    project_path = f"/v1/projects/{project}"
    query = f"?account_id={account}"
    destinations, _ = call(port, project_path + "/webhook-destinations" + query, 200, headers=session)
    assert destinations == ["native"]
    client = "native-backup-client"
    subscription, _ = call(port, project_path + "/webhooks" + query, 201, method="POST", headers=session, body={
        "name": "Native backup receiver", "destination_id": "native", "client_ref": client,
    })
    principal, _ = call(port, project_path + "/service-principals" + query, 201, method="POST", headers=session, body={
        "name": "Native backup sender", "client_ref": client, "scopes": ["requests:submit", "requests:read"],
    })
    auth = {"Authorization": "Bearer " + principal["token"], "Origin": session["Origin"]}

    def verify():
        quote, _ = call(port, "/v1/quotes/scoped", 200, method="POST", headers=auth, body={
            "model": "example_image", "workload": {"kind": "image", "count": 1, "aspect_ratio": "4:3"},
        })
        receipt, _ = call(port, "/v1/requests", 202, method="POST",
                          headers={**auth, "Idempotency-Key": token_hex(16)}, body={
            "quote_id": quote["id"], "input": quote["input"], "client_ref": client,
            "image_prompt": "Synthetic webhook fixture",
            "delivery": {"webhook_subscription_id": subscription["id"]},
        })
        verify_signed_delivery(port, project_path + "/webhook-deliveries", query, session, receiver,
            subscription, "request.succeeded", {
                "request_id": receipt["id"], "project_id": project, "status": "succeeded",
                "fetch_url": receipt["status_url"], "client_ref": client,
            })

    return verify


@contextmanager
def packaged_delivery_tool(work, media, env, account, project):
    port, token = loopback_port(), token_hex(32)
    profile = work / "reviewed-tools"
    profile.mkdir()
    (profile / "delivery.json").write_bytes((media / "delivery.manifest.json").read_bytes())
    registry = profile / "tool-bindings.json"
    token_env = "TOKENCRAFT_CONNECTOR_TOKEN_NATIVE_DELIVERY"
    registry.write_text(json.dumps({"version": 1, "bindings": [{
        "name": "native-delivery", "revision": "v1", "origin": f"http://127.0.0.1:{port}/",
        "token_env": token_env, "manifest": "delivery.json", "account": account, "projects": [project],
    }]}), encoding="utf-8")
    with service("packaged-delivery", [sys.executable, "-B", media / "server.py", "--pack", "delivery",
                 "--port", str(port), "--state", work / "delivery-state"], work,
                 dict(env, TOKENCRAFT_MEDIA_SERVICE_TOKEN=token), lambda: ready(port)):
        yield registry, {token_env: token}


def verify_workflow_outputs(port, receipt, auth):
    wait_for_run(port, receipt, auth)
    content, _ = call(port, receipt["status_url"] + "/outputs/image", 200, headers=auth)
    assert content.startswith(b"\x89PNG\r\n\x1a\n")
    verify_output(port, receipt, auth, sha256(content).hexdigest(), len(content))
    return content


def prepare_workflow(port, session, account, project):
    project_path = f"/v1/projects/{project}"
    scope = f"?account_id={account}"
    tools, _ = call(port, project_path + "/workflow-tools" + scope + "&verify=native-delivery", 200, headers=session)
    (binding,) = tools
    assert binding["reachable"] and binding["manifest"]["plugin"] == "tokencraft-delivery"
    sibling, _ = call(port, "/v1/projects" + scope, 201, method="POST", headers=session,
                      body={"name": "Unbound native project"})
    hidden, _ = call(port, f"/v1/projects/{sibling['id']}/workflow-tools" + scope, 200, headers=session)
    assert hidden == []
    draft, _ = call(port, project_path + "/workflows" + scope, 201, method="POST", headers=session, body={
        "schema": 1, "title": "Native backup fixture", "max_usd_nanos": 0, "inputs": {},
        "nodes": [
            {"id": "generate", "title": "Generate", "kind": "image", "model": "example_image",
             "max_usd_nanos": 0, "settings": {
                 "prompt": {"source": "fixed", "value": "Synthetic recovery fixture"},
                 "aspect_ratio": {"source": "fixed", "value": "4:3"},
             }},
            {"id": "fingerprint", "title": "Fingerprint image", "kind": "tool", "model": "delivery.fingerprint",
             "max_usd_nanos": 0, "settings": {"source": {"source": "step", "step": "generate"}},
             "tool": {"binding_revision": binding["revision"], "revision": 1, "inputs": {"source": "media"},
                      "output": "metadata", "parameters": {}}},
        ],
        "outputs": {"image": "generate", "fingerprint": "fingerprint"},
    })
    workflow = project_path + f"/workflows/{draft['id']}"
    trial, _ = call(port, workflow + "/test-runs" + scope, 202, method="POST",
                    headers={**session, "Idempotency-Key": token_hex(16)},
                    body={"inputs": {}, "revision": draft["revision"]})
    wait_for_run(port, trial, session)
    call(port, workflow + "/versions" + scope, 200, method="POST", headers=session,
         body={"revision": draft["revision"], "active_version": None})
    app, _ = call(port, workflow + "/apps" + scope, 201, method="POST", headers=session,
                  body={"name": "Native backup app", "days": 1,
                        "scopes": ["runs:submit", "runs:read", "runs:cancel", "outputs:read"]})
    auth = {"Authorization": "Bearer " + app["secret"], "Origin": session["Origin"]}
    endpoint = project_path + f"/workflow-endpoints/{draft['id']}/runs"
    key = token_hex(16)
    receipt, _ = call(port, endpoint, 202, method="POST", headers={**auth, "Idempotency-Key": key},
                      body={"inputs": {}})
    content = verify_workflow_outputs(port, receipt, auth)
    print("PASS packaged media runner fingerprinted generated output and stayed scoped to its project")
    return PublishedRun(endpoint, receipt, auth, key, content)


def defer_workflow(work, state, pg_bin, db_port, env, receipt):
    # Only the generated package test state is used. Park a separate run without a child request.
    keys = json.loads((state / "dev-keys.json").read_bytes())
    credentials = dict(env, PGHOST="127.0.0.1", PGPORT=str(db_port), PGUSER="tokencraft",
                       PGPASSWORD=keys["database"])
    parked = "wfr_" + token_hex(13)
    fixture = work / "park-workflow.sql"
    fixture.write_text("""INSERT INTO workflow_runs
        (id, account_id, project_id, workflow_id, actor_id, version, draft_revision,
         idempotency_digest, intent_digest, status, graph, next_at)
        SELECT :'parked', account_id, project_id, workflow_id, actor_id, version, draft_revision,
            repeat('1',64), repeat('2',64), 'reconciling', graph, now() + interval '1 hour'
        FROM workflow_runs WHERE id = :'finished'
        RETURNING id;
    """)
    inserted = command(pg_bin / "psql", "-XqAtw", "-d", "tokencraft_dev", "-v", "ON_ERROR_STOP=1",
                       "-v", f"parked={parked}", "-v", f"finished={receipt['id']}", "-f", fixture,
                       cwd=work, env=credentials)
    assert inserted == parked
    return parked


def verify_backup(launcher, port, env, state, launch, workflow, parked, verify_webhook):
    work = state.parent
    backup, restored = work / "backup", work / "restored"
    rejected = subprocess.run([launcher, "backup", state, backup], cwd=work, env=env, capture_output=True,
                              text=True, timeout=25)
    assert rejected.returncode != 0 and "finish queued, running, or reconciling workflows" in rejected.stderr
    assert not backup.exists()
    print("PASS native backup rejected a reconciling workflow before publishing a backup")
    with launch(state):
        call(port, f"{workflow.endpoint}/{parked}/cancel", 204, method="POST", headers=workflow.auth)
    command(launcher, "backup", state, backup, cwd=work, env=env)
    command(launcher, "restore", backup, restored, cwd=work, env=env)
    with launch(restored):
        replay, _ = call(port, workflow.endpoint, 200, method="POST", body={"inputs": {}},
                         headers={**workflow.auth, "Idempotency-Key": workflow.idempotency_key})
        assert replay == {**workflow.receipt, "replayed": True}
        path = workflow.receipt["status_url"]
        saved, _ = call(port, path, 200, headers=workflow.auth)
        assert saved["id"] == workflow.receipt["id"] and saved["status"] == "succeeded"
        actual = verify_workflow_outputs(port, workflow.receipt, workflow.auth)
        assert actual == workflow.content
        new_run, _ = call(port, workflow.endpoint, 202, method="POST", body={"inputs": {}},
                          headers={**workflow.auth, "Idempotency-Key": token_hex(16)})
        assert new_run["id"] != workflow.receipt["id"]
        verify_workflow_outputs(port, new_run, workflow.auth)
        verify_webhook()
    print("PASS native backup restored the app key, receipt, outputs, and pinned workflow tool")
    print("PASS restored native state signed a new client webhook with the original subscription secret")
