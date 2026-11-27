"""Collect TokenCraft replica and PostgreSQL signals into a controller snapshot."""

import argparse
from concurrent.futures import ThreadPoolExecutor
from hashlib import sha256
import ipaddress
import json
import os
import re
from pathlib import Path
import subprocess
import time
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener

from controller import MAX_SAMPLE_AGE, ROLES, cpu_utilization, integer, read_object, save_state


BINDING_KINDS = ("local", "connector", "poll", "lookup")


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None


def scrape(url, token):
    address = urlsplit(url)
    if address.username is not None or address.password is not None or address.fragment or address.query:
        raise ValueError("capacity probe URL cannot contain credentials, query, or fragment")
    if address.path != "/internal/capacity" or not address.hostname:
        raise ValueError("capacity probe URL must target /internal/capacity")
    if address.scheme != "https":
        if address.scheme != "http" or not ipaddress.ip_address(address.hostname).is_loopback:
            raise ValueError("capacity probes require HTTPS or literal loopback HTTP")
    opener = build_opener(NoRedirect(), ProxyHandler({}))
    request = Request(url, headers={"Authorization": f"Bearer {token}"})
    with opener.open(request, timeout=4) as reply:
        data = reply.read(65537)
    if len(data) > 65536:
        raise ValueError("capacity probe response exceeds 64 KiB")
    return json.loads(data)


def query_database(psql, reserve, bindings):
    output = subprocess.run([
        psql, "-XqAtw", "--set=ON_ERROR_STOP=1", f"--set=reserve_connections={reserve}",
        f"--set=bindings={json.dumps(bindings)}",
        "--file", str(Path(__file__).with_name("signals.sql")),
    ], check=True, timeout=6, text=True, capture_output=True)
    return json.loads(output.stdout)


def collect(source, token, psql):
    sampled_at = source["observed_at"]
    if type(sampled_at) not in (int, float) or not 0 <= time.time() - sampled_at <= MAX_SAMPLE_AGE:
        raise ValueError("external database and provider signals are missing a fresh observation timestamp")
    reserve = integer(source["reserve_connections"], "reserve_connections", minimum=10)
    jobs = [(role, url) for role in ROLES for url in source[role]["probe_urls"]]
    if len({url for _, url in jobs}) != len(jobs):
        raise ValueError("supply a distinct direct probe URL for every replica")
    if not 1 <= len(jobs) <= 2000:
        raise ValueError("supply between 1 and 2000 replica probe URLs")
    pool = ThreadPoolExecutor(max_workers=16)
    try:
        probes = list(pool.map(lambda job: scrape(job[1], token), jobs, timeout=8))
    finally:
        pool.shutdown(cancel_futures=True)
    by_role = {role: [] for role in ROLES}
    seen_instances = set()
    for (role, _), probe in zip(jobs, probes):
        if probe["role"] != role:
            raise ValueError(f"expected a split {role} process")
        instance_id = probe.get("instance_id")
        if not isinstance(instance_id, str) or not re.fullmatch(r"[0-9a-f]{32}", instance_id):
            raise ValueError("capacity probes must report a valid instance ID")
        if instance_id in seen_instances:
            raise ValueError("multiple probe URLs reached the same replica process")
        seen_instances.add(instance_id)
        by_role[role].append(probe)

    worker_bindings = set()
    for probe in by_role["worker"]:
        groups = []
        for kind in BINDING_KINDS:
            digests = probe.get(f"{kind}_bindings")
            if (not isinstance(digests, list) or len(digests) > 512
                    or any(not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest)
                           for digest in digests)):
                raise ValueError("workers must report valid binding digests for every provider lane")
            groups.append(tuple(sorted(set(digests))))
        worker_bindings.add(tuple(groups))
    if len(worker_bindings) != 1:
        raise ValueError("worker replicas must report matching binding revisions")
    bindings = dict(zip(BINDING_KINDS, worker_bindings.pop()))
    database = query_database(psql, reserve, bindings)
    snapshot = {
        "cell": source["cell"], "observed_at": sampled_at,
        "database": {
            "healthy": source["database_healthy"] is True,
            "additional_connections": database["additional_connections"],
        },
    }
    for role in ROLES:
        role_source = source[role]
        ready = sum(probe["ready"] is True for probe in by_role[role])
        desired = integer(role_source["desired"], f"{role}.desired", minimum=1)
        instances = sorted(probe["instance_id"] for probe in by_role[role])
        snapshot[role] = {
            "replica_set": sha256("".join(instances).encode("ascii")).hexdigest(),
            "ready": ready, "desired": desired,
            "stable": role_source["stable"] is True and len(by_role[role]) == desired,
            "deployment_version": role_source["deployment_version"],
        }
    snapshot["api"].update(
        cpu_fraction=cpu_utilization(source["api"]["cpu_fraction"]),
        sse_readers=sum(probe["sse_readers"] or 0 for probe in by_role["api"]),
        sse_capacity=sum(probe["sse_capacity"] or 0 for probe in by_role["api"]),
    )
    snapshot["worker"].update(
        active_tasks=sum(probe["provider_tasks"] or 0 for probe in by_role["worker"]),
        task_capacity=sum(probe["provider_capacity"] or 0 for probe in by_role["worker"]),
        oldest_ready_age_seconds=database["oldest_ready_age_seconds"],
        eligible_sample=database["eligible_sample"],
        provider_headroom=source.get("provider_headroom") is True,
    )
    return snapshot


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="Fresh rollout, CPU, database and provider observations")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--psql", default="psql")
    args = parser.parse_args()
    token = os.environ.get("TOKENCRAFT_CAPACITY_TOKEN", "")
    if len(token) != 64 or any(char not in "0123456789abcdefABCDEF" for char in token):
        parser.error("TOKENCRAFT_CAPACITY_TOKEN must contain 64 hex characters")
    save_state(args.output, collect(read_object(args.source), token, args.psql))


if __name__ == "__main__":
    main()
