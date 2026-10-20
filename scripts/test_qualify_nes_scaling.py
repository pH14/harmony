# SPDX-License-Identifier: AGPL-3.0-or-later

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("nes_scaling", Path(__file__).with_name("qualify-nes-scaling.py"))
scaling = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(scaling)


class ScalingTests(unittest.TestCase):
    def test_missing_cpu_coverage_is_not_reported_as_zero_cost(self):
        unknown = scaling.coordination_costs({}, 10)
        self.assertTrue(all(value is None for value in unknown.values()))
        measured = scaling.coordination_costs({
            "dispatch_phase": {"wall_ns": 100, "host": {"coordinator_cpu_ns": 0}},
            "stream": {"worker_cpu_ns": None, "producer_wait_ns": 20},
        }, 10)
        self.assertEqual(measured["coordinator_cpu_ns_per_job"], 0)
        self.assertIsNone(measured["stream_aux_cpu_ns_per_job"])
        self.assertEqual(measured["producer_wait_ns_per_job"], 2)

    def test_parallel_stream_bottleneck_uses_every_role_and_complete_cpu_coverage(self):
        stream = {"worker_cpu_ns": 90, "serializers": [{"cpu_ns": 60}, {"cpu_ns": 20}],
                  "relay": {"cpu_ns": 10}, "counter_overflow": False}
        def measure():
            return scaling.coordination_costs({"stream": stream}, 10, "task")
        costs = measure()
        self.assertEqual(costs["stream_aux_cpu_ns_per_task"], 9)
        self.assertEqual(costs["serializer_cpu_ns_per_task"], [6, 2])
        self.assertEqual(costs["relay_cpu_ns_per_task"], 1)
        self.assertEqual(costs["stream_dedicated_core_bottleneck_ns_per_task"], 6)
        stream["relay"]["cpu_ns"] = 80
        self.assertEqual(measure()["stream_dedicated_core_bottleneck_ns_per_task"], 8)
        stream["serializers"][1]["cpu_ns"] = None
        self.assertIsNone(measure()["stream_dedicated_core_bottleneck_ns_per_task"])
        self.assertEqual(measure()["serializer_cpu_ns_per_task"], [6, None])
        stream["serializers"][1]["cpu_ns"] = 20
        stream["counter_overflow"] = True
        self.assertIsNone(measure()["stream_aux_cpu_ns_per_task"])
        self.assertIsNone(measure()["stream_dedicated_core_bottleneck_ns_per_task"])
        stream["counter_overflow"] = False
        stream["serializers"][0]["counter_overflow"] = True
        self.assertIsNone(measure()["stream_dedicated_core_bottleneck_ns_per_task"])
        stream["serializers"][0]["counter_overflow"] = False
        stream["serializers"][0]["cpu_ns"] = True
        with self.assertRaises(ValueError):
            measure()
        stream["serializers"][0]["cpu_ns"] = 60
        stream["serializers"].pop()
        with self.assertRaises(ValueError):
            measure()
        with self.assertRaises(ValueError):
            scaling.coordination_costs({}, 0)

    def test_worker_execute_cpu_requires_complete_matching_job_coverage(self):
        import copy
        value = dict(clock="thread_cpu_time", scope=
                     "actual_worker_execute_including_unwind_excluding_queue_reply_and_post_job_telemetry",
                     supported=True, total_cpu_ns=900, calls=3, samples=3, errors=0, panics=0,
                     counter_overflow=False, complete=True)
        telemetry = {"workers": [{"jobs": 3, "execute_cpu": value}]}
        self.assertEqual(scaling.worker_execute_cost(telemetry, 3), 300)
        self.assertIsNone(scaling.worker_execute_cost({"workers": [{"jobs": 3}]}, 3))
        idle = dict(value, calls=0, samples=0, total_cpu_ns=0)
        telemetry["workers"].append({"jobs": 0, "execute_cpu": idle})
        self.assertEqual(scaling.worker_execute_cost(telemetry, 3), 300)
        for patch in ({"samples": 2, "complete": False}, {"supported": False, "complete": False},
                      {"counter_overflow": True, "complete": False}, {"total_cpu_ns": None, "complete": False},
                      {"errors": 1}, {"panics": 1}):
            changed = copy.deepcopy(telemetry)
            changed["workers"][0]["execute_cpu"].update(patch)
            self.assertIsNone(scaling.worker_execute_cost(changed, 3))
        for patch in ({"samples": 2}, {"calls": 2}, {"total_cpu_ns": True},
                      {"total_cpu_ns": 1 << 128}, {"errors": 3, "panics": 1}, {"scope": "lifetime_cpu"}):
            changed = copy.deepcopy(telemetry)
            changed["workers"][0]["execute_cpu"].update(patch)
            with self.assertRaises(ValueError):
                scaling.worker_execute_cost(changed, 3)
        with self.assertRaises(ValueError):
            scaling.worker_execute_cost(telemetry, 4)

    def write_run(self, directory, changed=False, workers=1, checkpoint=b"\x00\xffcheckpoint-v13\x80"):
        directory.mkdir()
        (directory / "stream.jsonl").write_text('complete stream\n' if not changed else 'changed stream\n')
        stream_hash = scaling.digest(directory / "stream.jsonl")
        data = {
            "result": {"status": "complete", "campaign_replay": True, "witness_replays": 2,
                       "executions": 10, "frames_emulated": 20, "search_seconds": 1,
                       "executions_per_second": 10, "stop_reason": "execution_limit", "stream_sha256": stream_hash,
                       "stream_bytes_generated": (directory / "stream.jsonl").stat().st_size},
            "campaign": {"stream_sha256": stream_hash, "executions_completed": 10},
            "telemetry": {"search_ns": 100, "workers": [{"jobs": 10, "busy_ns": 50}] + [{"jobs": 0, "busy_ns": 0}] * (workers - 1)},
            "identity": {"source": "fixed"},
        }
        for name, value in data.items():
            (directory / f"{name}.json").write_text(json.dumps(value))
        (directory / "checkpoint.bin").write_bytes(checkpoint)

    def write_expansion(self, directory, workers=1, resumed=False):
        self.write_run(directory, workers=workers)
        initial = dict(tasks_reserved=2 if resumed else 0, tasks_admitted=1 if resumed else 0,
                       edge_attempts=4 if resumed else 0, executed_edges=3 if resumed else 0,
                       budget_consumed=4 if resumed else 0, preparation_failures=1 if resumed else 0,
                       empty_tasks=0, bootstrap_work=3, reconstruction_work=2 if resumed else 0,
                       execution_work=10 if resumed else 0,
                       executions_to_first_objective=None, work_to_first_objective=None)
        final = dict(tasks_reserved=3, tasks_admitted=3, edge_attempts=9, executed_edges=7,
                     budget_consumed=10, preparation_failures=2, empty_tasks=1,
                     bootstrap_work=3, reconstruction_work=8, execution_work=31,
                     executions_to_first_objective=None, work_to_first_objective=None)
        deltas = {target: final[source] - initial[source]
                  for source, target in scaling.EXPANSION_FIELDS.items()}
        deltas["frames_emulated"] = sum(deltas[name] for name in
                                       ("bootstrap_frames", "reconstruction_frames", "edge_frames"))
        report = json.loads((directory / "campaign.json").read_text())
        report.update(header={"format": "dissonance-bounded-expansion-v2",
                              "schedule": "bounded_beam_fifo_tasks_v1", "window": 512,
                              "execution_budget": 10}, counters=final, initial_counters=initial)
        (directory / "campaign.json").write_text(json.dumps(report))
        result = json.loads((directory / "result.json").read_text())
        result.update(engine="bounded_expansion", window_semantics="fixed_outstanding_tasks",
                      initial_counters=initial, session_counter_deltas=deltas, executions=7,
                      frames_emulated=42, executions_per_second=deltas["executed_edges"],
                      rate_basis="session_counter_deltas_over_search_seconds",
                      rates_include_startup_and_finalization_wall=True)
        for source, target in scaling.EXPANSION_FIELDS.items():
            if source != "executed_edges":
                result[target] = final[source]
        (directory / "result.json").write_text(json.dumps(result))
        telemetry = json.loads((directory / "telemetry.json").read_text())
        telemetry["workers"][0]["jobs"] = deltas["tasks_admitted"]
        (directory / "telemetry.json").write_text(json.dumps(telemetry))

    def test_expansion_schema_uses_tasks_not_executed_edges_and_checked_resumed_deltas(self):
        with tempfile.TemporaryDirectory() as temporary:
            for resumed in (False, True):
                directory = Path(temporary) / str(resumed)
                self.write_expansion(directory, resumed=resumed)
                result, telemetry, _ = scaling.validate_result(directory, "bounded_expansion")
                report = scaling.read_json(directory / "campaign.json")
                deltas = scaling.expansion_deltas(result, report)
                self.assertEqual(sum(row["jobs"] for row in telemetry["workers"]), 2 if resumed else 3)
                self.assertEqual(deltas["executed_edges"], 4 if resumed else 7)
                self.assertEqual(deltas["reconstruction_frames"], 6 if resumed else 8)
                self.assertEqual(result["frames_emulated"], 42)
                costs = scaling.coordination_costs({"stream": {"worker_cpu_ns": 12}}, 3, "task")
                self.assertEqual(costs["stream_aux_cpu_ns_per_task"], 4)

    def test_expansion_rejects_wrong_shape_bad_deltas_and_edge_based_worker_counts(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "run"
            self.write_expansion(directory)
            result_path, report_path = directory / "result.json", directory / "campaign.json"
            original_result, original_report = scaling.read_json(result_path), scaling.read_json(report_path)
            plants = [lambda r, p: r.update(engine="linear"),
                      lambda r, p: r.update(window_semantics="per_worker"),
                      lambda r, p: p.pop("counters"),
                      lambda r, p: p["counters"].update(executed_edges=8),
                      lambda r, p: (p["initial_counters"].update(execution_work=99),
                                    r["initial_counters"].update(execution_work=99)),
                      lambda r, p: r["session_counter_deltas"].update(tasks_admitted=7),
                      lambda r, p: r["session_counter_deltas"].update(empty_tasks=True),
                      lambda r, p: r.update(frames_emulated=31),
                      lambda r, p: r.update(rate_basis="cumulative"),
                      lambda r, p: p["counters"].update(tasks_reserved=4)]
            for plant in plants:
                r, p = json.loads(json.dumps(original_result)), json.loads(json.dumps(original_report))
                plant(r, p)
                result_path.write_text(json.dumps(r)); report_path.write_text(json.dumps(p))
                with self.assertRaises((ValueError, KeyError)):
                    scaling.validate_result(directory, "bounded_expansion")
            result_path.write_text(json.dumps(original_result)); report_path.write_text(json.dumps(original_report))
            telemetry_path = directory / "telemetry.json"
            telemetry = scaling.read_json(telemetry_path)
            telemetry["workers"][0]["jobs"] = original_result["executions"]
            telemetry_path.write_text(json.dumps(telemetry))
            with self.assertRaisesRegex(ValueError, "worker telemetry"):
                scaling.validate_result(directory, "bounded_expansion")

    def test_counter_bounds_never_accept_boolean_negative_or_overflow(self):
        for value in (True, -1, 1 << 64, 1.0, None):
            with self.subTest(value=value), self.assertRaises(ValueError):
                scaling.counter(value)
        with self.assertRaises(ValueError):
            scaling.checked_sum([(1 << 64) - 1, 1])

    def test_expansion_fixed_absolute_window_allows_nondividing_worker_counts(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            evaluator = root / "evaluator"; evaluator.write_text("immutable executable")
            request = root / "request.json"
            request.write_text(json.dumps({"game": "famidash", "executions": 10}))
            observed = []
            def run(command, directory, timeout):
                configured = scaling.read_json(Path(command[1]))
                observed.append((configured["workers"], configured["window"]))
                self.write_expansion(Path(command[2]), workers=configured["workers"])
                return 1.0
            output = root / "evidence"
            args = ["qualify", "--evaluator", str(evaluator), "--request", str(request),
                    "--out", str(output), "--workers", "1", "3", "--window", "512", "--repeats", "1",
                    "--allow-oversubscription"]
            with patch.object(sys, "argv", args), patch.object(scaling, "run_trial", side_effect=run):
                scaling.main()
            self.assertEqual(observed, [(1, 512), (3, 512)])
            summary = scaling.read_json(output / "summary.json")
            self.assertEqual(summary["status"], "verified")
            self.assertEqual(summary["throughput_unit"], "tasks_per_second")
            self.assertEqual(summary["trials"][0]["throughput_per_second"], 3)
            self.assertEqual(summary["trials"][0]["executed_edges_per_second"], 7)
            self.assertEqual(summary["trials"][0]["coordination"]["unit"], "task")

    def test_checkpoint_identity_hashes_binary_bytes_and_requires_current_artifact(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "run"
            self.write_run(directory)
            _, _, original = scaling.validate_result(directory)
            checkpoint = directory / "checkpoint.bin"
            self.assertEqual(original["checkpoint"], scaling.digest(checkpoint))
            checkpoint.write_bytes(checkpoint.read_bytes() + b"\xfe")
            _, _, changed = scaling.validate_result(directory)
            self.assertNotEqual(original["checkpoint"], changed["checkpoint"])
            checkpoint.rename(directory / "checkpoint.json")
            with self.assertRaises(FileNotFoundError):
                scaling.validate_result(directory)

    def test_changed_binary_checkpoint_fails_trial_comparison(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            evaluator = root / "evaluator"
            evaluator.write_text("immutable executable")
            request = root / "request.json"
            request.write_text(json.dumps({"executions": 10}))

            def run(command, directory, timeout):
                count = json.loads(Path(command[1]).read_text())["workers"]
                self.write_run(Path(command[2]), workers=count, checkpoint=bytes([0, 255, count]))
                return 1.0

            output = root / "evidence"
            args = ["qualify", "--evaluator", str(evaluator), "--request", str(request),
                    "--out", str(output), "--workers", "1", "2", "--window", "512", "--repeats", "1",
                    "--allow-oversubscription"]
            with patch.object(sys, "argv", args), patch.object(scaling, "run_trial", side_effect=run):
                with self.assertRaisesRegex(ValueError, "logical work differs"):
                    scaling.main()
            self.assertEqual(json.loads((output / "summary.json").read_text())["status"], "failed")
            self.assertEqual((output / "r1-w2/run/checkpoint.bin").read_bytes(), bytes([0, 255, 2]))

    def test_validation_rejects_corruption_time_limits_and_missing_work(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary) / "run"
            self.write_run(directory)
            scaling.validate_result(directory)
            path = directory / "result.json"
            original = json.loads(path.read_text())
            for field, value in (("campaign_replay", False), ("stop_reason", "wall_limit"),
                                 ("witness_replays", 0), ("executions", 9), ("stream_sha256", "corrupt"),
                                 ("stream_bytes_generated", 0)):
                with self.subTest(field=field):
                    path.write_text(json.dumps(dict(original, **{field: value})))
                    with self.assertRaises(ValueError):
                        scaling.validate_result(directory)

    def test_fixed_window_and_divergent_trial_fail_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            evaluator = root / "evaluator"
            evaluator.write_text("immutable executable")
            request = root / "request.json"
            request.write_text(json.dumps({"executions": 10}))
            observed = []

            def run(command, directory, timeout):
                configured = json.loads(Path(command[1]).read_text())
                observed.append(configured["workers"] * configured["window"])
                self.write_run(Path(command[2]), changed=configured["workers"] == 2, workers=configured["workers"])
                return 1.0

            output = root / "evidence"
            args = ["qualify", "--evaluator", str(evaluator), "--request", str(request),
                    "--out", str(output), "--workers", "1", "2", "--window", "512", "--repeats", "1",
                    "--allow-oversubscription"]
            with patch.object(sys, "argv", args), patch.object(scaling, "run_trial", side_effect=run):
                with self.assertRaisesRegex(ValueError, "logical work differs"):
                    scaling.main()
            self.assertEqual(observed, [512, 512])
            self.assertEqual(json.loads((output / "summary.json").read_text())["status"], "failed")
            self.assertTrue((output / "r1-w2/run/stream.jsonl").exists())

    def test_success_keeps_one_reference_and_alternates_trial_order(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            evaluator = root / "evaluator"
            evaluator.write_text("immutable executable")
            request = root / "request.json"
            request.write_text(json.dumps({"executions": 10}))
            observed = []

            def run(command, directory, timeout):
                configured = json.loads(Path(command[1]).read_text())
                observed.append(configured["workers"])
                self.write_run(Path(command[2]), workers=configured["workers"])
                return 1.0

            output = root / "evidence"
            args = ["qualify", "--evaluator", str(evaluator), "--request", str(request),
                    "--out", str(output), "--workers", "1", "2", "--window", "512", "--repeats", "2",
                    "--allow-oversubscription"]
            with patch.object(sys, "argv", args), patch.object(scaling, "run_trial", side_effect=run):
                scaling.main()
            self.assertEqual(observed, [1, 2, 2, 1])
            summary = json.loads((output / "summary.json").read_text())
            self.assertEqual(summary["status"], "verified")
            self.assertEqual(summary["rates"]["2"]["speedup"], 1)

    def test_failed_evaluator_retains_log(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            with self.assertRaisesRegex(ValueError, "exited 3"):
                scaling.run_trial([sys.executable, "-c", "print('failure evidence'); raise SystemExit(3)"], directory, 5)
            self.assertIn("failure evidence", (directory / "execution.log").read_text())


if __name__ == "__main__":
    unittest.main()
