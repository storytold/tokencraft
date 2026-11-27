"""Offline coverage of operator configuration, telemetry, alerts, and policy wiring."""

import base64
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from http.client import HTTPMessage
from urllib.error import HTTPError

from autoscaling_snapshot import snapshot as scaling_snapshot
from history import History
from monitor import Collector, Handler, evaluate, metrics
from sources import gpu_sample, load_config, observe_target, probe_url, replica_sample

HERE = Path(__file__).parent
CAPACITY_TOKEN = "a" * 64
PLUGIN_TOKEN = "p" * 64
POLICY = {
    "consecutive_samples": 2,
    "queue_seconds": 60,
    "publication_seconds": 120,
    "webhook_seconds": 300,
    "failure_percent": 25,
    "minimum_outcomes": 10,
}


def sample():
    return {
        "window_seconds": 900, "truncated": False, "provider": {}, "compute": {},
        "eligible": 2, "oldest_eligible_seconds": 75, "database_connections_available": 20,
        "provider_work": {
            "submit": {"eligible": 2, "oldest_seconds": 75},
            "poll": {"eligible": 1, "oldest_seconds": 95},
            "lookup": {"eligible": 0, "oldest_seconds": None},
        },
        "publication": [], "request_queues": [], "workflow_queues": [],
        "delivery_queues": [], "workflow_outcomes": {},
    }


class Sources(unittest.TestCase):
    def test_example_parses_without_reading_private_sources(self):
        with patch.dict(os.environ, TOKENCRAFT_CAPACITY_TOKEN=CAPACITY_TOKEN, TOKENCRAFT_MONITOR_FAL=PLUGIN_TOKEN):
            config = load_config(HERE / "settings.example.json")
        self.assertEqual(config["cells"]["dev-cell"]["providers"], ["fal", "kinovi", "openrouter", "self_hosted"])
        self.assertEqual(config["targets"][2]["token"], PLUGIN_TOKEN)

    def test_compute_example_imports_the_gateway_stack_without_network(self):
        config = load_config(HERE / "settings.compute.example.json")
        targets = config["targets"]
        self.assertEqual([target["target"] for target in targets], ["primary", "standby"])
        self.assertEqual([target["url"] for target in targets],
                         ["http://127.0.0.1:4031/v1/models", "http://127.0.0.1:4032/v1/models"])
        for target in targets:
            self.assertEqual((target["cell"], target["provider"], target["stack"], target["cloud"], target["region"]),
                             ("dev-cell", "self_hosted", "local-chat", "local", "workstation"))
            self.assertEqual(target["expected_models"], ["local-chat"])
            self.assertIsNone(target["token"])
        self.assertEqual([target["id"] for target in targets], [
            target["id"] for target in load_config(HERE / "settings.compute.example.json")["targets"]
        ])

    def test_profile_overrides_keep_inference_credentials_out_of_health_checks(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            catalog = directory / "catalog.json"
            bindings = directory / "bindings.json"
            settings = directory / "settings.json"
            catalog.write_text(json.dumps({"version": 1, "offers": [
                {"provider": "self_hosted", "adapter": "openai_compatible_chat", "local_binding": "chat",
                 "upstream_model": model} for model in ("text-v2", "text-v1")
            ]}))
            bindings.write_text(json.dumps({"version": 1, "cell": "east", "bindings": [{
                "name": "chat", "targets": [
                    {"id": "primary", "endpoint": "https://provider.example/v1/chat/completions",
                     "key_env": "TOKENCRAFT_LOCAL_CHAT_KEY_PROVIDER", "zone": "east-a",
                     "location": {"cloud": "private", "region": "east"}},
                ],
            }]}))
            override = {"provider": "self_hosted", "stack": "chat", "target": "primary",
                        "id": "chat-east", "url": "https://status.example/ready", "token_env": "FIXTURE_PROBE"}
            cell = {"id": "east", "providers": ["self_hosted"], "compute_profile": {
                "catalog": "catalog.json", "bindings": "bindings.json", "probes": [override]}}
            data = {"version": 1, "cells": [cell], "targets": []}
            settings.write_text(json.dumps(data))
            with patch.dict(os.environ, {"FIXTURE_PROBE": PLUGIN_TOKEN, "TOKENCRAFT_LOCAL_CHAT_KEY_PROVIDER": "unread"}):
                target = load_config(settings)["targets"][0]
            self.assertEqual((target["id"], target["url"], target["token"]),
                             ("chat-east", "https://status.example/ready", PLUGIN_TOKEN))
            self.assertNotIn("expected_models", target)
            override["verify_models"] = True
            settings.write_text(json.dumps(data))
            with patch.dict(os.environ, {"FIXTURE_PROBE": PLUGIN_TOKEN}):
                target = load_config(settings)["targets"][0]
            self.assertEqual(target["expected_models"], ["text-v1", "text-v2"])
            result, _ = observe_target(target, lambda *_: (200, b'{"data":[{"id":"text-v1"},{"id":[]}]}'))
            self.assertEqual((result["status"], result["signals"]["missing_models"]), ("degraded", 1))
            result, _ = observe_target(target, lambda *_: (200, b'{"data":[{"id":"text-v1"},{"id":"text-v2"}]}'))
            self.assertEqual((result["status"], result["signals"]["missing_models"]), ("up", 0))
            self.assertNotIn("text-v1", json.dumps(result))

            override["target"] = "retired"
            settings.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, "override does not match"):
                load_config(settings)
            cell["id"] = "different"
            settings.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, "home cell must match"):
                load_config(settings)

    def test_remote_http_url_credentials_and_query_rejected(self):
        for url in ("http://private.example/ready", "https://a:b@example.test/ready",
                    "https://example.test/ready?secret=x", "https://example.test/ready#secret",
                    "https://example.test:0/ready", "https://example.test/health\\other"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                probe_url(url)
        self.assertEqual(probe_url("http://[::1]:4000/ready").hostname, "::1")

    def test_replica_exposes_only_expected_checks_and_binding_shape(self):
        payload = json.dumps({"ready": True, "role": "worker", "instance_id": "d" * 32,
                              "provider_tasks": 4, "provider_capacity": 16,
                              "checks": {"migrations": "ready", "large_private_field": "not fetched",
                                         "database": {"untrusted": True}}, "local_bindings": ["f" * 64]})
        ready, signals, bindings = replica_sample(payload, "worker")
        self.assertTrue(ready)
        self.assertEqual(signals["checks"], {"migrations": "ready"})
        self.assertEqual(signals["instance_id"], "d" * 32)
        self.assertEqual(bindings["local"], ["f" * 64])
        with self.assertRaises(ValueError):
            replica_sample(payload, "gateway")

    def test_plugin_probe_and_upstream_model_check_are_distinct(self):
        remote = "https://example.test/internal/monitoring"
        plugin = {"id": "plugin-a", "cell": "lab", "kind": "plugin", "provider": "fal",
                  "url": remote, "token": PLUGIN_TOKEN}
        result, _ = observe_target(plugin, lambda *_: (204, b""))
        self.assertEqual(result["status"], "up")
        self.assertNotIn("token", result)
        self.assertNotIn("url", result)
        result, _ = observe_target(plugin, lambda *_: (202, b""))
        self.assertEqual((result["status"], result["reason"]), ("unknown", "plugin_contract"))
        compute = dict(plugin, kind="compute", id="a", expected_model="image")
        result, _ = observe_target(compute, lambda *_: (200, b'{"data":[{"id":"other"}]}'))
        self.assertEqual((result["status"], result["reason"]), ("degraded", "model_unavailable"))

        def throttled(*_):
            raise HTTPError(remote, 429, "", {}, None)

        self.assertEqual(observe_target(plugin, throttled)[0]["reason"], "rate_limited")

    def test_gpu_invalid_sentinel_and_mig_do_not_pollute_physical_gpu(self):
        result = gpu_sample(b"""
DCGM_FI_DEV_GPU_UTIL{gpu="0",UUID="GPU-a"} 58
DCGM_FI_DEV_GPU_UTIL{gpu="0",UUID="GPU-a",GPU_I_ID="5"} 100
DCGM_FI_DEV_GPU_UTIL{gpu="1",UUID="GPU-b"} 9223372036854775794
DCGM_FI_DEV_FB_USED{gpu="0",UUID="GPU-a"} 400
DCGM_FI_DEV_FB_USED{gpu="1",UUID="GPU-b"} 500
DCGM_FI_DEV_GPU_TEMP{gpu="1",UUID="GPU-b"} NaN
""")
        self.assertEqual(result, {"gpu_utilization_percent": 58, "gpu_memory_used_mib": 900, "gpu_count": 2})


class Recording(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory(prefix="tokencraft-monitor-")
        self.addCleanup(folder.cleanup)
        self.history = History(Path(folder.name, "state.sqlite"), 300)

    def record(self, timestamp, bad):
        properties = {
            "observed_at": timestamp, "targets": {"lab.2": {"status": "down" if bad else "up", "latency_ms": 3}},
            "cells": {"east": {"sample": {"provider": {"fal": {"p95_total_seconds": 6}}},
                               "scopes": {"review.1": {"sample": {"provider": {"fal": {"p95_total_seconds": 4}}}}}}},
        }
        self.history.record(properties, {"probe/lab.2": ("Fixture outage", bad)}, 2, 15)

    def test_incident_survives_restart_and_unknown_does_not_resolve(self):
        self.record(100, True)
        self.record(115, True)
        self.history = History(self.history.path, 300)
        self.record(130, None)
        self.record(145, False)
        snapshot, incidents = self.history.overview()
        self.assertEqual(snapshot["observed_at"], 145)
        self.assertIsNone(incidents[0]["resolved_at"])
        self.record(160, False)
        _, incidents = self.history.overview()
        self.assertEqual(incidents[0]["resolved_at"], 160)
        report = self.history.series("probe_latency", 100, 30, "lab.2")
        self.assertEqual(report["availability"], {"up": 3, "degraded": 0, "down": 2, "unknown": 0})
        self.assertEqual(len(report["points"]), 3)
        report = self.history.series("request_p95", 100, 30, "east", "fal", "review.1")
        self.assertEqual([point["value"] for point in report["points"]], [4, 4, 4])

    def test_retention_does_not_break_time_series_or_incident_resumes(self):
        self.record(100, True)
        self.record(200, True)
        self.assertFalse(self.history.overview()[1])
        self.record(215, True)
        self.record(600, True)
        report = self.history.series("probe_latency", 0, 30, "lab.2")
        self.assertEqual([point["observed_at"] for point in report["points"]], [600])
        self.assertEqual(self.history.overview()[1][0]["opened_at"], 215)
        with self.assertRaises(ValueError):
            self.record(600, False)


class IntegrationLogic(unittest.TestCase):
    def test_collection_uses_recent_worker_bindings_and_only_exports_aggregates(self):
        with tempfile.TemporaryDirectory() as root, patch("monitor.observe_target") as probe, patch("monitor.sample_database") as db:
            history = History(Path(root, "state.sqlite"), 300)
            target = {"id": "replica", "cell": "lab", "kind": "worker"}
            config = {
                "targets": [target], "alerts": POLICY, "interval_seconds": 15,
                "cells": {"lab": {"providers": ["fal"], "pg_service": "lab",
                                  "reserved_connections": 20, "scopes": {"team": {"account": "acct_test"}}}},
            }
            binding = {"local": [], "connector": ["b" * 64], "poll": ["b" * 64], "lookup": []}
            probe.return_value = (dict(target, status="up", latency_ms=2, signals={}), binding)
            db.side_effect = lambda *_: {"status": "up", "sample": sample()}
            collector = Collector(config, history, "/psql")
            with patch("monitor.time.time", return_value=100):
                collector.collect()
            probe.return_value = (dict(target, status="down", latency_ms=2, signals={}), None)
            with patch("monitor.time.time", return_value=115):
                collector.collect()
            current, incidents = history.overview()
            lab = current["cells"]["lab"]
            self.assertEqual(lab["queue_coverage"], "cached")
            self.assertEqual(lab["sample"]["eligible"], 2)
            self.assertNotIn("acct_test", json.dumps(current))
            self.assertNotIn("b" * 64, json.dumps(current))
            self.assertEqual(incidents[0]["key"], "dispatch/lab")
            self.assertIn('tokencraft_monitor_incident_open{rule="dispatch",subject="lab"} 1',
                          "\n".join(metrics(current, incidents, False)))
            self.assertNotIn("probe_seconds", "\n".join(metrics(current, incidents, True)))
            with patch("monitor.time.time", return_value=301):
                collector.collect()
            current, _ = history.overview()
            self.assertEqual(current["cells"]["lab"]["queue_coverage"], "unknown")
            self.assertIsNone(current["cells"]["lab"]["sample"]["eligible"])

    def test_provider_failure_threshold_and_insufficient_traffic(self):
        data = sample()
        data["provider"] = {"fal": {"failed": 3, "uncertain": 1, "succeeded": 6}}
        state = {"targets": {}, "cells": {"lab": {"sample": data, "status": "up", "queue_coverage": "fresh",
                                                "providers": ["fal", "kinovi"]}}}
        alerts = evaluate(state, POLICY)
        self.assertIs(alerts["provider/lab/fal"][1], True)
        self.assertIsNone(alerts["provider/lab/kinovi"][1])

    def test_scale_adapter_requires_matching_inventory_freshness_and_headroom(self):
        obs = {"observed_at": 110, "cells": {"lab": {"sample": sample(), "queue_coverage": "fresh",
               "worker_bindings_uniform": True, "reserved_connections": 20}}, "targets": {
               "api": {"id": "api", "cell": "lab", "kind": "gateway", "status": "up",
                       "signals": {"instance_id": "a" * 32, "sse_readers": 8, "sse_capacity": 32}},
               "work": {"id": "work", "cell": "lab", "kind": "worker", "status": "up",
                        "signals": {"instance_id": "b" * 32, "provider_tasks": 15, "provider_capacity": 16}},
        }}
        source = {"cell": "lab", "observed_at": 108, "database_healthy": True, "reserve_connections": 20,
                  "api": {"replicas": ["api"], "desired": 1, "stable": True, "deployment_version": "x", "cpu_fraction": .6},
                  "worker": {"replicas": ["work"], "desired": 1, "stable": True, "deployment_version": "y"}}
        computed = scaling_snapshot(obs, source, 115)
        self.assertEqual(computed["observed_at"], 108)
        self.assertEqual(computed["worker"]["oldest_ready_age_seconds"], 95)
        self.assertEqual(computed["worker"]["eligible_sample"], 3)
        self.assertFalse(computed["worker"]["provider_headroom"])
        self.assertRegex(computed["api"]["replica_set"], r"^[0-9a-f]{64}$")
        obs["targets"]["alias"] = dict(obs["targets"]["api"], id="alias")
        source["api"].update(desired=2, replicas=["api", "alias"])
        with self.assertRaisesRegex(ValueError, "same replica process"):
            scaling_snapshot(obs, source, 115)
        del obs["targets"]["alias"]
        source["api"].update(desired=1, replicas=["api"])
        instance = obs["targets"]["api"]["signals"].pop("instance_id")
        with self.assertRaisesRegex(ValueError, "valid instance ID"):
            scaling_snapshot(obs, source, 115)
        obs["targets"]["api"]["signals"]["instance_id"] = instance
        with self.assertRaises(ValueError):
            scaling_snapshot(obs, source, 200)
        obs["cells"]["lab"]["queue_coverage"] = "cached"
        with self.assertRaises(ValueError):
            scaling_snapshot(obs, source, 115)

    def test_console_rejects_missing_and_duplicate_authorization(self):
        class Fake(Handler):
            def __init__(self):
                self.path = "/metrics"
                self.headers = HTTPMessage()
                self.server = type("Server", (), {"auth": b"Basic " + base64.b64encode(b"operator:fixture")})()
                self.status = None

            def reply(self, status, *_args):
                self.status = status

        request = Fake()
        request.do_GET()
        self.assertEqual(request.status, 401)
        for _ in range(2):
            request.headers["Authorization"] = request.server.auth.decode()
        request.do_GET()
        self.assertEqual(request.status, 401)


if __name__ == "__main__":
    unittest.main()
