"""Exercise the public console release gate on two disposable loopback origins."""

from contextlib import ExitStack
from gzip import compress
from hashlib import sha256
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
CONSOLE_HTML = b'<!doctype html><main id="root">Fixture console</main>'
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
        content_type = "application/json"
        headers = {}
        if route == "/":
            content_type = self.server.console_type
            headers["Content-Encoding"] = "gzip"
            data = compress(self.server.console_html)
        elif route == "/ready":
            index = self.server.ready_calls
            self.server.ready_calls += 1
            state, headers = self.server.ready_responses[min(index, len(self.server.ready_responses) - 1)]
            data = json.dumps({"ready": state}).encode()
        elif route in ASSETS:
            data = (ROOT / ASSETS[route]).read_bytes()
        elif route == "/release.json":
            headers = self.server.release_headers
            data = json.dumps({"source_revision": REVISION, "index_sha256": sha256(CONSOLE_HTML).hexdigest()}).encode()
        elif route in {"/v1/rate-limits", "/v1/auth/me"}:
            status = 401
            headers = {"Cache-Control": "private, no-store"}
            if route == "/v1/auth/me" and self.server.session_age is not None:
                headers["Age"] = self.server.session_age
            data = b'{"error":{"code":"unauthorized"}}'
        else:
            self.send_error(404)
            return
        if route.startswith("/v1/") or (route == "/openapi.json" and self.server.gateway_origin):
            revision = self.server.route_revisions.get(route, self.server.default_revision)
            if revision is not None:
                headers["TokenCraft-Source-Revision"] = revision
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        for key, value in headers.items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(data)


def release_gate(console_ready, gateway_ready, revision=REVISION, *, console_session_age=None,
                 console_route_revisions=None, console_html=CONSOLE_HTML, console_type="text/html; charset=utf-8",
                 release_headers=None):
    with ExitStack() as stack:
        servers = []
        for responses in (console_ready, gateway_ready):
            server = stack.enter_context(serve(ThreadingHTTPServer(("127.0.0.1", 0), ConsoleFixture)))
            server.ready_calls = 0
            server.ready_responses = responses
            server.session_age = console_session_age if not servers else None
            server.route_revisions = (console_route_revisions or {}) if not servers else {}
            server.gateway_origin = bool(servers)
            server.default_revision = revision
            server.console_html = console_html
            server.console_type = console_type
            server.release_headers = {"Cache-Control": "no-store"} if release_headers is None else release_headers
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

    def test_rejects_a_console_document_from_a_different_build(self):
        observed, _ = release_gate([(True, self.fresh)], [(True, self.fresh)],
                                   console_html=b"<!doctype html><p>Earlier console release</p>")
        self.assertEqual(observed.returncode, 1, observed.stdout + observed.stderr)
        self.assertIn("FAIL console / must match the release HTML", observed.stderr)

    def test_rejects_console_html_served_as_plain_text(self):
        observed, _ = release_gate([(True, self.fresh)], [(True, self.fresh)], console_type="text/plain")
        self.assertEqual(observed.returncode, 1, observed.stdout + observed.stderr)
        self.assertIn("FAIL console / must match the release HTML", observed.stderr)

    def test_rejects_a_cached_or_storable_console_release_manifest(self):
        for headers in ({"Cache-Control": "no-store", "Age": "30"}, {}):
            with self.subTest(headers=headers):
                observed, _ = release_gate([(True, self.fresh)], [(True, self.fresh)], release_headers=headers)
                self.assertEqual(observed.returncode, 1, observed.stdout + observed.stderr)
                self.assertIn("FAIL console /release.json", observed.stderr)

    def test_versioned_check_requires_each_proxied_response_to_match_readiness(self):
        for route, revision in (("/v1/schemas/primitives", "b" * 40), ("/v1/rate-limits", None),
                                ("/v1/auth/me", "b" * 40)):
            with self.subTest(route=route, revision=revision):
                observed, _ = release_gate([(True, self.fresh)], [(True, self.fresh)],
                                           console_route_revisions={route: revision})
                self.assertEqual(observed.returncode, 1, observed.stdout + observed.stderr)
                self.assertIn(f"FAIL console {route}", observed.stderr)
                self.assertIn("PASS console /ready and gateway source revision", observed.stdout)


if __name__ == "__main__":
    unittest.main()
