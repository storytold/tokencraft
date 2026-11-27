"""Verify price expiry at quote and admission time with a disposable feed and connector."""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager, ExitStack
from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
from secrets import token_hex
import subprocess
from tempfile import TemporaryDirectory
from time import sleep
from urllib.parse import urlsplit

from postgres_tools import postgres_bin
from smoke_cell_faults import Connector, eventually, serve
from smoke_connector_recovery import wait_for_status
from smoke_workflow_contract import HOST, ROOT, call, command, loopback_port, ready, rehearse, service


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
        primary = {**offer, "id": "primary", "pricing": {**offer["pricing"], "usd_nanos_per_image": 3_000_000}}
        backup = {**offer, "id": "backup", "provider": "fixture_alt",
                  "pricing": {**offer["pricing"], "usd_nanos_per_image": 5_000_000}}
        catalog = directory / "reviewed-rates.json"
        catalog.write_text(json.dumps({"version": 1, "rate_feed_offers": ["primary"], "offers": [primary, backup]}))
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
        worker = dict(settings, TOKENCRAFT_PROCESS_ROLE="worker", API_GATEWAY_PORT=str(loopback_port()))
        worker_port = int(worker["API_GATEWAY_PORT"])
        stack.enter_context(service("rate-worker", [gateway], directory, worker, lambda: ready(worker_port)))
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-rate-admission-") as temporary:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(temporary), verification=verify)


if __name__ == "__main__":
    main()
