"""Pull a TokenCraft export into a customer-owned PostgreSQL database."""

import argparse
from datetime import datetime
import hashlib
import hmac
import http.client
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


class SyncError(Exception):
    def __init__(self, message, *, retryable=False):
        super().__init__(message)
        self.retryable = retryable


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


EVENT_ID = re.compile(r"evt_[a-f0-9]{32}")
PAGE_BYTES = 2 * 1024 * 1024
SCHEMA_VERSION = 1
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())


def settings(*, require_token=True):
    base = os.environ.get("TOKENCRAFT_EXPORT_API_URL", "http://localhost:4000").rstrip("/")
    try:
        origin = urllib.parse.urlsplit(base)
        if not origin.hostname or origin.username or origin.password or origin.query or origin.fragment:
            raise ValueError
        if origin.scheme != "https" and not (
            origin.scheme == "http" and origin.hostname in ("localhost", "127.0.0.1", "::1")
        ):
            raise ValueError
    except ValueError:
        raise SyncError("Set the gateway URL to HTTPS or a loopback HTTP address.") from None
    project = os.environ.get("TOKENCRAFT_EXPORT_PROJECT", "")
    token = os.environ.get("TOKENCRAFT_EXPORT_API_KEY", "")
    client = os.environ.get("TOKENCRAFT_EXPORT_CLIENT_REF", "")
    if not re.fullmatch(r"proj_[A-Za-z0-9]{26}", project) or (require_token and not token.startswith("tc_project_")):
        raise SyncError("Set the destination project ID and an export-scoped TokenCraft project key.")
    if client and not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,63}", client):
        raise SyncError("Set a valid opaque client reference.")
    if not os.environ.get("PGHOST") or not os.environ.get("PGDATABASE"):
        raise SyncError("Set PGHOST and PGDATABASE for the customer-owned destination.")
    return base, token, {"source": base, "project": project, "client": client}


def gateway(base, token, path, body=None):
    headers = {"Authorization": f"Bearer {token}"}
    encoded = None
    if body is not None:
        encoded = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(base + path, data=encoded, headers=headers)
    try:
        with OPENER.open(request, timeout=20) as response:
            expected_size = response.length
            if expected_size is not None and expected_size > PAGE_BYTES:
                raise SyncError("Gateway returned an oversized export page.")
            payload = response.read(PAGE_BYTES + 1)
            if len(payload) > PAGE_BYTES:
                raise SyncError("Gateway returned an oversized export page.")
            # A bounded HTTPResponse read can return a short body without raising IncompleteRead.
            if expected_size is not None and len(payload) < expected_size:
                raise SyncError("The gateway interrupted the export response.", retryable=True)
            return payload, response.headers
    except urllib.error.HTTPError as error:
        with error:
            if error.code == 409 and is_cold_archive(error):
                raise SyncError(
                    "The next page needs an archived object. Ask a project administrator to restore it, "
                    "then let the sync resume from its checkpoint.", retryable=True
                ) from None
        raise SyncError(
            f"Gateway returned HTTP {error.code}.",
            retryable=error.code in (408, 429) or error.code >= 500,
        ) from None
    except (OSError, http.client.HTTPException):
        raise SyncError("Unable to read the gateway.", retryable=True) from None


def is_cold_archive(error):
    try:
        envelope = json.loads(error.read(4096))
        return envelope["error"]["code"] == "archive_retrieval_required"
    except (ValueError, KeyError, TypeError, OSError, http.client.HTTPException):
        return False


def postgres(sql, bindings):
    command = ["psql", "-X", "-w", "-qAt", "-v", "ON_ERROR_STOP=1"]
    for key, value in bindings.items():
        command.extend(["-v", f"{key}={value}"])
    pg_env = {key: value for key, value in os.environ.items() if not key.startswith("TOKENCRAFT_EXPORT_")}
    pg_env.setdefault("PGCONNECT_TIMEOUT", "10")
    try:
        result = subprocess.run(
            command, input=sql, text=True, capture_output=True, check=False, env=pg_env, timeout=60,
        )
    except subprocess.TimeoutExpired:
        raise SyncError("The PostgreSQL operation timed out.", retryable=True) from None
    except FileNotFoundError:
        raise SyncError("Install the psql client to sync to PostgreSQL.") from None
    if result.returncode:
        raise SyncError("PostgreSQL could not complete the operation. Check the destination connection and permissions.",
                        retryable=True)
    return result.stdout.strip()


SCOPE = "source_url = :'source' AND project_id = :'project' AND client_ref = :'client'"
CHECKPOINT = f"SELECT after_event_id FROM tokencraft_export.checkpoints WHERE {SCOPE};"

REGISTER = """
INSERT INTO tokencraft_export.checkpoints (source_url, project_id, client_ref)
VALUES (:'source', :'project', :'client') ON CONFLICT DO NOTHING;
""" + CHECKPOINT

INITIALIZE = """
BEGIN;
SELECT pg_advisory_xact_lock(761293, 2);
CREATE SCHEMA IF NOT EXISTS tokencraft_export;
CREATE TABLE IF NOT EXISTS tokencraft_export.events (
    source_url text NOT NULL,
    project_id text NOT NULL,
    event_id text NOT NULL,
    payload jsonb NOT NULL,
    PRIMARY KEY (source_url, project_id, event_id)
);
CREATE TABLE IF NOT EXISTS tokencraft_export.checkpoints (
    source_url text NOT NULL,
    project_id text NOT NULL,
    client_ref text NOT NULL,
    after_event_id text,
    PRIMARY KEY (source_url, project_id, client_ref)
);
ALTER TABLE tokencraft_export.checkpoints
    ADD COLUMN IF NOT EXISTS feed_as_of timestamptz,
    ADD COLUMN IF NOT EXISTS caught_up_as_of timestamptz,
    ADD COLUMN IF NOT EXISTS has_more boolean,
    ADD COLUMN IF NOT EXISTS publication_pending boolean;
COMMIT;
""" + REGISTER

STATUS = f"""
SELECT json_build_object(
    'after_event_id', after_event_id,
    'feed_as_of', feed_as_of,
    'caught_up_as_of', caught_up_as_of,
    'has_more', has_more,
    'publication_pending', publication_pending
) FROM tokencraft_export.checkpoints WHERE {SCOPE};
"""

ADVANCE = f"""
WITH checkpoint AS (
    UPDATE tokencraft_export.checkpoints
    SET after_event_id = nullif(:'next', ''),
        feed_as_of = greatest(feed_as_of, observed.as_of),
        caught_up_as_of = CASE WHEN NOT observed.more AND NOT observed.pending
                          THEN greatest(caught_up_as_of, observed.as_of) ELSE caught_up_as_of END,
        has_more = CASE WHEN feed_as_of IS NULL OR observed.as_of >= feed_as_of
                   THEN coalesce(observed.more, has_more) ELSE has_more END,
        publication_pending = CASE WHEN feed_as_of IS NULL OR observed.as_of >= feed_as_of
                              THEN coalesce(observed.pending, publication_pending)
                              ELSE publication_pending END
    FROM (SELECT nullif(:'as_of', '')::timestamptz AS as_of,
                 nullif(:'more', '')::boolean AS more,
                 nullif(:'pending', '')::boolean AS pending) AS observed
    WHERE {SCOPE} AND after_event_id IS NOT DISTINCT FROM nullif(:'after', '')
    RETURNING source_url, project_id
)
"""

INSERT_ROWS = r"""
, applied AS (
    INSERT INTO tokencraft_export.events (source_url, project_id, event_id, payload)
    SELECT source_url, project_id, payload->>'id', payload
    FROM checkpoint CROSS JOIN export_page
    ON CONFLICT (source_url, project_id, event_id) DO NOTHING RETURNING event_id
)
SELECT CASE WHEN EXISTS (SELECT 1 FROM checkpoint) THEN count(*)::text ELSE 'checkpoint_changed' END AS page_result,
       EXISTS (SELECT 1 FROM checkpoint) AND count(*) < (SELECT count(*) FROM export_page) AS compare_replays
FROM applied \gset
"""

# A separate statement can see an event committed by another scope while INSERT waited.
FINISH_PAGE = r"""
\set event_conflict false
\if :compare_replays
SELECT EXISTS (
    SELECT 1 FROM export_page incoming
    LEFT JOIN tokencraft_export.events stored
      ON stored.source_url = :'source' AND stored.project_id = :'project'
     AND stored.event_id = incoming.payload->>'id'
    WHERE stored.payload IS DISTINCT FROM incoming.payload
) AS event_conflict \gset
\endif
\if :event_conflict
ROLLBACK;
\echo event_conflict
\else
COMMIT;
\echo :page_result
\endif
"""


def import_page(records, binding, after, next_after, *, as_of="", more="", pending=""):
    if records:
        # COPY escapes backslashes. JSON escapes tabs and line breaks within fields.
        rows = "\n".join(json.dumps(record, separators=(",", ":")).replace("\\", "\\\\") for record in records)
        command = """
BEGIN;
CREATE TEMP TABLE export_page (payload jsonb) ON COMMIT DROP;
COPY export_page FROM STDIN;
""" + rows + "\n\\.\n" + ADVANCE + INSERT_ROWS + FINISH_PAGE
    else:
        command = ADVANCE + "SELECT CASE WHEN EXISTS (SELECT 1 FROM checkpoint) THEN '0' ELSE 'checkpoint_changed' END;"
    result = postgres(command, {**binding, "after": after, "next": next_after,
                                "as_of": as_of, "more": more, "pending": pending})
    if result == "checkpoint_changed":
        raise SyncError("Another sync advanced this checkpoint. Resuming from the current cursor.", retryable=True)
    if result == "event_conflict":
        raise SyncError("An export event ID already has different content in this destination. The page was rolled back.")
    return int(result)


def load_checkpoint(binding, statement=CHECKPOINT):
    checkpoint = postgres(statement, binding)
    if checkpoint and not EVENT_ID.fullmatch(checkpoint):
        raise SyncError("The stored checkpoint is invalid.")
    return checkpoint


def check_version(version):
    if type(version) is not int or version != SCHEMA_VERSION:
        label = str(version) if type(version) is int and 0 <= version < 65536 else "unknown"
        raise SyncError(f"Unsupported export schema version {label}. This importer supports {SCHEMA_VERSION}.")


def read_page(payload, headers, binding, checkpoint):
    checksum = headers.get("X-TokenCraft-Content-Sha256", "")
    if not hmac.compare_digest(hashlib.sha256(payload).hexdigest(), checksum):
        raise SyncError("Export page checksum does not match.")
    records = [json.loads(line) for line in payload.splitlines()]
    next_after = headers.get("X-TokenCraft-Next-After")
    more = headers.get("X-TokenCraft-Has-More")
    if more not in ("true", "false") or (not records and (more == "true" or next_after)):
        raise SyncError("Gateway returned inconsistent page metadata.")
    if records:
        ids = set()
        for event in records:
            if not isinstance(event, dict):
                raise SyncError("An export event must be a JSON object.")
            check_version(event.get("schema_version"))
            event_id = event.get("id")
            if (
                event.get("project_id") != binding["project"]
                or (binding["client"] and event.get("client_ref") != binding["client"])
                or not isinstance(event_id, str) or not EVENT_ID.fullmatch(event_id)
            ):
                raise SyncError("An event is outside the configured scope or has an invalid ID.")
            if event_id in ids:
                raise SyncError("The export page repeats an event ID.")
            ids.add(event_id)
        if next_after != records[-1]["id"] or checkpoint in ids:
            raise SyncError("The export cursor did not advance.")
    return records, next_after or checkpoint, more


def sync(setup):
    base, token, binding = settings()
    checkpoint = load_checkpoint(binding, setup)
    payload, _ = gateway(base, token, "/v1/exports", {
        "project_id": binding["project"], "client_ref": binding["client"] or None, "after": checkpoint or None,
    })
    snapshot = json.loads(payload)
    check_version(snapshot.get("schema_version"))
    if (
        snapshot.get("format") != "ndjson"
        or snapshot.get("project_id") != binding["project"] or (snapshot.get("client_ref") or "") != binding["client"]
        or snapshot.get("after_event_id") != (checkpoint or None)
    ):
        raise SyncError("The export scope or format differs from the configured destination.")
    export_id = snapshot.get("id", "")
    if not re.fullmatch(r"exp_[A-Za-z0-9]{26}", export_id):
        raise SyncError("Gateway returned an invalid export ID.")
    imported = 0
    while True:
        after = urllib.parse.urlencode({"after": checkpoint}) if checkpoint else ""
        payload, headers = gateway(base, token, f"/v1/exports/{export_id}/data" + (f"?{after}" if after else ""))
        records, next_after, more = read_page(payload, headers, binding, checkpoint)
        if records:
            imported += import_page(records, binding, checkpoint, next_after)
            checkpoint = next_after
        if more == "false":
            return imported, snapshot["pending_at_creation"]


def poll_page(base, token, binding, checkpoint):
    query = {"project_id": binding["project"]}
    if binding["client"]:
        query["client_ref"] = binding["client"]
    if checkpoint:
        query["after"] = checkpoint
    payload, headers = gateway(base, token, "/v1/exports/events?" + urllib.parse.urlencode(query))
    version = headers.get("X-TokenCraft-Schema-Version", "")
    check_version(int(version) if re.fullmatch(r"[0-9]{1,5}", version) else None)
    if (headers.get("X-TokenCraft-Project") != binding["project"]
            or headers.get("X-TokenCraft-Client-Ref") != binding["client"]):
        raise SyncError("The event feed scope differs from the configured destination.")
    pending = headers.get("X-TokenCraft-Publication-Pending")
    as_of = headers.get("X-TokenCraft-Feed-As-Of", "")
    if pending not in ("true", "false") or not datetime.fromisoformat(as_of).tzinfo:
        raise SyncError("Gateway returned invalid feed status.")
    records, next_after, more = read_page(payload, headers, binding, checkpoint)
    count = import_page(records, binding, checkpoint, next_after, as_of=as_of, more=more, pending=pending)
    return count, next_after, more == "true", pending == "true"


def watch(interval, setup):
    base, token, binding = settings()
    initialized = False
    checkpoint = None
    delay = interval
    previous_status = None
    while True:
        try:
            if checkpoint is None:
                checkpoint = load_checkpoint(binding, CHECKPOINT if initialized else setup)
                initialized = True
            count, checkpoint, more, pending = poll_page(base, token, binding, checkpoint)
            status = (more, pending)
            if count or status != previous_status:
                print(json.dumps({
                    "imported": count, "after_event_id": checkpoint or None,
                    "has_more": more, "publication_pending": pending,
                }), flush=True)
            previous_status = status
            delay = interval
            # Keep draining published work while bounding each HTTP and database operation.
            time.sleep(1 if more else interval)
        except SyncError as error:
            if not error.retryable:
                raise
            print(str(error), file=sys.stderr, flush=True)
            checkpoint = None
            previous_status = None
            time.sleep(delay)
            delay = min(delay * 2, max(interval, 300))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--watch", action="store_true", help="Continuously pull published events.")
    modes.add_argument("--status", action="store_true", help="Read the saved destination checkpoint as JSON.")
    parser.add_argument("--interval", type=int, default=30, help="Watch interval in seconds when caught up.")
    parser.add_argument("--no-init", action="store_true", help="Use tables already prepared by a previous run.")
    args = parser.parse_args()
    if not 1 <= args.interval <= 3600:
        parser.error("--interval must be between 1 and 3600 seconds")
    setup = REGISTER if args.no_init else INITIALIZE
    if args.status:
        _, _, binding = settings(require_token=False)
        print(postgres(STATUS, binding) or "null")
    elif args.watch:
        watch(args.interval, setup)
    else:
        count, pending = sync(setup)
        print(f"Imported {count} new events." + (" More events were pending at snapshot creation." if pending else ""))


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
    except (SyncError, ValueError, KeyError, TypeError, AttributeError) as error:
        raise SystemExit(str(error) if isinstance(error, SyncError) else "Gateway returned an invalid export document.") from None
