"""Verify price expiry and routing policy with a disposable rate feed and connector."""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager, ExitStack
from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
from secrets import token_hex
import signal
import subprocess
from tempfile import TemporaryDirectory
from time import monotonic, sleep
from urllib.parse import urlsplit

from postgres_tools import postgres_bin
from smoke_cell_faults import Connector, eventually, serve
from smoke_connector_recovery import wait_for_status
from smoke_workflow_contract import (
    HOST, ROOT, call, command, loopback_port, provision, ready, rehearse, service, wait_for_run,
)


@contextmanager
def hold_budget(psql, directory, env, account):
    with subprocess.Popen([*psql, "-v", f"account={account}"], cwd=directory, env=env,
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True) as holder:
        try:
            holder.stdin.write("SET statement_timeout = '5s';\nBEGIN;\n"
                               "SELECT 'held' FROM pg_advisory_xact_lock("
                               "hashtextextended('gateway_monthly_budget:' || :'account', 0));\n")
            holder.stdin.flush()
            assert holder.stdout.readline().strip() == "held"
            yield
        finally:
            holder.stdin.close()
            holder.wait(timeout=5)


def verify_automatic_routing(port, policy, project, session, auth, workload, connector):
    def quote(offer_id, **overrides):
        result, _ = call(port, "/v1/quotes/scoped", 200, method="POST", headers=auth,
                         body={"model": "auto", "workload": workload, **overrides})
        assert result["selected_offer_id"] == offer_id, result["selected_offer_id"]
        return result

    quote("primary")
    alternate = ["fixture_alt"]
    call(port, policy, 200, method="PUT", headers=session, body={
        "expected_revision": 1, "policy": {"preferred_providers": alternate},
    })
    inherited = quote("alternate_model")
    assert inherited["routing"]["provider_preference"] == {"source": "organization_default", "value": alternate}
    call(port, f"{policy}&project_id={project}", 200, method="PUT", headers=session, body={
        "expected_revision": 0, "policy": {
            "preferred_providers": ["self_hosted"], "route_ranking": "recent_reliability",
        },
    })
    project_quote = quote("primary")
    assert project_quote["routing"]["provider_preference"]["source"] == "project_default"
    assert project_quote["routing"]["route_ranking"]["value"] == "recent_reliability"
    price = quote("primary", preferred_providers=[])
    assert price["routing"]["route_ranking"] == {"source": "request", "value": "price"}
    quote("primary", preferred_providers=alternate, max_total_usd_nanos=4_000_000)
    threshold = quote(None, min_usage_accounts=5)
    assert {candidate["reason"] for candidate in threshold["candidates"]} == {"below_model_usage"}

    selected = quote("alternate_model", preferred_providers=alternate)
    assert selected["routing"]["provider_preference"] == {"source": "request", "value": alternate}
    body = {"quote_id": selected["id"], "input": selected["input"], "image_prompt": "Automatic routing fixture"}
    attempt = {**auth, "Idempotency-Key": token_hex(16)}
    receipt, _ = call(port, "/v1/requests", 202, method="POST", headers=attempt, body=body)
    stored = wait_for_status(port, receipt, auth, "succeeded", 18)["quote"]
    assert stored["input"]["model"] == "auto" and stored["candidates"][0]["model"] == "alternate_image"
    model_view, _ = call(port, f"/v1/model-requests/{receipt['id']}", 200, headers=auth)
    assert model_view["model"] == "alternate_image"
    usage, _ = call(port, "/v1/usage?model=alternate_image", 200, headers=auth)
    assert usage["totals"]["requests"] == 1 and usage["totals"]["estimated_usd_nanos"] == "4500000"

    call(port, policy, 200, method="PUT", headers=session, body={
        "expected_revision": 2, "policy": {"allowed_providers": ["self_hosted"]},
    })
    quote("primary", preferred_providers=alternate)
    replay, _ = call(port, "/v1/requests", 200, method="POST", headers=attempt, body=body)
    assert replay == receipt and connector.jobs[receipt["id"]] == 1
    rejected, _ = call(port, "/v1/requests", 409, method="POST", body=body,
                       headers={**auth, "Idempotency-Key": token_hex(16)})
    assert rejected["error"]["code"] == "quote_stale"
    print("PASS automatic model routing honors preference precedence, hard limits, and recorded model identity")


def verify_data_routing(port, policy, project, session, auth, workload, connector, worker):
    def set_limits(path, revision, regions, retention):
        call(port, path, 200, method="PUT", headers=session, body={
            "expected_revision": revision,
            "policy": {"data_requirements": {"allowed_regions": regions, "max_retention_seconds": retention}},
        })

    def quote(**requirements):
        return call(port, "/v1/quotes/scoped", 200, method="POST", headers=auth, body={
            "model": "auto", "workload": workload, "data_requirements": requirements,
        })[0]

    def submit(offers, expected, headers):
        return call(port, "/v1/requests", expected, method="POST", headers=headers, body={
            "quote_id": offers["id"], "input": offers["input"], "image_prompt": "Synthetic data handling fixture",
        })[0]

    project_policy = f"{policy}&project_id={project}"
    set_limits(policy, 3, ["eu-west-1", "us-east-1"], 120)
    set_limits(project_policy, 1, ["eu-west-1"], 60)
    constrained = quote(allowed_regions=["us-east-1", "eu-west-1"], max_retention_seconds=90)
    assert constrained["input"]["data_requirements"] == {
        "allowed_regions": ["eu-west-1", "us-east-1"], "max_retention_seconds": 90,
    }
    assert constrained["routing"]["data_requirements"] == {
        "allowed_regions": ["eu-west-1"], "max_retention_seconds": 60,
    }
    assert constrained["selected_offer_id"] == "alternate_model"
    assert {row["offer_id"]: row["reason"] for row in constrained["candidates"]} == {
        "primary": "provider_region_unmet", "backup": "provider_retention_unmet", "alternate_model": "eligible",
    }
    inherited = quote()
    assert inherited["selected_offer_id"] == "alternate_model" and "data_requirements" not in inherited["input"]
    disjoint = quote(allowed_regions=["us-east-1"])
    assert disjoint["selected_offer_id"] is None and disjoint["routing"]["data_requirements"]["allowed_regions"] == []

    key = {**auth, "Idempotency-Key": token_hex(16)}
    accepted = submit(constrained, 202, key)
    assert wait_for_status(port, accepted, auth, "succeeded", 18)["quote"]["input"] == constrained["input"]
    assert submit(constrained, 200, key) == accepted and connector.jobs[accepted["id"]] == 1

    worker.send_signal(signal.SIGSTOP)
    try:
        blocked = submit(constrained, 202, {**auth, "Idempotency-Key": token_hex(16)})
        set_limits(project_policy, 2, ["eu-west-1"], 0)
    finally:
        worker.send_signal(signal.SIGCONT)
    stopped = wait_for_status(port, blocked, auth, "failed", 18)
    assert stopped["upstream"]["error_code"] == "provider_data_handling_blocked"
    assert blocked["id"] not in connector.jobs
    assert submit(constrained, 200, key) == accepted
    print("PASS normalized data limits survive admission and tightened retention blocks queued provider submission")


def worker_rate_probe(port):
    session, account, project = provision(port)
    scope = f"?account_id={account}"
    project_path = f"/v1/projects/{project}"
    draft, _ = call(port, project_path + "/workflows" + scope, 201, method="POST", headers=session, body={
        "schema": 1, "title": "Worker rate refresh", "max_usd_nanos": 4_000_000,
        "inputs": {}, "outputs": {"image": "generate"}, "nodes": [{
            "id": "generate", "title": "Generate an image", "kind": "image", "model": "example_image",
            "max_usd_nanos": 4_000_000, "settings": {
                "prompt": {"source": "fixed", "value": "Synthetic worker rate fixture"},
                "aspect_ratio": {"source": "fixed", "value": "1:1"},
            },
        }],
    })
    management = project_path + f"/workflows/{draft['id']}"

    def verify(quote, connector, *, published=False):
        auth = session
        path = management + "/test-runs" + scope
        body = {"inputs": {}, "revision": draft["revision"]}
        if published:
            call(port, management + "/versions" + scope, 200, method="POST", headers=session,
                 body={"revision": draft["revision"], "active_version": None})
            app, _ = call(port, management + "/apps" + scope, 201, method="POST", headers=session, body={
                "name": "Worker rate fixture", "days": 1,
                "scopes": ["runs:submit", "runs:read", "outputs:read"],
            })
            auth = {"Authorization": "Bearer " + app["secret"]}
            path = project_path + f"/workflow-endpoints/{draft['id']}/runs"
            body = {"inputs": {}}
        receipt, _ = call(port, path, 202, method="POST",
                          headers={**auth, "Idempotency-Key": token_hex(16)}, body=body)
        finished, _ = wait_for_run(port, receipt, auth)
        request = finished["steps"][0]["request"]
        child, _ = call(port, f"/v1/requests/{request}{scope}&project_id={project}", 200, headers=session)
        stored = child["quote"]
        assert stored["selected_offer_id"] == quote["selected_offer_id"] == "primary"
        assert stored["catalog_revision"] == quote["catalog_revision"]
        selected = next(row for row in stored["candidates"] if row["offer_id"] == "primary")
        assert selected["estimated_usd_nanos"] == 4_000_000
        assert connector.jobs[request] == 1

    return verify


def verify(gateway, directory, settings, session, account, project):
    port = int(settings["API_GATEWAY_PORT"])
    principal = f"/v1/projects/{project}/service-principals?account_id={account}"
    policy = f"/v1/routing-policy?account_id={account}"
    db = urlsplit(settings["DATABASE_URL"])
    pg_env = dict(PATH=settings["PATH"], PGHOST=db.hostname, PGPORT=str(db.port), PGUSER=db.username,
                  PGPASSWORD=db.password, PGDATABASE=db.path.lstrip("/"))
    psql = (postgres_bin() / "psql", "-XqwAt", "-v", "ON_ERROR_STOP=1")

    with ExitStack() as stack:
        connector = stack.enter_context(serve(Connector()))
        bindings = directory / "rate-connectors.json"
        bindings.write_text(json.dumps({"version": 1, "bindings": [{
            "name": "example", "revision": "rate-fixture",
            "origin": f"http://{HOST}:{connector.server_port}/", "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE",
        }]}))
        offer = json.loads((ROOT / "examples/connector/catalog.json").read_bytes())["offers"][0]
        primary = {**offer, "id": "primary", "pricing": {**offer["pricing"], "usd_nanos_per_image": 3_000_000},
                   "data_handling": {"processing_regions": ["us-east-1"], "retention_seconds": 0}}
        backup = {**offer, "id": "backup", "provider": "fixture_alt",
                  "pricing": {**offer["pricing"], "usd_nanos_per_image": 5_000_000},
                  "data_handling": {"processing_regions": ["eu-west-1"], "retention_seconds": 180}}
        alternate = {**backup, "id": "alternate_model", "model": "alternate_image",
                     "pricing": {**offer["pricing"], "usd_nanos_per_image": 4_500_000},
                     "data_handling": {"processing_regions": ["eu-west-1"], "retention_seconds": 30}}
        catalog = directory / "reviewed-rates.json"
        catalog.write_text(json.dumps({"version": 1, "rate_feed_offers": ["primary"],
                                       "offers": [primary, backup, alternate]}))
        settings = dict(settings, TOKENCRAFT_CORE_CATALOG=str(catalog), TOKENCRAFT_CONNECTORS=str(bindings),
                        TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE=connector.token)
        settings.pop("TOKENCRAFT_LOCAL_CHAT_BINDINGS", None)
        report = json.loads(command(gateway, "catalog", "check-rate-feed", "--catalog", catalog,
                                    cwd=directory, env=settings))
        observed = datetime.now(timezone.utc)
        expiry = observed + timedelta(seconds=10)
        feed = directory / "fixture-rates.json"
        feed.write_text(json.dumps({"base_revision": report["base_revision"], "rates": {
            "primary": {"source_url": "https://supplier.example/prices", "observed_at": observed.isoformat(),
                        "valid_until": expiry.isoformat(),
                        "schedule": {"kind": "image", "usd_nanos_per_image": 2_000_000}},
        }}))
        settings["TOKENCRAFT_DEMO_RATE_FEED"] = str(feed)
        stack.enter_context(service("rate-api", [gateway], directory, settings, lambda: ready(port)))
        row, _ = call(port, principal, 201, method="POST", headers=session,
                      body={"name": "Rate admission smoke", "scopes": ["requests:submit", "requests:read", "usage:read"]})
        auth = {"Authorization": "Bearer " + row["token"]}
        call(port, policy, 200, method="PUT", headers=session, body={
            "expected_revision": 0, "policy": {"monthly_estimated_budget_usd_nanos": 20_000_000},
        })
        prompt = {"model": "example_image", "task": "text_to_image", "count": 1, "aspect_ratio": "1:1",
                  "prompt": "Synthetic price freshness fixture"}
        workload = {"kind": "image", "count": 1, "aspect_ratio": "1:1"}

        def run_image(key, expected, cap):
            return call(port, "/v1/image", expected, method="POST", body=prompt,
                        headers={**auth, "Idempotency-Key": key, "X-TokenCraft-Max-Cost-Nanos": str(cap)})[0]

        def quote(cap):
            return call(port, "/v1/quotes/scoped", 200, method="POST", headers=auth, body={
                "model": "example_image", "workload": workload, "max_total_usd_nanos": cap,
            })[0]

        before = quote(5_000_000)
        assert before["selected_offer_id"] == "primary"
        assert before["candidates"][0]["estimated_usd_nanos"] == 2_000_000
        within_window = next(candidate for candidate in quote(1_000_000)["candidates"]
                             if candidate["offer_id"] == "primary")
        assert within_window["reason"] == "above_cost_cap" and within_window["estimated_usd_nanos"] == 2_000_000
        accepted_key, race_key = token_hex(16), token_hex(16)
        accepted = run_image(accepted_key, 202, 3_000_000)
        delay = (expiry - datetime.now(timezone.utc)).total_seconds() - 2.5
        sleep(max(0, delay))
        with ThreadPoolExecutor(max_workers=1) as tasks:
            with hold_budget(psql, directory, pg_env, account):
                pending = tasks.submit(run_image, race_key, 409, 3_000_000)
                blocked = ("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE wait_event = 'advisory' "
                           "AND query LIKE '%gateway_monthly_budget:%')")
                eventually(lambda: command(*psql, "-c", blocked, cwd=directory, env=pg_env) == "t",
                           "automatic admission at the budget lock", timeout=2)
                assert datetime.now(timezone.utc) < expiry
                sleep(max(0, (expiry - datetime.now(timezone.utc)).total_seconds()) + .05)
                assert not pending.done()
            assert pending.result(timeout=5)["error"]["code"] == "quote_stale"
        budget, _ = call(port, policy, 200, headers=session)
        assert budget["monthly_budget"]["reserved_usd_nanos"] == "2000000"
        assert run_image(accepted_key, 200, 3_000_000) == accepted
        print("PASS expiry while waiting on budget rolled back admission and preserved the accepted replay")

        capped = quote(1_000_000)
        assert capped["selected_offer_id"] is None
        expired = next(candidate for candidate in capped["candidates"] if candidate["offer_id"] == "primary")
        assert expired["reason"] == "price_stale" and expired.get("estimated_usd_nanos") is None
        fallback = quote(5_000_000)
        assert fallback["selected_offer_id"] == "backup"
        assert run_image(token_hex(16), 400, 3_000_000)["error"]
        replacement = run_image(race_key, 202, 5_000_000)
        assert replacement["id"] != accepted["id"]
        assert not connector.jobs
        worker_port = loopback_port()
        worker_settings = dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(worker_port))
        worker = stack.enter_context(service("rate-worker", [gateway], directory, worker_settings, lambda: ready(worker_port)))
        for receipt in (accepted, replacement):
            wait_for_status(port, receipt, auth, "succeeded", 18)
        usage, _ = call(port, "/v1/usage", 200, headers=auth)
        assert usage["totals"]["requests"] == 2 and usage["totals"]["estimated_usd_nanos"] == "7000000"
        assert connector.jobs == {accepted["id"]: 1, replacement["id"]: 1}
        print("PASS the expired price disappeared and the next permitted request used the reviewed fallback")

        updated = json.loads(feed.read_bytes())
        observed = datetime.now(timezone.utc)
        updated["rates"]["primary"].update(
            observed_at=observed.isoformat(), valid_until=(observed + timedelta(hours=1)).isoformat(),
            schedule={"kind": "image", "usd_nanos_per_image": 4_000_000},
        )
        candidate = feed.with_suffix(".next.json")
        candidate.write_text(json.dumps(updated))
        command(gateway, "catalog", "check-rate-feed", "--catalog", catalog, "--previous-feed", feed,
                "--feed", candidate, cwd=directory, env=settings)
        candidate.replace(feed)
        published_at = monotonic()
        eventually(lambda: quote(4_000_000)["selected_offer_id"] == "primary", "the reviewed rate reload")
        assert quote(4_000_000)["candidates"][0]["estimated_usd_nanos"] == 4_000_000
        assert quote(3_000_000)["selected_offer_id"] is None
        rejection, _ = call(port, "/v1/requests", 409, method="POST",
                            headers={**auth, "Idempotency-Key": token_hex(16)}, body={
                                "quote_id": fallback["id"], "input": fallback["input"], "image_prompt": prompt["prompt"],
                            })
        assert rejection["error"]["code"] == "quote_stale"
        assert run_image(accepted_key, 200, 3_000_000) == accepted
        admitted = run_image(token_hex(16), 202, 4_000_000)
        wait_for_status(port, admitted, auth, "succeeded", 18)
        usage, _ = call(port, "/v1/usage", 200, headers=auth)
        assert usage["totals"]["requests"] == 3 and usage["totals"]["estimated_usd_nanos"] == "11000000"
        print("PASS an atomic rate refresh selected the cheaper reviewed offer and invalidated the old quote")
        probe = worker_rate_probe(port)
        # The API and worker have independent 15-second refresh schedules.
        sleep(max(0, published_at + 16 - monotonic()))
        probe(quote(4_000_000), connector)
        assert worker.poll() is None
        print("PASS the same worker admitted a workflow child at the refreshed rate")

        valid = feed.read_bytes()
        updated["rates"]["primary"]["schedule"]["usd_nanos_per_image"] = 2_000_000
        candidate.write_text(json.dumps(updated))
        candidate.replace(feed)
        try:
            def rejected():
                message = "catalog rate feed rejected"
                return all(message in (directory / f"rate-{role}.log").read_text() for role in ("api", "worker"))

            eventually(rejected, "API and worker rate rejection")
            accepted_quote = quote(4_000_000)
            assert accepted_quote["candidates"][0]["estimated_usd_nanos"] == 4_000_000
            probe(accepted_quote, connector, published=True)
            print("PASS a published workflow kept the accepted rate after both replicas rejected a conflicting observation")
        finally:
            candidate.write_bytes(valid)
            candidate.replace(feed)

        verify_automatic_routing(port, policy, project, session, auth, workload, connector)
        verify_data_routing(port, policy, project, session, auth, workload, connector, worker)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-rate-admission-") as temporary:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(temporary), verification=verify)


if __name__ == "__main__":
    main()
