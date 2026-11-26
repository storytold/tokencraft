"""Exercise the real Node plugin wire payload without running an HTTP service."""

import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from history import History
from monitor import Collector, metrics
from plugin_traffic import activity, parse_sample
from sources import observe_target
from test_monitor import POLICY

PLUGINS = Path(__file__).resolve().parents[2] / "plugins/providers/inference"
FIXTURE = """
import { EventEmitter } from 'node:events';
const { monitoringProbe } = await import(process.argv[1]);
const token = 'm'.repeat(40);
const monitor = monitoringProbe(token, 'test-service-token', undefined, ['submit']);
const request = {
  method: 'GET', url: '/internal/monitoring', headers: { accept: 'application/json' },
  headersDistinct: { authorization: ['Bearer ' + token] },
};
const poll = () => monitor.handle(request, {
  writeHead(status) { if (status !== 200) throw Error('probe did not produce telemetry'); },
  end(body) { console.log(body); },
});
poll();
for (const code of [200, 429, 502, 429, 429]) {
  const response = new EventEmitter();
  response.statusCode = code;
  monitor.record('submit', response);
  response.emit('finish');
}
poll();
"""
ROUTE_FIXTURE = """
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { Readable } from 'node:stream';
const [module, provider, state] = process.argv.slice(1);
const { queueServer, openRouterProxy } = await import(module);
const token = 's'.repeat(40);
const monitoringToken = 'm'.repeat(40);
const chat = provider === 'openrouter';
const server = chat
  ? openRouterProxy({ endpoint: 'https://fixture.invalid/v1/chat/completions', upstreamKey: 'fixture',
      gatewayKey: token, models: new Set(['fixture']), monitoringToken })
  : await queueServer({ state, token, monitoringToken, provider: {} });
assert.equal(server.listening, false);
const handle = server.listeners('request')[0];
let upstreamCalls = 0;
globalThis.fetch = async (url) => {
  assert.equal(url, 'https://fixture.invalid/v1/chat/completions');
  upstreamCalls++;
  return new Response('fixture', { status: 429 });
};
async function call(key, method, path, body = null) {
  const request = Readable.from(body === null ? [] : [Buffer.from(JSON.stringify(body))]);
  request.method = method;
  request.url = path;
  request.headers = { accept: 'application/json' };
  request.headersDistinct = { authorization: ['Bearer ' + key] };
  const response = new EventEmitter();
  response.writeHead = (code) => { response.statusCode = code; response.headersSent = true; };
  response.end = (payload) => { response.body = payload; response.emit('finish'); };
  response.destroy = () => response.emit('close');
  await handle(request, response);
  return response;
}
const method = chat ? 'POST' : 'GET';
const path = chat ? '/v1/chat/completions' : '/v1/jobs/fixture_job';
const body = chat ? { model: 'fixture', messages: [{ role: 'user', content: 'fixture' }], max_tokens: 8 } : null;
const denied = await call(monitoringToken, method, path, body);
assert.equal(denied.statusCode, 401);
const completed = await call(token, method, path, body);
assert.equal(completed.statusCode, chat ? 429 : 404);
assert.equal(upstreamCalls, chat ? 1 : 0);
const sample = await call(monitoringToken, 'GET', '/internal/monitoring');
assert.equal(sample.statusCode, 200);
console.log(sample.body);
"""


@unittest.skipUnless(shutil.which("node"), "node is needed for the provider plugin fixture")
class PluginContract(unittest.TestCase):
    def wire_samples(self, provider):
        result = subprocess.run(["node", "--input-type=module", "-e", FIXTURE,
                                 (PLUGINS / provider / "service-auth.mjs").as_uri()],
                                capture_output=True, check=True, timeout=5)
        return result.stdout.splitlines()

    def test_all_three_plugins_send_the_same_contract(self):
        for provider in ("fal", "kinovi", "openrouter"):
            with self.subTest(provider=provider):
                first, second = map(parse_sample, self.wire_samples(provider))
                window = activity(second, first, 20)["submit"]
                self.assertEqual(window["responses_per_second"], 0.25)
                self.assertEqual(window["responses"],
                                 {"ok": 1, "throttled": 3, "server_error": 1, "client_error": 0, "interrupted": 0})
                self.assertGreater(window["p95_ms"], 0)
                self.assertEqual(activity(dict(second, boot_id="restart"), first, 20), {})
                self.assertEqual(activity(first, second, 20), {})

    def test_report_persists_deltas_exposes_histogram_and_keeps_gaps_unknown(self):
        first, second = self.wire_samples("fal")
        third = json.loads(second)
        third["operations"]["submit"]["responses"]["throttled"] += 5
        fourth = dict(third, boot_id="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
        payloads = (first, second, json.dumps(third), json.dumps(fourth))
        target = {"id": "fal-plugin", "cell": "demo", "kind": "plugin", "provider": "fal",
                  "url": "https://plugins.example.test/internal/monitoring", "token": "m" * 40}
        config = {"targets": [target], "cells": {}, "interval_seconds": 20,
                  "alerts": dict(POLICY, minimum_outcomes=5)}
        with tempfile.TemporaryDirectory() as directory, patch("monitor.observe_target") as observe:
            replies = [observe_target(target, lambda *_: (200, sample)) for sample in payloads]
            replies.append(observe_target(target, lambda *_: (204, b"")))
            observe.side_effect = replies
            history = History(Path(directory) / "test.sqlite", 3600)
            collector = Collector(config, history, None)
            for timestamp in (100, 120, 140):
                with patch("monitor.time.time", return_value=timestamp):
                    collector.collect()
            snapshot, incidents = history.overview()
            report = "\n".join(metrics(snapshot, incidents, False))
            self.assertIn('outcome="throttled"} 8', report)
            self.assertIn('le="+Inf"} 1', report)
            self.assertNotIn(parse_sample(first)["boot_id"], json.dumps(snapshot))
            self.assertEqual(incidents[0]["key"], "plugin/fal-plugin/submit")
            series = history.series("plugin_throughput", 100, 20, "fal-plugin", "submit")
            self.assertEqual([point["value"] for point in series["points"]], [None, 0.25, 0.25])
            with patch("monitor.time.time", return_value=160):
                collector.collect()
            snapshot, incidents = history.overview()
            self.assertEqual(snapshot["targets"]["fal-plugin"]["plugin"]["operations"], {})
            self.assertIsNone(incidents[0]["resolved_at"])
            collector = Collector(config, history, None)
            with patch("monitor.time.time", return_value=180):
                collector.collect()
            snapshot, _ = history.overview()
            self.assertEqual(snapshot["targets"]["fal-plugin"]["plugin_operations"], ["submit"])
            self.assertNotIn("plugin", snapshot["targets"]["fal-plugin"])

    def test_inconsistent_counts_and_unexpected_labels_are_rejected(self):
        _, second = self.wire_samples("fal")
        broken = json.loads(second)
        broken["operations"]["submit"]["responses"]["ok"] += 1
        with self.assertRaises(ValueError):
            parse_sample(json.dumps(broken))
        broken["operations"]["unexpected-model-id"] = broken["operations"].pop("submit")
        with self.assertRaises(ValueError):
            parse_sample(json.dumps(broken))

    def test_real_gateway_routes_record_traffic_without_binding_a_socket(self):
        for provider in ("fal", "kinovi", "openrouter"):
            with self.subTest(provider=provider), tempfile.TemporaryDirectory() as directory:
                filename = "proxy.mjs" if provider == "openrouter" else "queue.mjs"
                result = subprocess.run(["node", "--input-type=module", "-e", ROUTE_FIXTURE,
                                         (PLUGINS / provider / filename).as_uri(), provider, directory],
                                        capture_output=True, check=True, timeout=5)
                operations = parse_sample(result.stdout)["operations"]
                responses = (operations["chat"] if provider == "openrouter" else operations["status"])["responses"]
                self.assertEqual(responses["throttled" if provider == "openrouter" else "client_error"], 1)
                self.assertEqual(sum(responses.values()), 1)

if __name__ == "__main__":
    unittest.main()
