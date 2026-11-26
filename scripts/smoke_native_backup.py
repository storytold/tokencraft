"""Exercise a published workflow through the native package's backup and restore."""

from dataclasses import dataclass
import json
from secrets import token_hex
import subprocess

from smoke_workflow_contract import call, command, provision, wait_for_run


@dataclass(frozen=True)
class PublishedRun:
    endpoint: str
    receipt: dict
    auth: dict
    idempotency_key: str
    content: bytes


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


def verify_backup(launcher, port, env, state, launch, workflow, parked):
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
    print("PASS native backup restored the original workflow app key, replayed receipt, and PNG bytes")
