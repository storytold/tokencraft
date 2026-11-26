"""Persist staged media and recover accepted jobs after a runner restart."""
from __future__ import annotations

from contextlib import closing, contextmanager
import fcntl
from hashlib import file_digest, sha256
import json
import os
from pathlib import Path
import secrets
import shutil
import sqlite3
import time


class Conflict(ValueError):
    pass


class Capacity(ValueError):
    pass


SCHEMA = """
CREATE TABLE IF NOT EXISTS runner_pack (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    name TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS requests (
    id TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('staging','queued','running','succeeded','failed','expired')),
    handle TEXT UNIQUE,
    fingerprint TEXT,
    payload TEXT,
    content_type TEXT,
    result_sha256 TEXT,
    result_size INTEGER
);
CREATE TABLE IF NOT EXISTS inputs (
    request_id TEXT NOT NULL REFERENCES requests(id),
    slot TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    PRIMARY KEY(request_id, slot)
);
CREATE INDEX IF NOT EXISTS queued_requests ON requests(status, created_at);
CREATE INDEX IF NOT EXISTS retained_requests ON requests(status, updated_at);
"""


def sync_directory(directory: Path):
    fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


class Store:
    def __init__(
        self, state: Path, max_requests: int, retention_seconds: int, request_bytes: int,
        *, pack: str = "tokencraft-media",
    ):
        self.state = state.resolve()
        self.max_requests = max_requests
        self.retention_seconds = retention_seconds
        self.request_bytes = request_bytes
        self.state.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.lock = (self.state / ".runner.lock").open("a")
        try:
            fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            self.lock.close()
            raise RuntimeError("another media runner owns this state directory") from None
        self.inputs = self.state / "inputs"
        self.results = self.state / "results"
        self.uploads = self.state / "uploads"
        for directory in (self.inputs, self.results, self.uploads):
            directory.mkdir(mode=0o700, exist_ok=True)
        self.database = self.state / "queue.sqlite3"
        try:
            with closing(self.connect()) as db:
                db.execute("PRAGMA journal_mode=WAL")
                db.executescript(SCHEMA)
                pinned = db.execute("SELECT name FROM runner_pack WHERE id = 1").fetchone()
                if pinned is None:
                    legacy = db.execute("SELECT 1 FROM requests LIMIT 1").fetchone()
                    if legacy and pack != "tokencraft-media":
                        raise RuntimeError("state directory belongs to the original media pack")
                    db.execute("INSERT INTO runner_pack(id,name) VALUES (1,?)", (pack,))
                elif pinned["name"] != pack:
                    raise RuntimeError("state directory belongs to another tool pack or manifest")
                db.execute("UPDATE requests SET status = 'queued' WHERE status = 'running'")
                staged = {entry[0] for entry in db.execute(
                    "SELECT id FROM requests WHERE status != 'expired'",
                )}
                retained = {entry[0] for entry in db.execute(
                    "SELECT handle FROM requests WHERE status IN ('queued','running','succeeded')",
                )}
        except Exception:
            self.lock.close()
            raise
        for entry in self.inputs.iterdir():
            if entry.name not in staged and entry.is_dir() and not entry.is_symlink():
                shutil.rmtree(entry)
        for entry in self.uploads.iterdir():
            if entry.is_dir() and not entry.is_symlink():
                shutil.rmtree(entry)
            else:
                entry.unlink()
        for entry in self.results.iterdir():
            if entry.name not in retained:
                entry.unlink()

    def close(self):
        self.lock.close()

    def connect(self):
        connection = sqlite3.connect(self.database, timeout=10, isolation_level=None)
        connection.row_factory = sqlite3.Row
        connection.execute("PRAGMA foreign_keys=ON")
        return connection

    @contextmanager
    def write(self):
        with closing(self.connect()) as connection:
            connection.execute("BEGIN IMMEDIATE")
            with connection:
                yield connection

    def stage(self, request_id: str, slot: str, digest: str, size: int, upload: Path):
        now = int(time.time())
        with self.write() as db:
            request = db.execute("SELECT status FROM requests WHERE id = ?", (request_id,)).fetchone()
            if request is None:
                live = db.execute("SELECT count(*) FROM requests WHERE status != 'expired'").fetchone()[0]
                if live >= self.max_requests:
                    raise Capacity("media runner request capacity reached")
                db.execute(
                    "INSERT INTO requests(id, created_at, updated_at, status) VALUES (?, ?, ?, 'staging')",
                    (request_id, now, now),
                )
            elif request["status"] == "expired":
                raise Conflict("the request has expired")

            prior = db.execute(
                "SELECT sha256, size_bytes FROM inputs WHERE request_id = ? AND slot = ?",
                (request_id, slot),
            ).fetchone()
            if prior:
                if tuple(prior) != (digest, size):
                    raise Conflict("the input slot already has different content")
                return
            if request and request["status"] != "staging":
                raise Conflict("the accepted request cannot receive another source")
            count, used_bytes = db.execute(
                "SELECT count(*), coalesce(sum(size_bytes), 0) FROM inputs WHERE request_id = ?", (request_id,),
            ).fetchone()
            if count >= 8 or used_bytes + size > self.request_bytes:
                raise Capacity("the request exceeded its staged input capacity")

            folder = self.inputs / request_id
            folder.mkdir(mode=0o700, exist_ok=True)
            os.replace(upload, folder / slot)
            sync_directory(folder)
            sync_directory(self.inputs)
            db.execute("INSERT INTO inputs VALUES (?, ?, ?, ?)", (request_id, slot, digest, size))
            db.execute("UPDATE requests SET updated_at = ? WHERE id = ?", (now, request_id))

    def submit(self, request_id: str, payload: dict) -> str | None:
        canonical = json.dumps(payload, sort_keys=True, separators=(",", ":"), allow_nan=False)
        fingerprint = sha256(canonical.encode()).hexdigest()
        with self.write() as db:
            request = db.execute("SELECT * FROM requests WHERE id = ?", (request_id,)).fetchone()
            if request and request["handle"]:
                if request["fingerprint"] != fingerprint:
                    raise Conflict("the idempotency key already belongs to a different request")
                return request["handle"]
            if not request or request["status"] != "staging":
                return None
            sources = {
                row["slot"]: {"sha256": row["sha256"], "size_bytes": row["size_bytes"]}
                for row in db.execute("SELECT * FROM inputs WHERE request_id = ?", (request_id,))
            }
            if sources != payload["input"]["inputs"]:
                return None
            handle = "job_" + secrets.token_hex(16)
            db.execute(
                """UPDATE requests SET status='queued', handle=?, fingerprint=?, payload=?, updated_at=?
                   WHERE id=?""",
                (handle, fingerprint, canonical, int(time.time()), request_id),
            )
            return handle

    def lookup(self, value: str, *, by_id: bool = False):
        statement = "SELECT * FROM requests WHERE id = ?" if by_id else "SELECT * FROM requests WHERE handle = ?"
        with closing(self.connect()) as db:
            return db.execute(statement, (value,)).fetchone()

    def claim(self):
        with self.write() as db:
            row = db.execute(
                "SELECT * FROM requests WHERE status='queued' ORDER BY created_at, rowid LIMIT 1",
            ).fetchone()
            if row:
                db.execute("UPDATE requests SET status='running' WHERE id=?", (row["id"],))
            return row

    def finish(self, row, *, content_type: str | None = None):
        path = self.results / row["handle"]
        digest, size = None, None
        if content_type:
            with path.open("rb") as data:
                os.fsync(data.fileno())
                digest = file_digest(data, "sha256").hexdigest()
                size = os.fstat(data.fileno()).st_size
            sync_directory(self.results)
        with self.write() as db:
            db.execute(
                """UPDATE requests SET status = ?, result_sha256 = ?, result_size = ?, content_type = ?,
                                       updated_at = ?
                   WHERE id = ? AND status = 'running'""",
                ("succeeded" if content_type else "failed", digest, size, content_type,
                 int(time.time()), row["id"]),
            )

    def expire(self):
        deadline = int(time.time()) - self.retention_seconds
        with self.write() as db:
            rows = db.execute(
                """SELECT id, handle FROM requests
                   WHERE status IN ('staging','succeeded','failed') AND updated_at < ?""",
                (deadline,),
            ).fetchall()
            for row in rows:
                db.execute("UPDATE requests SET status='expired', payload=NULL WHERE id=?", (row["id"],))
                db.execute("DELETE FROM inputs WHERE request_id=?", (row["id"],))
        for row in rows:
            shutil.rmtree(self.inputs / row["id"], ignore_errors=True)
            if row["handle"]:
                (self.results / row["handle"]).unlink(missing_ok=True)
