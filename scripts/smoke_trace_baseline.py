"""Verify that request diagnostics select the newest matching history at volume."""

import argparse
import json
from pathlib import Path
from tempfile import TemporaryDirectory
from urllib.parse import urlsplit

from postgres_tools import postgres_bin
from smoke_workflow_contract import call, command, ready, rehearse, service


def request_id(number):
    return f"req_{number:026d}"


def run_seed(directory, settings, account, project, seed, **values):
    database = urlsplit(settings["DATABASE_URL"])
    env = dict(PATH=settings["PATH"], PGHOST=database.hostname, PGPORT=str(database.port),
               PGDATABASE=database.path.lstrip("/"), PGUSER=database.username, PGPASSWORD=database.password)
    variables = {"account": account, "project": project, **values}
    args = [part for key, value in variables.items() for part in ("-v", f"{key}={value}")]
    command(postgres_bin() / "psql", "-XqAtw", "-v", "ON_ERROR_STOP=1", *args, "-f", seed, cwd=directory, env=env)


def seed_history(directory, settings, account, project):
    seed = directory / "trace-history.sql"
    seed.write_text("""
        WITH fixtures AS (
            SELECT n, 'sample' AS kind FROM generate_series(1, 55) n
            UNION ALL
            SELECT n, 'other_model' FROM generate_series(1000, 1999) n
            UNION ALL
            SELECT * FROM (VALUES
                (60, 'too_old'), (61, 'finished_late'), (62, 'other_client'), (63, 'other_app'),
                (64, 'other_workload'), (65, 'other_profile'), (66, 'other_binding'),
                (67, 'other_upstream'), (68, 'other_offer'), (69, 'other_shape'), (2000, 'subject')
            ) AS excluded(n, kind)
        ), dates AS (
            SELECT n, kind, TIMESTAMPTZ '2026-11-24 00:00:00+00' AS subject_at
            FROM fixtures
        ), dated AS (
            SELECT *, CASE kind
                WHEN 'subject' THEN subject_at
                WHEN 'too_old' THEN subject_at - interval '169 hours'
                ELSE subject_at - interval '2 hours' + n * interval '1 second'
            END AS created_at
            FROM dates
        ), inserted AS (
            INSERT INTO gateway_requests (
                id, account_id, project_id, actor_id, idempotency_digest, intent_digest, client_ref, app_ref,
                model, offer_id, provider, adapter, quote, estimated_usd_nanos, created_at, dispatched_at, succeeded_at
            )
            SELECT 'req_' || lpad(n::text, 26, '0'), :'account', :'project', 'trace-fixture',
                   lpad(n::text, 64, '0'), repeat('a', 64),
                   CASE WHEN kind = 'other_client' THEN 'client-b' ELSE 'client-a' END,
                   CASE WHEN kind = 'other_app' THEN 'app-b' ELSE 'app-a' END,
                   CASE WHEN kind = 'other_model' THEN 'distractor' ELSE 'trace-model' END,
                   'trace-offer', 'self_hosted', 'connector_queue',
                   jsonb_build_object(
                       'profile', CASE WHEN kind = 'other_profile' THEN 'queue_media' ELSE 'native' END,
                       'input', jsonb_build_object('workload', jsonb_build_object(
                           'kind', 'image', 'count', CASE WHEN kind = 'other_workload' THEN 2 ELSE 1 END,
                           'aspect_ratio', '1:1')),
                       'candidates', jsonb_build_array(jsonb_build_object(
                           'offer_id', 'trace-offer',
                           'applied_aspect_ratio', CASE WHEN kind = 'other_shape' THEN '4:3' ELSE '1:1' END))),
                   0, created_at, created_at + interval '100 milliseconds',
                   CASE WHEN kind = 'finished_late' THEN subject_at + interval '1 second'
                        ELSE created_at + (400 + n * 10) * interval '1 millisecond' END
            FROM dated ORDER BY n
            RETURNING id, created_at, succeeded_at
        )
        INSERT INTO gateway_provider_runs
            (request_id, binding_digest, upstream_model, accepted_at, upstream_handle, result_observed_at, route_retries)
        SELECT id, repeat(CASE WHEN kind = 'other_binding' THEN 'b' ELSE 'a' END, 64),
               CASE WHEN kind = 'other_upstream' THEN 'alternate' ELSE 'origin-model' END,
               i.created_at + interval '200 milliseconds', 'job_' || n,
               i.succeeded_at - interval '50 milliseconds',
               CASE WHEN kind = 'other_offer' THEN '[{"decision":{"next_offer_id":"alternate"}}]'::jsonb
                    ELSE '[]'::jsonb END
        FROM inserted i JOIN fixtures f ON i.id = 'req_' || lpad(f.n::text, 26, '0');
    """, encoding="utf-8")
    run_seed(directory, settings, account, project, seed)


def seed_warning_history(port, directory, settings, account, project, session):
    scope = f"account_id={account}&project_id={project}"
    quote, _ = call(port, "/v1/quotes/scoped?" + scope, 200, method="POST", headers=session,
                    body={"model": "runtime_image", "workload": {"kind": "image", "count": 1, "aspect_ratio": "4:3"}})
    seed = directory / "warning-history.sql"
    seed.write_text("""
        INSERT INTO gateway_webhook_subscriptions (id, account_id, project_id, name, destination_id, destination_url)
        VALUES ('whs_fixture', :'account', :'project', 'Warning fixture', 'fixture', 'https://fixture.invalid');

        INSERT INTO gateway_requests (
            id, account_id, project_id, actor_id, idempotency_digest, intent_digest, client_ref, app_ref,
            model, offer_id, provider, adapter, quote, estimated_usd_nanos, created_at, dispatched_at,
            succeeded_at, failed_at, uncertain_at, webhook_subscription_id
        )
        SELECT 'req_' || lpad(n::text, 26, '0'), :'account', :'project', 'warning-fixture',
               lpad(n::text, 64, '0'), repeat('a', 64), CASE WHEN n = 3037 THEN 'client-b' ELSE 'client-a' END,
               'warning-fixture', 'runtime_image', 'warning-offer', 'self_hosted',
               CASE WHEN n IN (3034, 3035) THEN 'openai_compatible_chat' ELSE 'connector_queue' END,
               :'quote'::jsonb, 0, at, at,
               CASE WHEN n NOT BETWEEN 3000 AND 3026 AND n NOT IN (3027, 3034, 3037) THEN at END,
               CASE WHEN n = 3027 THEN at END,
               CASE WHEN n BETWEEN 3000 AND 3026 OR n = 3037 THEN at END,
               CASE WHEN n IN (3030, 3031, 3032) THEN 'whs_fixture' END
        FROM generate_series(3000, 4037) n
        CROSS JOIN LATERAL (SELECT TIMESTAMPTZ '2026-11-24 12:00:00+00' +
            CASE WHEN n = 3036 THEN interval '1 day' ELSE interval '0' END AS at) time
        ORDER BY n;

        INSERT INTO gateway_provider_runs (request_id, binding_digest, upstream_model, route_retries, compute_attempts)
        SELECT id, repeat('a', 64), 'fixture',
               CASE WHEN right(id, 4) IN ('3029', '3036') THEN
                   '[{"at":"2026-11-24T12:00:00Z","decision":{"offer_id":"a","next_offer_id":"b","estimated_usd_nanos":1,"delay_ms":20}}]'::jsonb
                   ELSE '[]'::jsonb END,
               CASE WHEN right(id, 4) IN ('3034', '3035') THEN
                   '[{"started_at":"2026-11-24T12:00:00Z","selection":{"home_cell":"a","stack":"b","target":"c","cloud":"d","region":"e","zone":"f","priority":0}}]'::jsonb
                   ELSE '[]'::jsonb END
        FROM gateway_requests WHERE app_ref = 'warning-fixture';

        INSERT INTO gateway_webhook_deliveries (id, request_id, event_type, occurred_at, payload, attempt_count)
        SELECT 'whd_' || substr(id, 5), id, 'request.succeeded', created_at, '{}',
               CASE WHEN right(id, 4) = '3032' THEN 51 ELSE 1 END
        FROM gateway_requests WHERE webhook_subscription_id = 'whs_fixture';

        INSERT INTO gateway_webhook_attempts (delivery_id, number, finished_at, outcome)
        SELECT d.id, n, clock_timestamp(), CASE
            WHEN right(d.id, 4) IN ('3030', '3032') AND n = 1 THEN 'lease_expired' ELSE 'http_error' END
        FROM gateway_webhook_deliveries d CROSS JOIN LATERAL generate_series(1, d.attempt_count) n;
    """, encoding="utf-8")
    run_seed(directory, settings, account, project, seed, quote=json.dumps(quote, separators=(",", ":")))


def verify_warning_history(port, directory, settings, account, project, session, auth):
    seed_warning_history(port, directory, settings, account, project, session)
    path = "/v1/requests?app_ref=warning-fixture&from=2026-11-24&to=2026-11-25"
    warnings = path + "&diagnostic=warnings"
    expected = [3035, 3030, 3029, *range(3026, 2999, -1)]
    for diagnostic, matches in (
        ("warnings", expected),
        ("failed_or_warning", [*expected[:3], 3027, *expected[3:]]),
    ):
        suffix = f"{path}&diagnostic={diagnostic}"
        page, response = call(port, suffix, 200, headers=auth)
        assert response["Cache-Control"] == "private, no-store"
        assert page["diagnostic"] == diagnostic
        assert [row["id"] for row in page["requests"]] == [request_id(n) for n in matches[:25]]
        assert page["next_after"] == request_id(matches[24])
        older, _ = call(port, suffix + "&after=" + page["next_after"], 200, headers=auth)
        assert [row["id"] for row in older["requests"]] == [request_id(n) for n in matches[25:]]
        assert older["next_after"] is None
    for n in (3026, 3027, 3029, 3030, 3031, 3032, 3034, 3035):
        trace, _ = call(port, f"/v1/requests/{request_id(n)}/trace", 200, headers=auth)
        has_warning = any(step["tone"] == "warning" or any(log["severity"] == "warning" for log in step["logs"])
                          for step in trace["evidence"]["steps"])
        assert has_warning == (n in expected), n
    model, _ = call(port, warnings.replace("/v1/requests", "/v1/model-requests") + "&status=succeeded", 200, headers=auth)
    assert model["diagnostic"] == "warnings"
    assert [row["id"] for row in model["requests"]] == [request_id(n) for n in expected[:3]]
    call(port, warnings + "&client_ref=client-b", 404, headers=auth)
    scoped, _ = call(port, warnings + "&after=" + request_id(3037), 200, headers=auth)
    assert scoped["requests"] == [] and scoped["next_after"] is None
    call(port, path + "&diagnostic=unknown", 400, headers=auth)
    print("PASS warning and failure history paginates scoped durable evidence and agrees with individual traces")


def verify(gateway, directory, settings, session, account, project):
    port = int(settings["API_GATEWAY_PORT"])
    seed_history(directory, settings, account, project)
    with service("trace-baseline-api", [gateway], directory, settings, lambda: ready(port)):
        def key(client):
            path = f"/v1/projects/{project}/service-principals?account_id={account}"
            row, _ = call(port, path, 201, method="POST", headers=session,
                          body={"name": client, "client_ref": client, "scopes": ["requests:read"]})
            return {"Authorization": "Bearer " + row["token"]}

        path = f"/v1/requests/{request_id(2000)}/trace/baseline"
        call(port, path, 404, headers=key("client-b"))
        baseline, _ = call(port, path, 200, headers=key("client-a"))
        assert baseline["offer_id"] == "trace-offer" and baseline["model"] == "trace-model"
        expected = [request_id(n) for n in range(55, 5, -1)]
        assert baseline["request_ids"] == expected, (
            f"Expected 50 recent matching requests, got {len(baseline['request_ids'])}"
        )
        assert baseline["timings"] == [
            {"metric": "elapsed", "request_ms": 20400, "median_ms": 705, "samples": 50},
            {"metric": "queue", "request_ms": 100, "median_ms": 100, "samples": 50},
            {"metric": "publication", "request_ms": 50, "median_ms": 50, "samples": 50},
        ]
        admin_baseline, _ = call(port, f"{path}?account_id={account}&project_id={project}", 200, headers=session)
        assert admin_baseline == baseline
        verify_warning_history(port, directory, settings, account, project, session, key("client-a"))
    print("PASS trace baseline retained the latest 50 scoped matches and exact medians behind 1000 other models")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gateway", type=Path)
    parser.add_argument("admin", type=Path)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-trace-baseline-") as directory:
        rehearse(args.gateway.resolve(strict=True), args.admin.resolve(strict=True), Path(directory), verification=verify)


if __name__ == "__main__":
    main()
