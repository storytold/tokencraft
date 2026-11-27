"""Exercise the public console release gate on two disposable loopback origins."""

from contextlib import ExitStack
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import unittest
from urllib.parse import urlsplit

from smoke_cell_faults import serve


ROOT = Path(__file__).resolve().parents[1]
REVISION = "a" * 40
ASSETS = {
    "/openapi.json": "contracts/openapi.json",
    "/docs/openapi.json": "contracts/openapi.json",
    "/v1/schemas/primitives": "crates/core/provider-mapping/schemas/v1.json",
    "/catalog/artcraft-v1.json": "catalog/artcraft-v1.json",
}


class ConsoleFixture(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        route = urlsplit(self.path).path
        status = 200
        headers = {}
        if route == "/ready":
            index = self.server.ready_calls
            self.server.ready_calls += 1
            state, headers = self.server.ready_responses[min(index, len(self.server.ready_responses) - 1)]
            data = json.dumps({"ready": state}).encode()
        elif route in ASSETS:
            data = (ROOT / ASSETS[route]).read_bytes()
        elif route == "/release.json":
            data = json.dumps({"source_revision": REVISION}).encode()
        elif route in {"/v1/rate-limits", "/v1/auth/me"}:
            status = 401
            headers = {"Cache-Control": "private, no-store"}
            if route == "/v1/auth/me" and self.server.session_age is not None:
                headers["Age"] = self.server.session_age
            data = b'{"error":{"code":"unauthorized"}}'
        else:
            self.send_error(404)
            return
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        for key, value in headers.items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(data)


def release_gate(console_ready, gateway_ready, revision=REVISION, *, console_session_age=None):
    with ExitStack() as stack:
        servers = []
        for responses in (console_ready, gateway_ready):
            server = stack.enter_context(serve(ThreadingHTTPServer(("127.0.0.1", 0), ConsoleFixture)))
            server.ready_calls = 0
            server.ready_responses = responses
            server.session_age = console_session_age if not servers else None
            servers.append(server)
        origins = [f"http://127.0.0.1:{server.server_port}" for server in servers]
        args = ["bash", str(ROOT / "scripts/smoke_hosted_console.sh"), *origins]
        if revision is not None:
            args.append(revision)
        environment = {**os.environ, "NO_PROXY": "127.0.0.1", "no_proxy": "127.0.0.1"}
        observed = subprocess.run(args, env=environment, capture_output=True, text=True, timeout=15)
        return observed, [server.ready_calls for server in servers]


class HostedConsoleReadinessTest(unittest.TestCase):
    fresh = {"TokenCraft-Source-Revision": REVISION, "Cache-Control": "no-store"}

    def test_fresh_ready_and_revision_are_read_together_from_each_origin(self):
        observed, calls = release_gate([(True, self.fresh)], [(True, {**self.fresh, "Age": "0"})],
                                       console_session_age="0")
        self.assertEqual(observed.returncode, 0, observed.stderr)
        self.assertEqual(calls, [1, 1])

    def test_rejects_cached_and_storable_ready_responses(self):
        for headers in ({**self.fresh, "Age": "30"}, {"TokenCraft-Source-Revision": REVISION}):
            with self.subTest(headers=headers):
                observed, _ = release_gate([(True, headers)], [(True, self.fresh)])
                self.assertEqual(observed.returncode, 1, observed.stdout + observed.stderr)
                self.assertIn("FAIL console /ready", observed.stderr)

    def test_cannot_combine_healthy_previous_revision_with_unready_expected_revision(self):
        previous = {**self.fresh, "TokenCraft-Source-Revision": "b" * 40}
        for responses in ([(True, previous), (False, self.fresh)], [(False, self.fresh)]):
            with self.subTest(responses=responses):
                observed, _ = release_gate(responses, [(True, self.fresh)])
                self.assertEqual(observed.returncode, 1, observed.stdout + observed.stderr)
                self.assertIn("FAIL console /ready", observed.stderr)

    def test_unversioned_check_keeps_supporting_unstamped_gateways(self):
        observed, calls = release_gate([(True, {})], [(True, {})], revision=None)
        self.assertEqual(observed.returncode, 0, observed.stderr)
        self.assertEqual(calls, [1, 1])

    def test_cached_session_denial_fails_even_with_non_storable_policy(self):
        observed, _ = release_gate([(True, self.fresh)], [(True, self.fresh)], console_session_age="30")
        self.assertEqual(observed.returncode, 1, observed.stdout + observed.stderr)
        self.assertIn("FAIL console /v1/auth/me", observed.stderr)


if __name__ == "__main__":
    unittest.main()
