"""Exercise a published workflow through the native package's backup and restore."""

from dataclasses import dataclass
import json
from secrets import token_hex
import subprocess

from smoke_workflow_contract import call, command, provision, verify_signed_delivery, wait_for_run


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


def prepare_workflow(port, web_port):
    session, account, project = provision(port, origin=f"http://localhost:{web_port}")
    project_path = f"/v1/projects/{project}"
    scope = f"?account_id={account}"
    draft, _ = call(port, project_path + "/workflows" + scope, 201, method="POST", headers=session, body={
        "schema": 1, "title": "Native backup fixture", "max_usd_nanos": 0, "inputs": {},
        "nodes": [{"id": "generate", "title": "Generate", "kind": "image", "model": "example_image",
                   "max_usd_nanos": 0, "settings": {
                       "prompt": {"source": "fixed", "value": "Synthetic recovery fixture"},
                       "aspect_ratio": {"source": "fixed", "value": "4:3"},
                   }}],
        "outputs": {"image": "generate"},
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
    wait_for_run(port, receipt, auth)
    content, _ = call(port, receipt["status_url"] + "/outputs/image", 200, headers=auth)
    assert content.startswith(b"\x89PNG\r\n\x1a\n")
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
        actual, _ = call(port, path + "/outputs/image", 200, headers=workflow.auth)
        assert actual == workflow.content
        verify_webhook()
    print("PASS native backup restored the original workflow app key, replayed receipt, and PNG bytes")
    print("PASS restored native state signed a new client webhook with the original subscription secret")
