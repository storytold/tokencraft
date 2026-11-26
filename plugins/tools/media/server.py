"""Opt-in HTTP media worker. The gateway is the only trusted client."""
from __future__ import annotations

import argparse
from hashlib import file_digest, sha256
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import logging
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import sqlite3
import tempfile
from threading import BoundedSemaphore, Event, Thread
import time

from contract import MANIFEST, MediaError, validate_request
from engine import Engine, Limits, run_command
from queue_store import Capacity, Conflict, Store


IDENTIFIER = re.compile(r"[A-Za-z0-9_-]{1,128}")
SLOT = re.compile(r"[a-z][a-z0-9_]{0,31}")
SHA256 = re.compile(r"[a-f0-9]{64}")
REQUEST_LIMIT = 16 * 1024
CHUNK_SIZE = 64 * 1024


def valid(pattern: re.Pattern, field: object) -> bool:
    return isinstance(field, str) and pattern.fullmatch(field) is not None


def json_object(pairs: list[tuple]) -> dict:
    data = {}
    for key, value in pairs:
        if key in data:
            raise MediaError("duplicate JSON field")
        data[key] = value
    return data


def validate_submission(data: object, key: str, limit: int) -> str:
    if not isinstance(data, dict) or data.keys() != {"version", "request_id", "model", "input"}:
        raise MediaError("expected version, request_id, model, and input")
    if type(data["version"]) is not int or data["version"] != 1:
        raise MediaError("unsupported protocol version")
    request_id = data["request_id"]
    if not valid(IDENTIFIER, request_id) or request_id != key:
        raise MediaError("Idempotency-Key must match request_id")
    tool = data["input"]
    if not isinstance(tool, dict) or tool.keys() != {"kind", "revision", "inputs", "parameters"} or tool["kind"] != "tool":
        raise MediaError("invalid tool input")
    validate_request(data["model"], tool["revision"], tool["inputs"], tool["parameters"])
    for metadata in tool["inputs"].values():
        if (
            not isinstance(metadata, dict) or metadata.keys() != {"sha256", "size_bytes"}
            or not valid(SHA256, metadata["sha256"]) or type(metadata["size_bytes"]) is not int
            or not 0 < metadata["size_bytes"] <= limit
        ):
            raise MediaError("invalid staged input metadata")
    return request_id


class Worker:
    def __init__(self, engine: Engine, store: Store):
        self.engine = engine
        self.store = store
        self.stop = Event()
        self.wakeup = Event()
        self.thread = Thread(target=self.run, name="media-queue", daemon=True)

    def run(self):
        next_expiry = 0
        try:
            while not self.stop.is_set():
                if time.monotonic() >= next_expiry:
                    self.store.expire()
                    next_expiry = time.monotonic() + 30
                job = self.store.claim()
                if job is None:
                    self.wakeup.wait(0.5)
                    self.wakeup.clear()
                else:
                    self.execute(job)
        except (OSError, sqlite3.Error):
            # Leave the claimed job recoverable and let the readiness probe report failure.
            self.stop.set()

    def execute(self, job):
        payload = json.loads(job["payload"])
        work = payload["input"]
        paths = {}
        try:
            for slot, metadata in work["inputs"].items():
                path = self.store.inputs / job["id"] / slot
                with path.open("rb") as data:
                    if (os.fstat(data.fileno()).st_size != metadata["size_bytes"]
                            or file_digest(data, "sha256").hexdigest() != metadata["sha256"]):
                        raise MediaError("staged input failed integrity verification")
                paths[slot] = path
            output = self.store.results / job["handle"]
            content_type = self.engine.execute(payload["model"], work["revision"], paths, work["parameters"], output)
        except (MediaError, OSError):
            self.store.finish(job)
        else:
            self.store.finish(job, content_type=content_type)


class Server(ThreadingHTTPServer):
    def __init__(self, address, store: Store, worker: Worker, token: str):
        self.store = store
        self.worker = worker
        self.authorization = ("Bearer " + token).encode("ascii")
        self.threads = BoundedSemaphore(16)
        super().__init__(address, Handler)

    def process_request(self, request, client_address):
        if not self.threads.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except Exception:
            self.threads.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.threads.release()

    def handle_error(self, _request, _client_address):
        logging.error("media runner HTTP handler failed")


class Handler(BaseHTTPRequestHandler):
    def setup(self):
        self.request.settimeout(12)
        super().setup()

    def log_message(self, _format, *_args):
        # Request paths may contain identifiers. Keep credentials and job data out of logs.
        pass

    def reply(self, status: int, payload: dict | None = None, headers: dict | None = None):
        body = json.dumps(payload, separators=(",", ":")).encode() if payload is not None else b""
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        if payload is not None:
            self.send_header("Content-Type", "application/json")
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.end_headers()
        if body:
            self.wfile.write(body)

    def authenticated(self) -> bool:
        supplied = self.headers.get("Authorization", "").encode("latin-1")
        if not secrets.compare_digest(supplied, self.server.authorization):
            self.reply(401, {"error": "service bearer required"})
            return False
        return True

    def length(self, maximum: int) -> int:
        values = self.headers.get_all("Content-Length", [])
        if len(values) != 1 or self.headers.get("Transfer-Encoding"):
            raise MediaError("exact Content-Length required")
        length = values[0]
        if not re.fullmatch(r"[0-9]{1,9}", length) or not 0 < int(length) <= maximum:
            raise MediaError("body exceeds the operation size limit")
        return int(length)

    def do_GET(self):
        if self.path == "/ready":
            ready = self.server.worker.thread.is_alive() and not self.server.worker.stop.is_set()
            self.reply(200 if ready else 503, {"ready": ready})
            return
        if not self.authenticated():
            return
        if self.path == "/v1/tools":
            self.reply(200, MANIFEST)
            return
        parts = self.path.split("/")
        if len(parts) == 5 and parts[1:4] == ["v1", "jobs", "by-request"] and valid(IDENTIFIER, parts[4]):
            row = self.server.store.lookup(parts[4], by_id=True)
            if row and row["handle"] and row["status"] != "expired":
                self.reply(200, {"version": 1, "request_id": row["id"], "handle": row["handle"]})
                return
        if len(parts) >= 4 and parts[1:3] == ["v1", "jobs"] and valid(IDENTIFIER, parts[3]):
            row = self.server.store.lookup(parts[3])
            if row and row["status"] != "expired":
                if len(parts) == 4:
                    self.reply(200, {"version": 1, "handle": row["handle"], "status": row["status"]})
                    return
                if parts[4:] == ["artifacts", "0"] and row["status"] == "succeeded":
                    self.download(row)
                    return
        self.reply(404, {"error": "not found"})

    def download(self, row):
        source = self.server.store.results / row["handle"]
        try:
            file = source.open("rb")
        except FileNotFoundError:
            self.reply(404, {"error": "artifact unavailable"})
            return
        with file:
            if os.fstat(file.fileno()).st_size != row["result_size"]:
                self.reply(503, {"error": "artifact unavailable"})
                return
            self.send_response(200)
            self.send_header("Content-Type", row["content_type"])
            self.send_header("Content-Length", str(row["result_size"]))
            self.send_header("X-TokenCraft-Content-Sha256", row["result_sha256"])
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            shutil.copyfileobj(file, self.wfile, CHUNK_SIZE)

    def do_PUT(self):
        if not self.authenticated():
            return
        parts = self.path.split("/")
        if (len(parts) != 6 or parts[1:3] != ["v1", "jobs"] or parts[4] != "inputs"
                or not valid(IDENTIFIER, parts[3]) or not valid(SLOT, parts[5])):
            self.reply(404, {"error": "not found"})
            return
        try:
            length = self.length(self.server.worker.engine.limits.input_bytes)
            digest = self.headers.get("X-TokenCraft-Content-Sha256")
            if not valid(SHA256, digest):
                raise MediaError("valid input digest required")
            self.stage(parts[3], parts[5], digest, length)
        except Capacity as error:
            self.reply(429, {"error": str(error)}, {"Retry-After": "1"})
        except Conflict as error:
            self.reply(409, {"error": str(error)})
        except MediaError as error:
            self.reply(400, {"error": str(error)})
        else:
            self.reply(204)

    def stage(self, request_id: str, slot: str, digest: str, length: int):
        store = self.server.store
        with tempfile.TemporaryDirectory(dir=store.uploads) as directory:
            upload = Path(directory) / "input"
            calculated = sha256()
            remaining = length
            deadline = time.monotonic() + 90
            with upload.open("wb") as target:
                while remaining:
                    if time.monotonic() > deadline:
                        raise MediaError("input upload exceeded its deadline")
                    part = self.rfile.read(min(remaining, CHUNK_SIZE))
                    if not part:
                        raise MediaError("incomplete input upload")
                    target.write(part)
                    calculated.update(part)
                    remaining -= len(part)
                target.flush()
                os.fsync(target.fileno())
            if not secrets.compare_digest(calculated.hexdigest(), digest):
                raise MediaError("input digest mismatch")
            store.stage(request_id, slot, digest, length, upload)

    def do_POST(self):
        if not self.authenticated():
            return
        if self.path != "/v1/jobs":
            self.reply(404, {"error": "not found"})
            return
        try:
            if self.headers.get_content_type() != "application/json":
                raise MediaError("application/json required")
            length = self.length(REQUEST_LIMIT)
            raw = self.rfile.read(length)
            if len(raw) != length:
                raise MediaError("incomplete job submission")
            data = json.loads(raw, object_pairs_hook=json_object)
            request_id = validate_submission(
                data, self.headers.get("Idempotency-Key"), self.server.worker.engine.limits.input_bytes,
            )
            handle = self.server.store.submit(request_id, data)
        except Conflict as error:
            self.reply(409, {"error": str(error)})
            return
        except (MediaError, ValueError, RecursionError):
            self.reply(400, {"error": "invalid media tool submission"})
            return
        if handle:
            self.server.worker.wakeup.set()
            outcome = {"status": "accepted", "handle": handle}
        else:
            outcome = {"status": "rejected"}
        self.reply(200, {"version": 1, "request_id": request_id, "outcome": outcome})


def positive(value: str) -> int:
    number = int(value)
    if number <= 0:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return number


def main():
    parser = argparse.ArgumentParser(description="TokenCraft optional media tool runner")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=positive, default=8094)
    parser.add_argument("--state", required=True, type=Path)
    parser.add_argument("--ffmpeg", default="ffmpeg")
    parser.add_argument("--ffprobe", default="ffprobe")
    parser.add_argument("--max-file-mib", type=positive, default=64)
    parser.add_argument("--max-requests", type=positive, default=32)
    parser.add_argument("--retention-hours", type=positive, default=24)
    parser.add_argument("--timeout-seconds", type=positive, default=90)
    args = parser.parse_args()
    token = os.environ.get("TOKENCRAFT_MEDIA_SERVICE_TOKEN", "")
    if not token.isascii() or any(char.isspace() for char in token) or not 32 <= len(token) <= 512:
        parser.error("TOKENCRAFT_MEDIA_SERVICE_TOKEN requires 32 to 512 non-whitespace ASCII characters")
    if args.max_file_mib > 512 or args.port > 65535:
        parser.error("the maximum file size is 512 MiB and the maximum port is 65535")
    os.umask(0o077)
    size = args.max_file_mib * 1024 * 1024
    engine = Engine(Limits(size, size, args.timeout_seconds), args.ffmpeg, args.ffprobe)
    encoders = run_command([engine.ffmpeg, "-hide_banner", "-encoders"], 10, read_output=True).decode()
    for name in ("libx264", "libmp3lame", "libwebp"):
        if not re.search(rf"^\s*[A-Z.]+\s+{name}\s", encoders, re.MULTILINE):
            parser.error(f"FFmpeg needs the {name} encoder")
    store = Store(args.state, args.max_requests, args.retention_hours * 3600, 2 * size)
    worker = Worker(engine, store)

    def terminate(*_):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, terminate)
    try:
        with Server((args.host, args.port), store, worker, token) as server:
            worker.thread.start()
            try:
                server.serve_forever()
            except KeyboardInterrupt:
                pass
    finally:
        worker.stop.set()
        worker.wakeup.set()
        if worker.thread.is_alive():
            worker.thread.join()
        store.close()


if __name__ == "__main__":
    main()
