"""Verify webhook signing rotation recovery and its authorization boundaries."""

import argparse
from base64 import b64decode, b64encode
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
from hashlib import sha256
import hmac
import json
from pathlib import Path
from secrets import token_hex
from tempfile import TemporaryDirectory
from urllib.parse import urlsplit

from postgres_tools import postgres_bin
from smoke_cell_faults import serve
from smoke_webhook_backlog import Receiver
from smoke_workflow_contract import call, command, loopback_port, provision, ready, rehearse, service


def verify(gateway, directory, settings, session, account, project, *, legacy=False):
    api = int(settings["API_GATEWAY_PORT"])
    with ExitStack() as stack:
        receiver = stack.enter_context(serve(Receiver()))
        config = directory / "receiver.json"
        config.write_text(json.dumps({"destinations": {"fixture": receiver.url("/events")}}))
        settings = dict(settings, TOKENCRAFT_DEMO_WEBHOOK_CONFIG=str(config),
                        TOKENCRAFT_DEMO_WEBHOOK_KEY=token_hex(32))
        stack.enter_context(service("rotation-api", [gateway], directory, settings, lambda: ready(api)))
        session = {**session, "Origin": f"http://localhost:{api}"}
        project_path, query = f"/v1/projects/{project}", f"?account_id={account}"
        subscription, _ = call(api, project_path + "/webhooks" + query, 201, method="POST", headers=session,
                               body={"name": "Rotation fixture", "destination_id": "fixture"})
        path = project_path + "/webhooks/" + subscription["id"]
        key = token_hex(24)
        retry = {**session, "Idempotency-Key": key}
        request = {"expected_signing_version": 1}
        if legacy:
            call(api, path + "/rotate" + query, 200, method="POST", headers=retry)
            call(api, path + "/rotate" + query, 409, method="POST", headers=retry)
            print("REPRODUCED the old endpoint cannot recover a committed rotation response")
            return

        peer = loopback_port()
        stack.enter_context(service("rotation-peer", [gateway], directory,
                                    dict(settings, API_GATEWAY_PORT=str(peer)), lambda: ready(peer)))

        def rotate(port, expected=200, headers=retry, body=request):
            return call(port, path + "/rotations" + query, expected, method="POST", headers=headers, body=body)

        rotate(api, 400, headers=session)
        rotate(api, 400, body={"expected_signing_version": 0})
        rotate(api, 400, headers={**retry, "Idempotency-Key": "short"})
        with ThreadPoolExecutor(max_workers=2) as requests:
            trials = [requests.submit(rotate, api), requests.submit(rotate, peer)]
            [(issued, first), (replayed, second)] = [trial.result() for trial in trials]
        assert issued == replayed and issued["signing_version"] == 2
        assert issued["signing_secret"] != subscription["signing_secret"]
        assert first["Cache-Control"] == second["Cache-Control"] == "private, no-store"
        saved, _ = call(api, path + query, 200, headers=session)
        assert saved == {name: value for name, value in issued.items() if name != "signing_secret"}
        rotate(peer, 409, headers={**retry, "Idempotency-Key": token_hex(24)})

        db = urlsplit(settings["DATABASE_URL"])
        env = dict(PATH=settings["PATH"], PGHOST=db.hostname, PGPORT=str(db.port), PGUSER=db.username,
                   PGPASSWORD=db.password, PGDATABASE=db.path.lstrip("/"))
        statement = directory / "rotation-fixture.sql"

        def sql(value, **variables):
            statement.write_text(value + "\n")
            bindings = {"subscription": subscription["id"], "account": account, **variables}
            args = [item for name, item_value in bindings.items() for item in ("-v", f"{name}={item_value}")]
            return command(postgres_bin() / "psql", "-XqAtw", "-v", "ON_ERROR_STOP=1", *args,
                           "-f", statement, cwd=directory, env=env)

        count = "SELECT count(*) FROM account_audit_logs WHERE target_id = :'subscription' AND account_id = :'account';"
        assert sql(count) == "2"
        digest = sql("SELECT rotation_receipt_digest FROM gateway_webhook_subscriptions WHERE id = :'subscription';")
        assert len(digest) == 64 and key not in digest and issued["signing_secret"] not in digest
        verify_creation(api, peer, project_path, query, session, sql)

        other_session, other_account, _ = provision(api)
        other = {**other_session, "Idempotency-Key": key}
        rotate(api, (403, 404), headers=other)
        sql("""INSERT INTO account_members (id, account_id, user_id, role)
               SELECT :'member', :'account', user_id, 'admin'
                 FROM account_members WHERE account_id = :'other' AND role = 'owner';""",
            other=other_account, member="mem_0" + token_hex(13)[:25])
        call(api, path + query, 200, headers=other)
        rotate(api, 409, headers=other)
        assert rotate(peer)[0] == issued and sql(count) == "2"
        print("PASS administrative replay recovered the same key once per intent and retained private scope")

        principal, _ = call(api, project_path + "/service-principals" + query, 201, method="POST", headers=session,
                            body={"name": "Rotation fixture", "scopes": ["requests:submit", "requests:cancel"]})
        api_key = {"Authorization": "Bearer " + principal["token"]}
        rotate(api, 401, headers={**api_key, "Idempotency-Key": key})
        quote, _ = call(api, "/v1/quotes/scoped", 200, method="POST", headers=api_key,
                        body={"model": "runtime_image", "workload": {"kind": "image", "count": 1, "aspect_ratio": "4:3"}})
        accepted, _ = call(api, "/v1/requests", 202, method="POST", headers={**api_key, "Idempotency-Key": token_hex(16)},
                           body={"quote_id": quote["id"], "input": quote["input"],
                                 "delivery": {"webhook_subscription_id": subscription["id"]}})
        call(api, accepted["status_url"] + "/cancel", 200, method="POST", headers=api_key, body={})
        worker_port = loopback_port()
        worker = dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port))
        stack.enter_context(service("rotation-worker", [gateway], directory, worker, lambda: ready(worker_port)))
        _, headers, raw = receiver.received.get(timeout=12)
        assert json.loads(raw)["data"]["request_id"] == accepted["id"]
        signed = f"{headers['webhook-id']}.{headers['webhook-timestamp']}.".encode() + raw

        def signature(secret):
            return "v1," + b64encode(hmac.digest(b64decode(secret.removeprefix("whsec_")), signed, sha256)).decode()

        assert headers["webhook-signature"].split() == [
            signature(issued["signing_secret"]), signature(subscription["signing_secret"]),
        ]
        sql("""UPDATE gateway_webhook_subscriptions SET signing_rotated_at = now() - interval '25 hours'
               WHERE id = :'subscription';""")
        rotate(api, 409)
        assert call(api, path + query, 200, headers=session)[0]["signing_version"] == 2
        next_retry = {**session, "Idempotency-Key": token_hex(24)}
        next_request = {"expected_signing_version": 2}
        assert rotate(api, headers=next_retry, body=next_request)[0]["signing_version"] == 3
        rotate(api, 409)
        call(api, path + query, 204, method="DELETE", headers=session)
        rotate(peer, 409, headers=next_retry, body=next_request)
        print("PASS both keys signed delivery during overlap, and expired or revoked recovery issued no key")


def verify_creation(api, peer, project_path, query, session, sql):
    creation = project_path + "/webhooks/creations" + query
    intent = {"name": "Creation fixture", "destination_id": "fixture", "client_ref": "creation-fixture"}
    key = token_hex(24)
    receipt = {**session, "Idempotency-Key": key}

    def create(port, status=201, body=intent, headers=receipt):
        return call(port, creation, status, method="POST", headers=headers, body=body)

    create(api, 400, headers=session)
    with ThreadPoolExecutor(max_workers=2) as requests:
        trials = [requests.submit(create, api), requests.submit(create, peer)]
        [(created, first), (recovered, second)] = [trial.result() for trial in trials]
    assert created == recovered and created["signing_version"] == 1
    assert first["Cache-Control"] == second["Cache-Control"] == "private, no-store"
    create(api, 409, body={**intent, "name": "Different subscription"})

    target = project_path + "/webhooks/" + created["id"]
    metadata, _ = call(api, target + query, 200, headers=session)
    assert metadata == {name: value for name, value in created.items() if name != "signing_secret"}
    count = "SELECT count(*) FROM account_audit_logs WHERE target_id = :'subscription' AND account_id = :'account';"
    assert sql(count, subscription=created["id"]) == "1"
    digest = sql("SELECT encode(creation_receipt_digest, 'hex') FROM gateway_webhook_subscriptions WHERE id = :'subscription';",
                 subscription=created["id"])
    assert len(digest) == 64 and key not in digest and created["signing_secret"] not in digest
    sql("UPDATE gateway_webhook_subscriptions SET created_at = now() - interval '25 hours' WHERE id = :'subscription';",
        subscription=created["id"])
    create(peer, 409)
    call(api, target + query, 204, method="DELETE", headers=session)
    create(api, 409)

    fresh = {**session, "Idempotency-Key": token_hex(24)}
    other, _ = create(api, headers=fresh)
    assert other["id"] != created["id"]
    call(api, project_path + "/webhooks/" + other["id"] + "/rotate" + query, 200, method="POST", headers=session)
    create(peer, 409, headers=fresh)
    print("PASS concurrent webhook creation recovered one subscription and rejected changed, expired, revoked, or rotated receipts")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    parser.add_argument("--legacy", action="store_true")
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-webhook-rotation-") as folder:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(folder),
                 verification=lambda *fixture: verify(*fixture, legacy=args.legacy))


if __name__ == "__main__":
    main()
