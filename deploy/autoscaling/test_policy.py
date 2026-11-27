import unittest
from contextlib import nullcontext
from copy import deepcopy
from unittest.mock import patch

from collect import collect
from controller import apply_scale, decide


class PolicyTest(unittest.TestCase):
    def setUp(self):
        self.config = {
            "cell": "test", "context": "test-context", "namespace": "test-namespace",
            "connections_per_replica": 10, "connection_budget": 100,
            "api": {"deployment": "api", "min": 2, "max": 5, "allow_scale_in": True},
            "worker": {"deployment": "worker", "min": 2, "max": 4},
        }
        self.sample = {
            "cell": "test", "observed_at": 1000,
            "database": {"healthy": True, "primary": True, "additional_connections": 40, "admissions_paused": False},
            "api": {"desired": 3, "ready": 3, "stable": True, "deployment_version": "11",
                    "cpu_fraction": 0.4, "sse_readers": 0, "sse_capacity": 96},
            "worker": {"desired": 2, "ready": 2, "stable": True, "deployment_version": "21", "active_tasks": 0,
                       "task_capacity": 8, "oldest_ready_age_seconds": 0, "provider_headroom": True},
        }

    def tick(self, previous, offset):
        self.sample["observed_at"] = 1000 + offset
        return decide(self.config, self.sample, previous, 1000 + offset)

    def test_sustained_api_sse_pressure_and_actuator_precondition(self):
        self.sample["api"]["sse_readers"] = 80
        action, state = self.tick({}, 0)
        self.assertEqual(action["decision"], "hold")
        self.assertEqual(self.tick(state, 15)[0]["decision"], "hold")
        action, _ = self.tick(state, 30)
        self.assertEqual((action["role"], action["expected"], action["replicas"]), ("api", 3, 4))
        with patch("controller.subprocess.run") as run:
            apply_scale("kubectl", self.config, action)
        self.assertIn("--current-replicas=3", run.call_args.args[0])
        self.assertIn("--resource-version=11", run.call_args.args[0])
        self.assertIn("--context=test-context", run.call_args.args[0])
        self.assertIn("--replicas=4", run.call_args.args[0])

    def test_worker_scale_out_requires_provider_and_writer_headroom(self):
        self.sample["worker"].update(oldest_ready_age_seconds=61, active_tasks=8, provider_headroom=False)
        _, state = self.tick({}, 0)
        action, state = self.tick(state, 30)
        self.assertEqual(action["decision"], "hold")
        self.sample["worker"]["provider_headroom"] = True
        _, state = self.tick(state, 45)
        self.sample["database"]["additional_connections"] = 9
        action, state = self.tick(state, 75)
        self.assertEqual(action["decision"], "hold")
        self.sample["database"]["additional_connections"] = 40
        self.config["connection_budget"] = 59
        self.assertEqual(self.tick(state, 90)[0]["decision"], "hold")
        self.config["connection_budget"] = 60
        action, _ = self.tick(state, 90)
        self.assertEqual((action["role"], action["replicas"]), ("worker", 3))

    def test_gap_or_rollout_resets_the_pressure_window(self):
        self.sample["api"]["cpu_fraction"] = 0.90
        _, state = self.tick({}, 0)
        self.assertEqual(self.tick(state, 60)[0]["decision"], "hold")
        self.sample["api"]["stable"] = False
        action, state = self.tick(state, 30)
        self.assertEqual(action["decision"], "hold")
        self.sample["api"]["stable"] = True
        self.assertEqual(self.tick(state, 45)[0]["decision"], "hold")
        stale = deepcopy(self.sample)
        self.assertEqual(decide(self.config, stale, state, 2000)[0]["decision"], "hold")

    def test_replica_replacement_restarts_pressure_at_the_same_desired_count(self):
        self.sample["api"].update(cpu_fraction=0.9, replica_set="a" * 64)
        _, state = self.tick({}, 0)
        self.sample["api"].update(replica_set="b" * 64, deployment_version="12")
        action, state = self.tick(state, 30)
        self.assertEqual(action["decision"], "hold")
        self.assertEqual(state["roles"]["api"]["high_since"], 1030)
        action, _ = self.tick(state, 60)
        self.assertEqual((action["decision"], action["role"]), ("scale", "api"))

    def test_unverified_writer_clears_pressure_even_at_the_same_timestamp(self):
        self.sample["api"]["cpu_fraction"] = 0.9
        self.sample["worker"].update(oldest_ready_age_seconds=61, active_tasks=8)
        database = self.sample["database"]
        for evidence in (False, None, 1, "true"):
            with self.subTest(evidence=evidence):
                database["primary"] = True
                _, state = self.tick({}, 0)
                _, state = self.tick(state, 15)
                if evidence is None:
                    database.pop("primary")
                else:
                    database["primary"] = evidence
                action, state = self.tick(state, 15)
                self.assertIn("primary database", action["reason"])
                self.assertEqual(state["roles"], {})
                self.assertEqual(self.tick(state, 30)[0]["decision"], "hold")

                database["primary"] = True
                action, state = self.tick(state, 30)
                self.assertEqual(action["decision"], "hold")
                self.assertEqual(self.tick(state, 60)[0]["decision"], "scale")

    def test_legacy_state_cannot_supply_verified_writer_pressure(self):
        self.sample["api"]["cpu_fraction"] = 0.9
        _, state = self.tick({}, 0)
        del state["database_primary"]
        action, state = self.tick(state, 30)
        self.assertEqual(action["decision"], "hold")
        self.assertEqual(state["roles"]["api"]["high_since"], 1030)

    def test_api_can_scale_over_requested_cpu_without_provider_slots(self):
        self.sample["api"]["cpu_fraction"] = 1.3
        self.sample["worker"]["task_capacity"] = 0
        _, state = self.tick({}, 0)
        self.assertEqual(self.tick(state, 30)[0]["role"], "api")

    def test_scale_in_waits_and_respects_minimum(self):
        self.sample["api"]["cpu_fraction"] = 0.10
        state = {}
        for offset in range(0, 300, 15):
            action, state = self.tick(state, offset)
            self.assertEqual(action["decision"], "hold")
        action, _ = self.tick(state, 300)
        self.assertEqual((action["role"], action["replicas"]), ("api", 2))
        self.sample["api"].update(desired=2, ready=2)
        self.assertEqual(self.tick(state, 300)[0]["decision"], "hold")

    def test_scale_in_requires_a_live_guard_and_a_still_fresh_observation(self):
        decision = {"role": "api", "deployment": "api", "replicas": 2, "expected": 3,
                    "deployment_version": "11", "observed_at": 1000}
        with patch("controller.subprocess.run") as run:
            self.assertIn("api.admission_service", apply_scale("kubectl", self.config, decision))
            run.assert_not_called()
        self.config["api"]["admission_service"] = "fixture"
        with patch("controller.admission_guard", return_value=nullcontext()) as guard, \
                patch("controller.time.time", return_value=1046), patch("controller.subprocess.run") as run:
            self.assertIn("expired", apply_scale("kubectl", self.config, decision))
            guard.assert_called_once_with("psql", "fixture")
            run.assert_not_called()
        with patch("controller.admission_guard", return_value=nullcontext("paused")), \
                patch("controller.subprocess.run") as run:
            self.assertEqual(apply_scale("kubectl", self.config, decision), "paused")
            run.assert_not_called()

    def test_admission_service_must_be_a_service_name(self):
        self.config["api"]["admission_service"] = "fixture dbname=other"
        with self.assertRaisesRegex(ValueError, "libpq service name"):
            self.tick({}, 0)

    def test_admission_pause_or_unknown_restarts_the_api_scale_in_window(self):
        self.sample["api"]["cpu_fraction"] = 0.10
        for observed in (True, None, 0, "false"):
            with self.subTest(observed=observed):
                self.sample["database"]["admissions_paused"] = False
                state = {}
                for offset in range(0, 300, 15):
                    _, state = self.tick(state, offset)

                self.sample["database"]["admissions_paused"] = observed
                action, state = self.tick(state, 300)
                self.assertEqual(action["decision"], "hold")
                for offset in range(315, 615, 15):
                    action, state = self.tick(state, offset)
                    self.assertEqual(action["decision"], "hold")

                self.sample["database"]["admissions_paused"] = False
                for offset in range(615, 915, 15):
                    action, state = self.tick(state, offset)
                    self.assertEqual(action["decision"], "hold")
                action, _ = self.tick(state, 915)
                self.assertEqual((action["role"], action["replicas"]), ("api", 2))

    def test_pause_at_the_same_external_timestamp_clears_scale_in_history(self):
        self.sample["api"]["cpu_fraction"] = 0.10
        state = {}
        for offset in range(0, 300, 15):
            _, state = self.tick(state, offset)
        self.sample["database"]["admissions_paused"] = True
        action, state = self.tick(state, 285)
        self.assertEqual(action["decision"], "hold")
        self.sample["database"]["admissions_paused"] = False
        self.assertEqual(self.tick(state, 300)[0]["decision"], "hold")

    def test_legacy_controller_state_cannot_carry_an_open_admissions_window(self):
        self.sample["api"]["cpu_fraction"] = 0.10
        state = {}
        for offset in range(0, 300, 15):
            _, state = self.tick(state, offset)
        del state["admissions_paused"]
        action, current = self.tick(state, 300)
        self.assertEqual(action["decision"], "hold")
        self.assertEqual(current["roles"]["api"]["low_since"], 1300)

    def test_paused_and_legacy_observations_allow_scale_out(self):
        for observed in (True, None):
            with self.subTest(observed=observed):
                if observed is None:
                    self.sample["database"].pop("admissions_paused", None)
                else:
                    self.sample["database"]["admissions_paused"] = observed
                self.sample["api"]["cpu_fraction"] = 0.88
                self.sample["worker"].update(oldest_ready_age_seconds=61, active_tasks=8)
                _, state = self.tick({}, 0)
                action, _ = self.tick(state, 30)
                self.assertEqual((action["role"], action["replicas"]), ("api", 4))
                self.sample["api"]["cpu_fraction"] = 0.10
                _, state = self.tick({}, 0)
                action, _ = self.tick(state, 30)
                self.assertEqual((action["role"], action["replicas"]), ("worker", 3))

    def test_missing_provider_health_does_not_enable_worker_scaling(self):
        source = {
            "cell": "test", "observed_at": 1000, "database_healthy": True, "reserve_connections": 20,
            "api": {"desired": 1, "stable": True, "deployment_version": "11", "cpu_fraction": 0.52, "probe_urls": ["api"]},
            "worker": {"desired": 1, "stable": True, "deployment_version": "21", "probe_urls": ["worker"]},
        }
        probes = {
            "api": {"role": "api", "instance_id": "a" * 32, "ready": True, "sse_readers": 24, "sse_capacity": 32},
            "worker": {"role": "worker", "instance_id": "b" * 32, "ready": True, "provider_tasks": 4, "provider_capacity": 4,
                       "local_bindings": [], "connector_bindings": ["1" * 64],
                       "poll_bindings": ["1" * 64, "2" * 64], "lookup_bindings": ["2" * 64]},
        }
        database = {"database_primary": True, "additional_connections": 90, "eligible_sample": 3, "oldest_ready_age_seconds": 41,
                    "admissions_paused": True}
        with patch("collect.time.time", return_value=1001), \
                patch("collect.scrape", side_effect=lambda url, _: probes[url]), \
                patch("collect.query_database", return_value=database) as query:
            snapshot = collect(source, "synthetic", "psql")
        query.assert_called_once_with("psql", 20, {
            "local": (), "connector": ("1" * 64,), "poll": ("1" * 64, "2" * 64), "lookup": ("2" * 64,),
        })
        self.assertFalse(snapshot["worker"]["provider_headroom"])
        self.assertEqual(snapshot["api"]["sse_readers"], 24)
        self.assertEqual(snapshot["worker"]["oldest_ready_age_seconds"], 41)
        self.assertRegex(snapshot["api"]["replica_set"], r"^[0-9a-f]{64}$")
        self.assertIs(snapshot["database"]["admissions_paused"], True)
        self.assertIs(snapshot["database"]["primary"], True)

        source["database_primary"] = True
        database.pop("database_primary")
        with patch("collect.time.time", return_value=1001), \
                patch("collect.scrape", side_effect=lambda url, _: probes[url]), \
                patch("collect.query_database", return_value=database):
            self.assertIs(collect(source, "synthetic", "psql")["database"]["primary"], False)
            database["database_primary"] = False
            self.assertIs(collect(source, "synthetic", "psql")["database"]["primary"], False)

        source["api"].update(desired=2, probe_urls=["api", "alias"])
        probes["alias"] = probes["api"].copy()
        with patch("collect.time.time", return_value=1001), \
                patch("collect.scrape", side_effect=lambda url, _: probes[url]), \
                patch("collect.query_database") as query:
            with self.assertRaisesRegex(ValueError, "same replica process"):
                collect(source, "synthetic", "psql")
            probes["alias"].pop("instance_id")
            with self.assertRaisesRegex(ValueError, "valid instance ID"):
                collect(source, "synthetic", "psql")
        query.assert_not_called()


if __name__ == "__main__":
    unittest.main()
