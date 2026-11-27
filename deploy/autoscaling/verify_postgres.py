"""Verify quota transactions and the autoscaler query on disposable PostgreSQL."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
import time


HERE = Path(__file__).resolve().parent
ENV = {"LANG": "C"}


def verify(pg_bin, directory):
    data = directory / "pg"
    subprocess.run([
        pg_bin / "initdb", "-D", data, "--no-locale", "--encoding=UTF8", "--auth-local=trust",
        "--auth-host=scram-sha-256", "-U", "capacity_test",
    ], env=ENV, check=True, stdout=subprocess.DEVNULL)
    with (directory / "postgres.log").open("w+") as log:
        process = subprocess.Popen([
            pg_bin / "postgres", "-D", data, "-h", "", "-k", directory,
        ], env=ENV, stdout=log, stderr=log)
        try:
            wait_for_db(pg_bin, directory, process)
            check_queries(pg_bin, directory)
        except BaseException:
            log.seek(0)
            print(log.read())
            raise
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def wait_for_db(pg_bin, directory, process):
    for _ in range(50):
        if process.poll() is not None:
            raise RuntimeError("disposable PostgreSQL failed to start")
        if subprocess.run([
            pg_bin / "pg_isready", "-h", directory, "-U", "capacity_test", "-d", "postgres",
        ], env=ENV, stdout=subprocess.DEVNULL).returncode == 0:
            return
        time.sleep(0.1)
    raise RuntimeError("timed out waiting for disposable PostgreSQL")


def check_queries(pg_bin, directory):
    def query(*args, role="capacity_test"):
        return subprocess.run([
            pg_bin / "psql", "-XqAtw", "-h", directory, "-U", role, "-d", "postgres",
            "-v", "ON_ERROR_STOP=1", *args,
        ], env=ENV, check=True, capture_output=True, text=True).stdout.strip()

    migrations = sorted((HERE.parents[1] / "crates/db/migrations").glob("*.sql"))
    limit_index = next(index for index, path in enumerate(migrations)
                       if path.name.endswith("_gateway_submission_rate_limits.sql"))
    gate_index = next(index for index, path in enumerate(migrations)
                      if path.name.endswith("_gateway_admission_control.sql"))
    query("--single-transaction", *(arg for path in migrations[:limit_index] for arg in ("-f", path)))
    query("-c", "INSERT INTO accounts(id, placeholder_business_name, status) VALUES ('second', 'Existing', 'active')")
    query("--single-transaction", *(arg for path in migrations[limit_index:gate_index] for arg in ("-f", path)))
    assert query("-c", "SELECT * FROM gateway_submission_limit('second')") == "120|20|240||f"
    print("PASS an existing organization inherits the default limit after migration")

    query("-c", """
        INSERT INTO accounts(id, placeholder_business_name, status)
        VALUES ('burst', 'Burst', 'active'), ('tiers', 'Tiers', 'active');
        INSERT INTO gateway_submission_limits(account_id, refilled_at)
        VALUES ('burst', clock_timestamp() + interval '5 minutes');
    """)
    take = "SELECT gateway_consume_submission_credit('burst')"
    with ThreadPoolExecutor(max_workers=8) as pool:
        results = list(pool.map(lambda _: query("-c", take), range(35)))
    assert results.count("0") == 20 and results.count("1") == 15, results
    assert query("-c", "SELECT gateway_consume_submission_credit('second')") == "0"
    before = query("-c", "SELECT row(credit, refilled_at) FROM gateway_submission_limits WHERE account_id = 'second'")
    assert query("-c", "BEGIN; SELECT gateway_consume_submission_credit('second'); ROLLBACK;") == "0"
    assert query("-c", "SELECT row(credit, refilled_at) FROM gateway_submission_limits WHERE account_id = 'second'") == before
    assert query("-c", "SELECT gateway_submission_credit(0,120,interval '500ms')") == "60000000"
    assert query("-c", "SELECT gateway_submission_credit(0,120,interval '500s')") == "1200000000"
    print("PASS 35 concurrent attempts allow exactly 20; organization isolation, refill and rollback verified")

    def increase(expected, requested):
        return query("-c", f"SELECT gateway_increase_submission_limit('tiers','test-admin',{expected},{requested})")

    with ThreadPoolExecutor(max_workers=8) as pool:
        results = list(pool.map(lambda _: increase(120, 240), range(8)))
    assert results == ["approved"] * 8, results
    assert query("-c", "SELECT count(*) FROM gateway_submission_limit_increases") == "1"
    assert increase(240, 480) == "cooldown"
    assert query("-c", "SELECT concat_ws('|', requests_per_minute, burst, next_self_service_limit, next_self_service_at > now()) FROM gateway_submission_limit('tiers')") == "240|40|480|t"
    assert increase(240, 600) == "invalid_tier"
    for current, target in [(240, 480), (480, 600)]:
        query("-c", "UPDATE gateway_submission_limits SET last_increase_at = now() - interval '25 hours' WHERE account_id = 'tiers'")
        assert increase(current, target) == "approved"
    assert increase(600, 1200) == "invalid_tier"
    assert query("-c", "SELECT count(*) FROM gateway_submission_limit_increases") == "3"
    query("-c", "UPDATE gateway_submission_limits SET operator_locked = true WHERE account_id = 'tiers'")
    assert increase(480, 600) == "operator_managed"
    assert query("-c", "SELECT * FROM gateway_submission_limit('tiers')") == "600|100|||t"
    print("PASS concurrent increase retries audit once; step order, 24-hour cooldown, ceiling and operator lock verified")

    query("-c", """
        INSERT INTO gateway_projects(id, account_id, name) VALUES ('project', 'burst', 'Fixture');
        INSERT INTO gateway_requests(id, account_id, project_id, actor_id, idempotency_digest, intent_digest,
            model, offer_id, provider, quote, adapter, estimated_usd_nanos, dispatch_after)
        VALUES ('old-eligible', 'burst', 'project', 'anonymous', repeat('1',64), repeat('2',64),
            'fixture', 'offer', 'fal', '{}', 'connector_queue', 0, now() - interval '30 seconds'),
            ('scheduled', 'burst', 'project', 'anonymous', repeat('3',64), repeat('4',64),
            'fixture', 'offer', 'fal', '{}', 'connector_queue', 0, now() + interval '30 seconds');
        INSERT INTO gateway_provider_runs(request_id, binding_digest, upstream_model, request_payload)
        VALUES ('old-eligible', repeat('a',64), 'fixture', decode('aa','hex')),
               ('scheduled', repeat('a',64), 'fixture', decode('aa','hex'));
    """)

    bindings = {"local": [], "connector": ["a" * 64], "poll": ["b" * 64], "lookup": ["c" * 64]}

    def sample(role="capacity_test"):
        return json.loads(query(
            "-v", "reserve_connections=10", "-v", f"bindings={json.dumps(bindings)}", "-f", HERE / "signals.sql",
            role=role,
        ))

    measured = sample()
    assert measured["oldest_ready_age_seconds"] >= 30 and measured["eligible_sample"] == 1, measured
    assert measured["additional_connections"] > 0, measured
    assert measured["admissions_paused"] is None
    query("--single-transaction", *(arg for path in migrations[gate_index:] for arg in ("-f", path)))
    assert sample()["admissions_paused"] is False
    query("-c", """
        CREATE ROLE capacity_collector LOGIN;
        GRANT SELECT ON public.gateway_requests, public.gateway_provider_runs, public.gateway_dispatch_controls,
            public.gateway_queue_cooldowns, public.gateway_connector_completions TO capacity_collector;
        UPDATE public.gateway_admission_control SET paused = true;
    """)
    assert sample("capacity_collector")["admissions_paused"] is None
    query("-c", "GRANT SELECT ON public.gateway_admission_control TO capacity_collector")
    observed = sample("capacity_collector")
    assert observed["admissions_paused"] is True and observed["eligible_sample"] == 1, observed
    query("-c", "UPDATE public.gateway_admission_control SET paused = false")
    assert sample("capacity_collector")["admissions_paused"] is False
    assert query("-c", "SELECT has_table_privilege('capacity_collector', 'public.gateway_admission_control', 'UPDATE')") == "f"
    print("PASS full migration chain; the standalone sampler sees pause with read access and preserves queue signals")
    verify_scale_in(pg_bin, directory, query)
    query("-c", "INSERT INTO gateway_queue_cooldowns VALUES ('burst','project',repeat('a',64),'fixture',now() + interval '60 seconds')")
    assert sample()["eligible_sample"] == 0
    query("-c", "DELETE FROM gateway_queue_cooldowns; INSERT INTO gateway_dispatch_controls VALUES ('burst',1,true,'{}')")
    assert sample()["eligible_sample"] == 0
    print("PASS sampler sees eligible work and excludes delayed, cooling-down and paused requests")

    query("-c", """
        INSERT INTO gateway_requests (id, account_id, project_id, actor_id, idempotency_digest, intent_digest,
            model, offer_id, provider, quote, adapter, estimated_usd_nanos, dispatched_at, uncertain_at)
        SELECT id, 'burst', 'project', 'anonymous', md5(id) || md5(id), repeat('e',64),
            'fixture', 'offer', 'fal', '{}', 'connector_queue', 0,
            now() - CASE WHEN id='lookup-expired' THEN interval '16 minutes' ELSE interval '5 minutes' END,
            CASE WHEN id LIKE 'lookup-%' THEN now() - interval '84 seconds' END
        FROM unnest(ARRAY['poll-due','poll-leased','poll-delayed','poll-expired','poll-other',
                          'lookup-due','lookup-leased','lookup-expired','lookup-other']) AS ids(id);

        INSERT INTO gateway_provider_runs (request_id, binding_digest, upstream_model, upstream_handle,
            accepted_at, poll_after, poll_deadline, lease_until, error_code)
        SELECT id, repeat(CASE WHEN id LIKE '%-other' THEN 'd' WHEN id LIKE 'poll-%' THEN 'b' ELSE 'c' END,64),
            'fixture', CASE WHEN id LIKE 'poll-%' THEN id END,
            CASE WHEN id LIKE 'poll-%' THEN now() - interval '3 minutes' END,
            CASE WHEN id='poll-delayed' THEN now() + interval '1 minute'
                 WHEN id LIKE 'poll-%' THEN now() - interval '62 seconds' END,
            CASE WHEN id='poll-expired' THEN now() - interval '1 second'
                 WHEN id LIKE 'poll-%' THEN now() + interval '3 minutes' END,
            CASE WHEN id='poll-leased' THEN now() + interval '1 minute' END,
            CASE WHEN id LIKE 'lookup-%' THEN 'connector_submission_unconfirmed' END
        FROM gateway_requests WHERE id LIKE 'poll-%' OR id LIKE 'lookup-%';

        INSERT INTO gateway_connector_completions (request_id, next_lookup_at)
        VALUES ('lookup-leased', now() + interval '10 seconds');
    """)
    measured = sample()
    assert measured["eligible_sample"] == 2 and 84 <= measured["oldest_ready_age_seconds"] < 95, measured
    print("PASS sampler includes due polls and lookups for retained bindings and excludes leases, delays and other bindings")

    query("-c", "CREATE ROLE capacity_monitor LOGIN")
    query("-v", "monitor_role=capacity_monitor", "-f", HERE.parent / "monitoring/install.sql")
    settings = json.dumps(bindings)
    providers = json.dumps(["fal"])
    query("-c", "UPDATE public.gateway_admission_control SET paused = true")
    whole_cell = json.loads(query("-c", "SET ROLE capacity_monitor; " +
        f"SELECT tokencraft_monitor.sample('{providers}', '[]', '{settings}', 10, NULL, NULL)"))
    assert whole_cell["maintenance"]["paused"] is sample("capacity_collector")["admissions_paused"] is True
    query("-c", "UPDATE public.gateway_admission_control SET paused = false")
    monitored = json.loads(query("-c", "SET ROLE capacity_monitor; " +
        f"SELECT tokencraft_monitor.sample('{providers}', '[]', '{settings}', 10, 'burst', 'project')"))
    lanes = monitored["provider_work"]
    assert monitored["eligible"] == 0 and monitored["oldest_eligible_seconds"] is None, monitored
    assert {name: lane["eligible"] for name, lane in lanes.items()} == {"submit": 0, "poll": 1, "lookup": 1}, lanes
    other_scope = json.loads(query("-c", "SET ROLE capacity_monitor; " +
        f"SELECT tokencraft_monitor.sample('{providers}', '[]', '{settings}', 10, 'second', NULL)"))
    assert all(lane["eligible"] == 0 for lane in other_scope["provider_work"].values())
    try:
        query("-c", f"SET ROLE capacity_monitor; SELECT * FROM public.gateway_provider_work_sample('{settings}')")
    except subprocess.CalledProcessError as error:
        assert "permission denied" in error.stderr, error.stderr
    else:
        raise AssertionError("the invoker routine bypassed table permissions")
    print("PASS monitor and standalone sampler agree on pause; scoped queues remain available")

    query("-c", """
        INSERT INTO gateway_projects(id, account_id, name) VALUES ('overflow', 'second', 'Overflow fixture');
        INSERT INTO gateway_requests (id, account_id, project_id, actor_id, idempotency_digest, intent_digest,
            model, offer_id, provider, quote, adapter, estimated_usd_nanos, dispatched_at, uncertain_at)
        SELECT 'overflow-' || n, 'second', 'overflow', 'anonymous', repeat(md5(n::text), 2), repeat('a',64),
            'fixture', 'offer', 'fal', '{}', 'connector_queue', 0,
            now() - interval '5 minutes', now() - n * interval '1 second'
        FROM generate_series(1, 270) n;
        INSERT INTO gateway_provider_runs (request_id, binding_digest, upstream_model, error_code)
        SELECT id, repeat('c',64), 'fixture', 'submission_interrupted'
        FROM gateway_requests WHERE project_id = 'overflow';
    """)
    capped = sample()
    assert capped["eligible_sample"] == 257 and 270 <= capped["oldest_ready_age_seconds"] < 280, capped
    print("PASS lookup sample caps at 256 without dropping the oldest item or the separate poll lane")

    query("-c", """
        INSERT INTO gateway_provider_workflow_frames (request_id, encrypted_png)
        VALUES ('old-eligible', decode(repeat('a0', 32796), 'hex'));
        UPDATE gateway_provider_runs SET request_payload = decode('bb', 'hex')
        WHERE request_id = 'old-eligible';
    """)
    assert query("-c", "SELECT octet_length(encrypted_png) FROM gateway_provider_workflow_frames") == "32796"
    query("-c", "UPDATE gateway_provider_runs SET request_payload = NULL WHERE request_id = 'old-eligible'")
    assert query("-c", "SELECT count(*) FROM gateway_provider_workflow_frames") == "0"
    print("PASS a large workflow frame survives a payload update and is cleared with the submit payload")


def verify_scale_in(pg_bin, directory, query):
    service = directory / "pg-services"
    service.write_text(f"[writer]\nhost={directory}\nuser=capacity_collector\ndbname=postgres\n")
    log = directory / "scale-requests"
    release = directory / "finish-scale"
    actuator = directory / "kubectl-fixture"
    actuator.write_text(
        f"#!{sys.executable}\n"
        "import json, pathlib, sys, time\n"
        f"with open({str(log)!r}, 'a') as output:\n"
        "    print(json.dumps(sys.argv[1:]), file=output)\n"
        "deadline = time.monotonic() + 8\n"
        f"while not pathlib.Path({str(release)!r}).exists():\n"
        "    if time.monotonic() >= deadline:\n"
        "        raise SystemExit('scale fixture was not released')\n"
        "    time.sleep(.025)\n"
    )
    actuator.chmod(0o700)
    config = {
        "cell": "fixture", "context": "local", "namespace": "disposable",
        "connections_per_replica": 10, "connection_budget": 100,
        "api": {"deployment": "api", "min": 2, "max": 6, "allow_scale_in": True, "admission_service": "writer"},
        "worker": {"deployment": "worker", "min": 2, "max": 6},
    }
    snapshot = {
        "cell": "fixture", "database": {"healthy": True, "additional_connections": 30, "admissions_paused": False},
        "api": {"desired": 3, "ready": 3, "stable": True, "deployment_version": "11", "cpu_fraction": 0.1,
                "sse_readers": 0, "sse_capacity": 96},
        "worker": {"desired": 2, "ready": 2, "stable": True, "deployment_version": "21", "active_tasks": 0,
                   "task_capacity": 8, "oldest_ready_age_seconds": 0, "provider_headroom": True},
    }
    policy_file, sample_file, state_file = (directory / name for name in ("policy.json", "sample.json", "state.json"))
    policy_file.write_text(json.dumps(config))

    def invoke():
        now = time.time()
        sample_file.write_text(json.dumps(dict(snapshot, observed_at=now)))
        state_file.write_text(json.dumps({
            "cell": "fixture", "last_observed_at": now - 15, "admissions_paused": False,
            "roles": {"api": {"desired": 3, "low_since": now - 301}},
        }))
        result = subprocess.run([
            sys.executable, "-B", HERE / "controller.py", "--config", policy_file, "--snapshot", sample_file,
            "--state", state_file, "--apply", "--kubectl", actuator, "--psql", pg_bin / "psql",
        ], env=dict(os.environ, PGSERVICEFILE=str(service)), check=True, capture_output=True, text=True, timeout=14)
        return json.loads(result.stdout)

    def pause():
        query("-c", "BEGIN; SET LOCAL statement_timeout = '5s'; "
              "LOCK TABLE public.gateway_admission_control IN ACCESS EXCLUSIVE MODE; "
              "UPDATE public.gateway_admission_control SET paused = true; COMMIT;")

    with ThreadPoolExecutor(max_workers=2) as tasks:
        applying = tasks.submit(invoke)
        try:
            for _ in range(100):
                if log.is_file() and log.stat().st_size:
                    break
                if applying.done():
                    raise AssertionError(f"Scale request failed before reaching the fixture: {applying.result()}")
                time.sleep(.05)
            else:
                raise AssertionError("Scale request did not reach the fixture")
            pausing = tasks.submit(pause)
            for _ in range(40):
                if query("-c", "SELECT EXISTS (SELECT 1 FROM pg_locks "
                         "WHERE relation = 'public.gateway_admission_control'::regclass "
                         "AND mode = 'AccessExclusiveLock' AND NOT granted)") == "t":
                    break
                time.sleep(.05)
            else:
                raise AssertionError("The pause did not wait for the actuator transaction")
            assert not applying.done() and not pausing.done()
        finally:
            release.write_text("done")
        assert applying.result(timeout=5)["applied"] is True
        pausing.result(timeout=5)

    held = invoke()
    assert held["decision"] == "hold" and held["applied"] is False and "paused" in held["reason"], held
    assert json.loads(state_file.read_bytes())["roles"]["api"]["low_since"] is None
    query("-c", "REVOKE SELECT ON public.gateway_admission_control FROM capacity_collector")
    unknown = invoke()
    assert unknown["decision"] == "hold" and unknown["applied"] is False and "could not verify" in unknown["reason"]
    query("-c", "GRANT SELECT ON public.gateway_admission_control TO capacity_collector")
    calls = [json.loads(line) for line in log.read_text().splitlines()]
    assert len(calls) == 1 and "--resource-version=11" in calls[0] and "--current-replicas=3" in calls[0]
    query("-c", "UPDATE public.gateway_admission_control SET paused = false")
    print("PASS an in-flight scale-in holds the pause, and a paused or unreadable writer vetoes a stale open plan")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pg-bin-dir", type=Path, required=True)
    args = parser.parse_args()
    with TemporaryDirectory(prefix="tokencraft-quota-", dir="/tmp") as folder:
        verify(args.pg_bin_dir.resolve(), Path(folder))
    print("PASS disposable PostgreSQL stopped and removed")


if __name__ == "__main__":
    main()
