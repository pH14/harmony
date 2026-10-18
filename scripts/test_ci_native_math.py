# SPDX-License-Identifier: AGPL-3.0-or-later
import copy
import errno
import importlib.util
import itertools
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location("native_math", Path(__file__).with_name("qualify-native-math.py"))
Q = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(Q)


class NativeMathTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.bundle = self.root / "bundle"
        self.bundle.mkdir()
        self.rows = []
        for op, (inputs, outputs) in Q.OPS.items():
            self.rows.append({"op": op, "round": 0, "flush": 0, "input": "0" * inputs,
                              "result": "0" * outputs, "x87_status": 0, "x87_control": 0x037f,
                              "mxcsr": 0x1f80, "errno": 0})
        self.rows.insert(1, {**self.rows[0], "input": "0000000000000001"})
        self.data = b"".join(Q.canonical(row) for row in self.rows)
        for name, data in (("native-math", b"exact-executable"), ("corpus.c", b"source"),
                           ("link.map", b"libm"), ("build-observations.jsonl", self.data)):
            (self.bundle / name).write_bytes(data)
        self.manifest = {"format": Q.FORMAT, "scope": "linux-x86_64-native-math-v1",
                         "execution_environment": {},
                         "qualifier_sha256": Q.digest(Q.SCRIPT.read_bytes()), "source_revision": "a" * 40,
                         "binary_sha256": Q.digest(b"exact-executable"), "source_sha256": Q.digest(b"source"),
                         "link_map_sha256": Q.digest(b"libm"), "build_observations_sha256": Q.digest(self.data),
                         "inventory": [Q.key(row) for row in self.rows], "compiler": {"sha256": "b" * 64},
                         "command": ["gcc", "-static"], "libraries": {"libm.a": "c" * 64},
                         "tools": {"ld": "d" * 64}, "libc_version": "glibc test"}
        (self.bundle / "manifest.json").write_bytes(Q.canonical(self.manifest))
        self.receipts = [self.receipt(1, "AuthenticAMD"), self.receipt(2, "GenuineIntel")]

    def receipt(self, number, vendor):
        directory = self.root / str(number)
        directory.mkdir()
        cpuinfo = f"processor: 3\nvendor_id: {vendor}\ncpu family: 6\nmodel: {number}\nmodel name: test\nstepping: 1\nmicrocode: 0x1\nflags: sse sse2\n\n"
        (directory / "cpuinfo.txt").write_text(cpuinfo)
        receipt = {"format": Q.FORMAT, "manifest_sha256": Q.digest((self.bundle / "manifest.json").read_bytes()),
                   "binary_sha256": self.manifest["binary_sha256"], "repeats": 3,
                   "build": Q.build_identity(self.manifest), "host_id": f"00000000-0000-0000-0000-{number:012d}",
                   "execution_environment": {},
                   "cpu": 3, "cpu_identity": Q.selected_cpu(cpuinfo, 3), "kernel": "test",
                   "case_count": len(self.rows), "observations_sha256": Q.digest(self.data)}
        (directory / "receipt.json").write_bytes(Q.canonical(receipt))
        for repeat in range(3):
            (directory / f"observations-{repeat}.jsonl").write_bytes(self.data)
        return directory / "receipt.json"

    def mutate_receipt(self, index, change):
        path = self.receipts[index]
        value = Q.load(path)
        change(value)
        path.write_bytes(Q.canonical(value))

    def replace_rows(self, rows):
        data = b"".join(Q.canonical(row) for row in rows)
        for repeat in range(3):
            (self.receipts[1].parent / f"observations-{repeat}.jsonl").write_bytes(data)
        self.mutate_receipt(1, lambda value: value.update(observations_sha256=Q.digest(data)))

    def test_exact_pair_is_bounded_observation(self):
        report = Q.compare(self.bundle, self.receipts)
        self.assertEqual(report["pairs"][0]["verdict"], "observed-exact")
        self.assertEqual(report["pairs"][0]["differences"], [])
        self.assertEqual(len(report["pairs"][0]["groups"]), len(Q.OPS))

    def test_planted_changed_result_is_retained_not_qualified(self):
        rows = copy.deepcopy(self.rows)
        rows[0]["result"] = "01" + rows[0]["result"][2:]
        self.replace_rows(rows)
        pair = Q.compare(self.bundle, self.receipts)["pairs"][0]
        self.assertEqual(pair["verdict"], "observed-differences")
        self.assertEqual(len(pair["differences"]), 1)
        self.assertEqual(pair["differences"][0]["right"], rows[0])
        self.assertEqual(pair["groups"]["libm.sin/round=0/flush=0"]["different"], 1)
        self.assertEqual(pair["groups"]["libm.cos/round=0/flush=0"]["different"], 0)

    def test_planted_exception_difference_is_retained(self):
        rows = copy.deepcopy(self.rows)
        rows[0]["mxcsr"] |= 0x20
        self.replace_rows(rows)
        self.assertEqual(Q.compare(self.bundle, self.receipts)["pairs"][0]["verdict"], "observed-differences")

    def test_missing_case_is_incomplete_even_when_digest_rebound(self):
        self.replace_rows([self.rows[0], *self.rows[2:]])
        with self.assertRaisesRegex(ValueError, "inventory mismatch"):
            Q.compare(self.bundle, self.receipts)

    def test_wrong_executable_is_incomplete(self):
        self.mutate_receipt(1, lambda value: value.update(binary_sha256="0" * 64))
        with self.assertRaisesRegex(ValueError, "retained executable"):
            Q.compare(self.bundle, self.receipts)

    def test_build_library_identity_cannot_be_rebound(self):
        self.mutate_receipt(1, lambda value: value["build"]["libraries"].update({"libm.a": "0" * 64}))
        with self.assertRaisesRegex(ValueError, "retained executable"):
            Q.compare(self.bundle, self.receipts)

    def test_execution_environment_cannot_be_rebound(self):
        self.mutate_receipt(1, lambda value: value.update(execution_environment={"GLIBC_TUNABLES": "glibc.cpu.hwcaps=-AVX2"}))
        with self.assertRaisesRegex(ValueError, "retained executable"):
            Q.compare(self.bundle, self.receipts)

    def test_nonrepeatable_receipt_is_incomplete(self):
        (self.receipts[1].parent / "observations-2.jsonl").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "repeat mismatch"):
            Q.compare(self.bundle, self.receipts)

    def test_repeated_host_is_incomplete(self):
        host = Q.load(self.receipts[0])["host_id"]
        self.mutate_receipt(1, lambda value: value.update(host_id=host))
        with self.assertRaisesRegex(ValueError, "distinct firmware"):
            Q.compare(self.bundle, self.receipts)

    def test_missing_vendor_is_incomplete(self):
        with self.assertRaisesRegex(ValueError, "AMD/Intel pair"):
            Q.compare(self.bundle, [self.receipts[0], self.receipt(3, "AuthenticAMD")])

    def test_cpu_metadata_must_match_pinned_cpu(self):
        self.mutate_receipt(1, lambda value: value["cpu_identity"].update(model="999"))
        with self.assertRaisesRegex(ValueError, "CPU metadata"):
            Q.compare(self.bundle, self.receipts)

    def test_ambient_control_mutation_is_incomplete(self):
        rows = copy.deepcopy(self.rows)
        rows[0]["mxcsr"] ^= 0x8000
        with self.assertRaisesRegex(ValueError, "MXCSR control"):
            Q.records(b"".join(Q.canonical(row) for row in rows))

    def test_workflow_builds_once_distributes_and_compares_all_receipts(self):
        import yaml

        jobs = yaml.safe_load((Q.ROOT / ".github/workflows/consonance-native-math-qualification.yml").read_text())["jobs"]
        setup = next(step for step in jobs["artifact"]["steps"] if step.get("uses") == "actions/setup-python@v5")
        self.assertEqual(setup["with"]["python-version"], "3.12")
        self.assertEqual(jobs["observe"]["needs"], "artifact")
        self.assertEqual(jobs["observe"]["strategy"]["matrix"]["replica"], [1, 2, 3, 4, 5, 6])
        self.assertEqual(jobs["results"]["needs"], ["artifact", "observe"])
        observe = "\n".join(step.get("run", "") for step in jobs["observe"]["steps"])
        self.assertNotIn(" bundle ", observe)
        self.assertIn(" run native-math-bundle", observe)
        self.assertIn("--host-id-file", observe)
        compare = "\n".join(step.get("run", "") for step in jobs["results"]["steps"])
        self.assertIn(" compare native-math-bundle", compare)
        self.assertIn("native-math-replicas/*/receipt.json", compare)

    def test_duplicate_record_and_duplicate_json_field_are_incomplete(self):
        with self.assertRaisesRegex(ValueError, "duplicate arithmetic"):
            Q.records(self.data + Q.canonical(self.rows[0]))
        with self.assertRaisesRegex(ValueError, "duplicate JSON"):
            Q.records(b'{"op":"x","op":"y"}\n')


@unittest.skipUnless(platform.system() == "Linux" and platform.machine() == "x86_64" and shutil.which("gcc"), "native Linux x86_64 corpus")
class NativeCorpusTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.bundle = self.root / "bundle"
        Q.bundle(self.bundle, "gcc", "a" * 40)
        self.host = self.root / "host-id"
        self.host.write_text("00000000-0000-0000-0000-000000000001")

    def test_native_corpus_modes_inputs_and_three_repeats(self):
        output = self.root / "report"
        Q.run(self.bundle, output, self.host)
        receipt = Q.load(output / "receipt.json")
        rows = Q.records((output / "observations-0.jsonl").read_bytes())
        self.assertEqual(receipt["case_count"], 10944)
        self.assertEqual({(row["round"], row["flush"]) for row in rows}, set(itertools.product(range(4), range(2))))
        exact = next(row for row in rows if Q.key(row) == ["libm.sqrt", 0, 0, "3ff0000000000000"])
        self.assertEqual(exact["result"], "000000000000f03f")
        self.assertEqual(receipt["repeats"], 3)
        observations = {tuple(Q.key(row)): row for row in rows}
        for observation in rows:
            if observation["op"].startswith("x87."):
                self.assertEqual((observation["x87_status"] >> 11) & 7, 7)

        def row(op, bits, rounding=0, flush=0):
            return observations[(op, rounding, flush, bits)]

        for op in ("x87.fsin", "x87.fcos"):
            for bits, extended in (("43e0000000000000", "00000000000000803e40"),
                                   ("7fefffffffffffff", "00f8fffffffffffffe43")):
                self.assertTrue(row(op, bits)["x87_status"] & 0x0400)
                self.assertEqual(row(op, bits)["result"], extended)
        self.assertTrue(row("x87.fsin", "7ff0000000000000")["x87_status"] & 1)
        invalid = row("sse.sqrtps", "bf800000")
        self.assertTrue(invalid["mxcsr"] & 1)
        self.assertEqual(invalid["result"], "0000c0ff" * 4)
        zero = row("sse.divps", "00000000")
        self.assertTrue(zero["mxcsr"] & 4)
        self.assertEqual(zero["result"], "0000807f" * 4)
        self.assertEqual(row("sse.sqrtps", "00000001", flush=1)["result"], "00000000" * 4)
        self.assertNotEqual(row("sse.sqrtps", "00000001")["result"], "00000000" * 4)
        self.assertEqual(row("sse.divps", "3f7fffff", rounding=1)["result"], "0000803f" * 4)
        self.assertEqual(row("sse.divps", "3f7fffff", rounding=2)["result"], "0100803f" * 4)
        self.assertEqual(row("libm.log", "0000000000000000")["errno"], errno.ERANGE)
        self.assertEqual(row("libm.log", "bff0000000000000")["errno"], errno.EDOM)

    def test_failed_process_retains_output_without_complete_receipt(self):
        output = self.root / "failed"
        failure = subprocess.CompletedProcess([], 1, b"partial witness\n", b"injected failure\n")
        with mock.patch.dict(Q.os.environ, {"GLIBC_TUNABLES": "glibc.cpu.hwcaps=-AVX2"}):
            with mock.patch.object(Q.subprocess, "run", return_value=failure) as process:
                with self.assertRaises(subprocess.CalledProcessError):
                    Q.run(self.bundle, output, self.host)
        self.assertEqual(process.call_args.kwargs["env"], {})
        self.assertEqual((output / "observations-0.jsonl").read_bytes(), b"partial witness\n")
        self.assertFalse((output / "receipt.json").exists())


if __name__ == "__main__":
    unittest.main()
