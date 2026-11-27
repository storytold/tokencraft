"""Verify that request diagnostics select the newest matching history at volume."""

import argparse
from pathlib import Path
from tempfile import TemporaryDirectory
from urllib.parse import urlsplit

from postgres_tools import postgres_bin
from smoke_workflow_contract import call, command, ready, rehearse, service


def request_id(number):
    return f"req_{number:026d}"


def seed_history(directory, settings, account, project):
    database = urlsplit(settings["DATABASE_URL"])
    env = dict(PATH=settings["PATH"], PGHOST=database.hostname, PGPORT=str(database.port),
               PGDATABASE=database.path.lstrip("/"), PGUSER=database.username, PGPASSWORD=database.password)
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
    command(postgres_bin() / "psql", "-XqAtw", "-v", "ON_ERROR_STOP=1", "-v", f"account={account}",
            "-v", f"project={project}", "-f", seed, cwd=directory, env=env)


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
