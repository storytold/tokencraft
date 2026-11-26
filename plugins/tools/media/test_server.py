from contextlib import contextmanager
from hashlib import sha256
import http.client
import json
from pathlib import Path
import tempfile
from threading import Thread
import time
import unittest

from engine import Engine, Limits
from queue_store import Conflict, Store
from server import Server, Worker


TOKEN = "media-runner-test-" + "a" * 32
IMAGE_1X1 = bytes.fromhex(
    "89504e470d0a1a0a0000000d4948445200000001000000010804000000b51c0c02"
    "0000000b4944415478da6364f80f00010501012718e3660000000049454e44ae426082"
)


@contextmanager
def running(store, engine, *, start_worker=True):
    worker = Worker(engine, store)
    server = Server(("127.0.0.1", 0), store, worker, TOKEN)
    thread = Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05})
    thread.start()
    if start_worker:
        worker.thread.start()
    try:
        yield server.server_port
    finally:
        server.shutdown()
        thread.join()
        server.server_close()
        worker.stop.set()
        worker.wakeup.set()
        if start_worker:
            worker.thread.join(timeout=15)
            if worker.thread.is_alive():
                raise RuntimeError("fixture media worker did not stop")


def request(port, method, path, *, authorized=True, headers=None, data=None):
    headers = dict(headers or {})
    if authorized:
        headers["Authorization"] = "Bearer " + TOKEN
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    try:
        connection.request(method, path, body=data, headers=headers)
        response = connection.getresponse()
        return response.status, dict(response.headers), response.read()
    finally:
        connection.close()


class ProtocolTests(unittest.TestCase):
    def test_authenticated_job_recovers_stable_handle_and_output_after_restart(self):
        digest = sha256(IMAGE_1X1).hexdigest()
        upload = "/v1/jobs/req_protocol/inputs/image"
        headers = {"X-TokenCraft-Content-Sha256": digest}
        job = {
            "version": 1, "request_id": "req_protocol", "model": "image.resize",
            "input": {"kind": "tool", "revision": 1,
                      "inputs": {"image": {"sha256": digest, "size_bytes": len(IMAGE_1X1)}},
                      "parameters": {"width": 32, "height": 24}},
        }
        job_headers = {"Idempotency-Key": "req_protocol", "Content-Type": "application/json"}

        def submit(port, payload):
            return request(port, "POST", "/v1/jobs", headers=job_headers, data=json.dumps(payload))

        def decode(response, status=200):
            self.assertEqual(response[0], status)
            return json.loads(response[2])

        with tempfile.TemporaryDirectory() as folder:
            state = Path(folder)
            engine = Engine(Limits(timeout_seconds=10))
            store = Store(state, max_requests=8, retention_seconds=60, request_bytes=1024 * 1024)
            try:
                with running(store, engine, start_worker=False) as port:
                    self.assertEqual(request(port, "GET", "/v1/tools", authorized=False)[0], 401)
                    self.assertGreater(len(decode(request(port, "GET", "/v1/tools"))["tools"]), 20)
                    self.assertEqual(decode(submit(port, job))["outcome"], {"status": "rejected"})
                    self.assertEqual(request(port, "PUT", upload, headers=headers, data=b"invalid")[0], 400)
                    for _ in range(2):
                        self.assertEqual(request(port, "PUT", upload, headers=headers, data=IMAGE_1X1)[0], 204)
                    status = decode(submit(port, job))
                    handle = status["outcome"]["handle"]
                    self.assertEqual(decode(submit(port, job)), status)
                    changed = dict(job, input=dict(job["input"], parameters={"width": 30, "height": 26}))
                    self.assertEqual(submit(port, changed)[0], 409)
                    intruder = b"replacement"
                    new_headers = {"X-TokenCraft-Content-Sha256": sha256(intruder).hexdigest()}
                    self.assertEqual(request(port, "PUT", upload, headers=new_headers, data=intruder)[0], 409)
                    self.assertEqual(request(port, "GET", f"/v1/jobs/{handle}/artifacts/0")[0], 404)
                    # Simulate a restart between claiming the request and publishing its result.
                    self.assertEqual(store.claim()["handle"], handle)
            finally:
                store.close()

            restarted = Store(state, max_requests=8, retention_seconds=60, request_bytes=1024 * 1024)
            try:
                with running(restarted, engine) as port:
                    self.assertEqual(decode(request(port, "GET", "/ready")), {"ready": True})
                    lookup = decode(request(port, "GET", "/v1/jobs/by-request/req_protocol"))
                    self.assertEqual(lookup["handle"], handle)
                    for _ in range(100):
                        progress = decode(request(port, "GET", f"/v1/jobs/{handle}"))
                        if progress["status"] in ("succeeded", "failed"):
                            break
                        time.sleep(0.03)
                    self.assertEqual(progress["status"], "succeeded")
                    status, headers, body = request(port, "GET", f"/v1/jobs/{handle}/artifacts/0")
                    self.assertEqual((status, headers["Content-Type"]), (200, "image/png"))
                    self.assertEqual(headers["X-TokenCraft-Content-Sha256"], sha256(body).hexdigest())
                    self.assertEqual(len(body), int(headers["Content-Length"]))
                    self.assertEqual(body[16:24], bytes.fromhex("0000002000000018"))
                    self.assertEqual(decode(submit(port, job))["outcome"]["handle"], handle)
            finally:
                restarted.close()

    def test_retention_rejects_new_sources_for_an_old_id(self):
        with tempfile.TemporaryDirectory() as folder:
            state = Path(folder)
            store = Store(state, max_requests=1, retention_seconds=60, request_bytes=64)
            self.addCleanup(store.close)
            source = state / "source"
            source.write_bytes(b"input")
            digest = sha256(source.read_bytes()).hexdigest()
            store.stage("req_old", "image", digest, 5, source)
            with store.write() as db:
                db.execute("UPDATE requests SET updated_at = 0")
            store.expire()
            self.assertEqual(store.lookup("req_old", by_id=True)["status"], "expired")
            self.assertFalse((state / "inputs" / "req_old").exists())
            source.write_bytes(b"input")
            with self.assertRaises(Conflict):
                store.stage("req_old", "image", digest, 5, source)
            store.stage("req_new", "image", digest, 5, source)
            self.assertTrue((state / "inputs" / "req_new" / "image").is_file())


if __name__ == "__main__":
    unittest.main()
