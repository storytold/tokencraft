"""Persistent samples and incident transitions for one operator collector."""

import json
import sqlite3
from contextlib import closing
from pathlib import Path

HISTORY_FIELDS = {
    "probe_latency": ("targets", "latency_ms"),
    "gpu_utilization": ("targets", "signals", "gpu_utilization_percent"),
    "request_p95": ("cells", "sample", "provider", "p95_total_seconds"),
    "target_p95": ("cells", "sample", "compute", "p95_response_headers_seconds"),
    "queue_age": ("cells", "sample", "oldest_eligible_seconds"),
    "workflow_p95": ("cells", "sample", "workflow_outcomes", "p95_succeeded_seconds"),
    "plugin_p95": ("targets", "plugin", "operations", "p95_ms"),
    "plugin_throughput": ("targets", "plugin", "operations", "responses_per_second"),
}


class History:
    def __init__(self, path, retention_seconds=None):
        self.path = Path(path)
        self.retention_seconds = retention_seconds
        if retention_seconds is None:
            return
        with closing(self.connect()) as db:
            db.execute("PRAGMA journal_mode=WAL")
            db.executescript("""
                CREATE TABLE IF NOT EXISTS samples (
                    observed_at INTEGER PRIMARY KEY,
                    data TEXT NOT NULL CHECK (json_valid(data))
                );
                CREATE TABLE IF NOT EXISTS alert_state (
                    key TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    consecutive_bad INTEGER NOT NULL,
                    consecutive_good INTEGER NOT NULL,
                    last_observed INTEGER NOT NULL,
                    opened_at INTEGER
                );
                CREATE TABLE IF NOT EXISTS incidents (
                    key TEXT NOT NULL,
                    title TEXT NOT NULL,
                    opened_at INTEGER NOT NULL,
                    resolved_at INTEGER,
                    PRIMARY KEY (key, opened_at)
                );
                CREATE INDEX IF NOT EXISTS incidents_history ON incidents (opened_at DESC);
            """)

    def connect(self):
        if self.retention_seconds is None:
            db = sqlite3.connect(self.path.absolute().as_uri() + "?mode=ro", uri=True, timeout=3)
        else:
            db = sqlite3.connect(self.path, timeout=3)
        db.row_factory = sqlite3.Row
        return db

    def record(self, snapshot, evaluations, threshold, interval):
        at = snapshot["observed_at"]
        with closing(self.connect()) as db, db:
            latest = db.execute("SELECT max(observed_at) FROM samples").fetchone()[0]
            if latest is not None and at <= latest:
                raise ValueError("collector clock must advance")
            db.execute("INSERT INTO samples VALUES (?, ?)", (at, json.dumps(snapshot, allow_nan=False)))
            previous = {row["key"]: row for row in db.execute("SELECT * FROM alert_state")}
            for key, (title, unhealthy) in evaluations.items():
                last = previous.pop(key, None)
                contiguous = last is not None and at - last["last_observed"] <= interval * 3
                bad = last["consecutive_bad"] if contiguous else 0
                good = last["consecutive_good"] if contiguous else 0
                opened = last["opened_at"] if last else None
                bad = bad + 1 if unhealthy is True else 0
                good = good + 1 if unhealthy is False else 0
                if opened is None and bad >= threshold:
                    opened = at
                    db.execute("INSERT INTO incidents VALUES (?, ?, ?, NULL)", (key, title, at))
                if opened is not None and good >= 2:
                    db.execute("UPDATE incidents SET resolved_at = ? WHERE key = ? AND opened_at = ?", (at, key, opened))
                    opened = None
                db.execute("INSERT OR REPLACE INTO alert_state VALUES (?, ?, ?, ?, ?, ?)",
                           (key, title, bad, good, at, opened))
            for key, last in previous.items():
                # Configuration removal retires the rule and closes any open incident.
                if last["opened_at"] is not None:
                    db.execute("UPDATE incidents SET resolved_at = ? WHERE key = ? AND opened_at = ?",
                               (at, key, last["opened_at"]))
                db.execute("DELETE FROM alert_state WHERE key = ?", (key,))
            cutoff = at - self.retention_seconds
            db.execute("DELETE FROM samples WHERE observed_at < ?", (cutoff,))
            db.execute("DELETE FROM incidents WHERE resolved_at < ?", (cutoff,))

    def overview(self):
        with closing(self.connect()) as db, db:
            db.execute("BEGIN")
            row = db.execute("SELECT data FROM samples ORDER BY observed_at DESC LIMIT 1").fetchone()
            incidents = db.execute("""
                SELECT key, title, opened_at, resolved_at FROM incidents
                ORDER BY resolved_at IS NOT NULL, opened_at DESC LIMIT 200
            """).fetchall()
        return json.loads(row["data"]) if row else None, [dict(item) for item in incidents]

    def active_incidents(self):
        with closing(self.connect()) as db:
            return [dict(row) for row in db.execute("SELECT * FROM incidents WHERE resolved_at IS NULL")]

    def series(self, metric, since, bucket_seconds, identifier, subject=None, scope=None):
        root, *parts = HISTORY_FIELDS[metric]
        if metric in {"request_p95", "target_p95", "plugin_p95", "plugin_throughput"}:
            parts.insert(2, subject)
        scope_parts = ("scopes", scope) if scope else ()
        path = "$." + ".".join(json.dumps(part) for part in (root, identifier, *scope_parts, *parts))
        state_path = "$.targets." + json.dumps(identifier) + ".status"
        with closing(self.connect()) as db:
            rows = db.execute("""
                WITH selected AS (
                    SELECT max(observed_at) AS observed_at FROM samples
                    WHERE observed_at >= ? GROUP BY (observed_at - ?) / ?
                    ORDER BY observed_at DESC LIMIT 720
                )
                SELECT s.observed_at, json_extract(s.data, ?) AS value,
                       json_extract(s.data, ?) AS status
                FROM selected JOIN samples s USING (observed_at) ORDER BY s.observed_at
            """, (since, since, bucket_seconds, path, state_path)).fetchall()
            summary = None
            if root == "targets":
                summary = dict(db.execute("""
                    WITH states AS (
                        SELECT json_extract(data, ?) AS status FROM samples WHERE observed_at >= ?
                    )
                    SELECT count(*) FILTER (WHERE status = 'up') AS up,
                           count(*) FILTER (WHERE status = 'degraded') AS degraded,
                           count(*) FILTER (WHERE status = 'down') AS down,
                           count(*) FILTER (WHERE status IS NULL OR status = 'unknown') AS unknown
                    FROM states
                """, (state_path, since)).fetchone())
        return {"points": [dict(row) for row in rows], "availability": summary}
