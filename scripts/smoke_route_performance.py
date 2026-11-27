"""Verify scoped reliability and settled-cost ranking in a busy project's history."""

import argparse
from contextlib import ExitStack, contextmanager
import json
from pathlib import Path
from secrets import token_hex
from tempfile import TemporaryDirectory
from urllib.parse import urlsplit

from postgres_tools import postgres_bin
from smoke_cell_faults import Connector, serve
from smoke_workflow_contract import HOST, ROOT, call, command, ready, rehearse, service


def seed_history(directory, settings, donor):
    db = urlsplit(settings["DATABASE_URL"])
    env = dict(PATH=settings["PATH"], PGHOST=db.hostname, PGPORT=str(db.port), PGUSER=db.username,
               PGPASSWORD=db.password, PGDATABASE=db.path.lstrip("/"))
    seed = directory / "rank-history.sql"
    seed.write_text("""
        WITH specimens AS MATERIALIZED (
            SELECT n, CASE WHEN n <= 20 THEN 'cheap' ELSE 'steady' END AS kind
            FROM generate_series(1, 40) n
            UNION ALL
            SELECT * FROM (VALUES (41, 'rerouted'), (42, 'wrong_shape'),
                (43, 'wrong_binding'), (44, 'other_client'), (45, 'today'), (46, 'missing_shape')) AS variants(n, kind)
            UNION ALL SELECT n, 'noise' FROM generate_series(1000, 2999) n
        ), source AS (
            SELECT r.*, p.binding_digest, p.upstream_model
            FROM gateway_requests r JOIN gateway_provider_runs p ON p.request_id = r.id
            WHERE r.id = :'donor'
        ), fixtures AS MATERIALIZED (
            SELECT n, kind, r.account_id, r.project_id, r.actor_id, r.binding_digest, r.upstream_model,
                   coalesce(r.quote #>> CASE WHEN kind IN ('cheap', 'rerouted', 'noise')
                       THEN '{candidates,0,model}'::text[] ELSE '{candidates,1,model}'::text[] END, r.model) AS model,
                   r.adapter, r.app_ref,
                   CASE WHEN kind = 'other_client' THEN 'client-b' ELSE r.client_ref END AS client_ref,
                   CASE WHEN kind IN ('cheap', 'rerouted', 'noise') THEN 'cheap' ELSE 'steady' END AS offer_id,
                   CASE WHEN kind IN ('cheap', 'rerouted', 'noise') THEN 'self_hosted' ELSE 'fixture_alt' END AS provider,
                   CASE kind
                       WHEN 'noise' THEN jsonb_set(r.quote, '{input,workload,count}', '2')
                       WHEN 'wrong_shape' THEN jsonb_set(r.quote, '{candidates,1,applied_aspect_ratio}', '"4:3"')
                       WHEN 'missing_shape' THEN jsonb_set(r.quote, '{candidates}', 'null')
                       ELSE jsonb_set(r.quote, '{candidates,0,applied_aspect_ratio}', '"4:3"')
                   END AS quote,
                   CASE WHEN kind = 'today' THEN date_trunc('day', statement_timestamp(), 'UTC')
                        ELSE date_trunc('day', statement_timestamp(), 'UTC')
                             - interval '24 hours' + n * interval '1 second' END AS created,
                   CASE WHEN n BETWEEN 15 AND 17 OR n = 40 THEN 'failed'
                        WHEN n BETWEEN 18 AND 20 THEN 'uncertain' ELSE 'success' END AS outcome
            FROM specimens CROSS JOIN source r
        ), inserted AS (
            INSERT INTO gateway_requests (
                id, account_id, project_id, actor_id, idempotency_digest, intent_digest,
                client_ref, app_ref, model, offer_id, provider, adapter, quote, estimated_usd_nanos,
                created_at, dispatched_at, succeeded_at, failed_at, uncertain_at
            )
            SELECT 'req_' || lpad(n::text, 26, '0'), account_id, project_id, actor_id, lpad(n::text, 64, '0'),
                   repeat('a', 64), client_ref, app_ref, model, offer_id, provider, adapter,
                   CASE WHEN kind = 'cheap' THEN jsonb_set(quote, '{candidates,0,applied_aspect_ratio}', '"1:1"')
                        ELSE quote END,
                   CASE WHEN offer_id = 'cheap' THEN 100 ELSE 200 END,
                   created, created + CASE WHEN kind = 'today' THEN interval '0'
                                           ELSE interval '100 milliseconds' END,
                   CASE WHEN outcome = 'success' THEN created + CASE kind
                        WHEN 'cheap' THEN interval '450 milliseconds'
                        WHEN 'today' THEN interval '0' ELSE interval '1100 milliseconds' END END,
                   CASE WHEN outcome = 'failed' THEN created + interval '1 second' END,
                   CASE WHEN outcome = 'uncertain' THEN created + interval '1 second' END
            FROM fixtures ORDER BY n
            RETURNING id
        )
        INSERT INTO gateway_provider_runs
            (request_id, binding_digest, upstream_model, accepted_at, upstream_handle, error_code, route_retries)
        SELECT i.id, CASE WHEN kind = 'wrong_binding' THEN repeat('b', 64) ELSE binding_digest END,
               upstream_model, created + CASE WHEN kind = 'today' THEN interval '0'
                                              ELSE interval '200 milliseconds' END, 'job_' || n,
               CASE WHEN outcome = 'failed' THEN 'provider_generation_failed' END,
               CASE WHEN kind = 'rerouted' THEN '[{"decision":{"next_offer_id":"steady"}}]'::jsonb
                    ELSE '[]'::jsonb END
        FROM inserted i JOIN fixtures f ON substring(i.id FROM 5)::integer = f.n WHERE kind <> 'noise';

        INSERT INTO gateway_supplier_settlements
            (request_id, revision, amount_usd_nanos, delta_usd_nanos, evidence_ref, idempotency_digest)
        SELECT 'req_' || lpad(n::text, 26, '0'), 1, CASE WHEN n <= 20 THEN 100 ELSE 40 END,
               0, 'fixture', repeat('c', 64) FROM generate_series(1, 40) n;
        INSERT INTO gateway_supplier_settlements
            (request_id, revision, amount_usd_nanos, delta_usd_nanos, evidence_ref, idempotency_digest)
        VALUES ('req_00000000000000000000000021', 2, 0, 0, 'fixture-credit', repeat('d', 64));
    """, encoding="utf-8")
    command(postgres_bin() / "psql", "-XqAtw", "-v", "ON_ERROR_STOP=1", "-v", f"donor={donor}",
            "-f", seed, cwd=directory, env=env)


@contextmanager
def ranking_api(gateway, directory, settings, *, steady_model="example_image"):
    port = int(settings["API_GATEWAY_PORT"])
    with ExitStack() as stack:
        connector = stack.enter_context(serve(Connector()))
        bindings = directory / "rank-connectors.json"
        bindings.write_text(json.dumps({"version": 1, "bindings": [{
            "name": "example", "revision": "rank-fixture",
            "origin": f"http://{HOST}:{connector.server_port}/", "token_env": "TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE",
        }]}))
        offer = json.loads((ROOT / "examples/connector/catalog.json").read_bytes())["offers"][0]
        catalog = directory / "rank-catalog.json"
        catalog.write_text(json.dumps({"version": 1, "offers": [
            {**offer, "id": name, "provider": provider, "model": model,
             "pricing": {**offer["pricing"], "usd_nanos_per_image": price}}
            for name, provider, model, price in [
                ("cheap", "self_hosted", "example_image", 100), ("steady", "fixture_alt", steady_model, 200),
            ]
        ]}))
        settings = dict(settings, TOKENCRAFT_CORE_CATALOG=str(catalog), TOKENCRAFT_CONNECTORS=str(bindings),
                        TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE=connector.token)
        settings.pop("TOKENCRAFT_LOCAL_CHAT_BINDINGS", None)
        stack.enter_context(service("rank-api", [gateway], directory, settings, lambda: ready(port)))
        yield port, connector


def verify(gateway, directory, settings, session, account, project):
    with ranking_api(gateway, directory, settings) as (port, connector):
        def key(client, *, read=True):
            path = f"/v1/projects/{project}/service-principals?account_id={account}"
            scopes = ["requests:submit", "requests:read"] if read else ["requests:submit"]
            row, _ = call(port, path, 201, method="POST", headers=session,
                          body={"name": client, "client_ref": client, "scopes": scopes})
            return {"Authorization": "Bearer " + row["token"]}

        def quote(headers, **hints):
            return call(port, "/v1/quotes/scoped", 200, method="POST", headers=headers, body={
                "model": "example_image", "workload": {"kind": "image", "count": 1, "aspect_ratio": "1:1"}, **hints,
            })[0]

        def submit(headers, quoted):
            return call(port, "/v1/requests", 202, method="POST",
                        headers={**headers, "Idempotency-Key": token_hex(16)}, body={
                            "quote_id": quoted["id"], "input": quoted["input"], "image_prompt": "Routing smoke fixture",
                        })[0]

        def policy(ranking, revision):
            call(port, f"/v1/routing-policy?account_id={account}", 200, method="PUT", headers=session,
                 body={"expected_revision": revision, "policy": {"route_ranking": ranking}})

        auth = key("client-a")
        initial = quote(auth)
        assert initial["selected_offer_id"] == "cheap"
        seed_history(directory, settings, submit(auth, initial)["id"])
        policy("recent_reliability", 0)
        reliable = quote(auth)
        assert reliable["selected_offer_id"] == "steady", reliable["performance_window"]
        assert reliable["performance_window"]["applied"] is True
        evidence = {row["offer_id"]: row["performance"] for row in reliable["candidates"]}
        assert evidence == {
            "cheap": {"samples": 20, "succeeded": 14, "unconfirmed": 3, "median_ms": 250},
            "steady": {"samples": 22, "succeeded": 21, "unconfirmed": 0, "median_ms": 900},
        }, evidence
        policy("cost_per_completion", 1)
        economic = quote(auth)
        assert economic["selected_offer_id"] == "steady"
        assert economic["performance_window"]["applied"] is True
        costs = {row["offer_id"]: row["performance"] for row in economic["candidates"]}
        assert costs["cheap"]["cost"] == {"settled": 20, "settled_usd_nanos": "2000"}
        assert costs["steady"]["cost"] == {"settled": 20, "settled_usd_nanos": "760"}
        admit = submit(auth, economic)
        saved, _ = call(port, admit["status_url"], 200, headers=auth)
        assert saved["quote"]["selected_offer_id"] == "steady"
        sibling = quote(key("client-b"))
        assert sibling["selected_offer_id"] == "cheap" and not sibling["performance_window"]["applied"]
        hidden = quote(key("client-a", read=False))
        assert hidden["selected_offer_id"] == "steady" and "performance_window" not in hidden
        assert all("performance" not in candidate for candidate in hidden["candidates"])
        assert quote(auth, preferred_providers=[])["selected_offer_id"] == "cheap"
        assert connector.jobs == {}
    print("PASS pricing, reliability and settled-cost routing through 2000 newer incompatible requests")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-route-performance-") as directory:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(directory), verification=verify)


if __name__ == "__main__":
    main()
