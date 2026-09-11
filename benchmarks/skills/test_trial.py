# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import base64
import hashlib
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

try:
    from . import build, materials, sandbox, trial
except ImportError:  # unittest discovery can load this directory as top-level.
    import build  # type: ignore[no-redef]
    import materials  # type: ignore[no-redef]
    import sandbox  # type: ignore[no-redef]
    import trial  # type: ignore[no-redef]


IMAGE = "sha256:" + "a" * 64
OTHER_IMAGE = "sha256:" + "b" * 64
MAX_SOURCE_BYTES = 1024


def _limits(*, toolcalls: int = 3) -> sandbox.Limits:
    return sandbox.Limits(
        wall=10.0,
        toolcalls=toolcalls,
        memorybytes=32 * 1024 * 1024,
        workbytes=8 * 1024 * 1024,
        cpus=0.5,
        pids=32,
        outputbytes=256 * 1024,
    )


def _result(
    exit_code: int | None = 0,
    stdout: bytes = b"",
    stderr: bytes = b"",
    termination: str = "exited",
) -> sandbox.RunResult:
    return sandbox.RunResult(exit_code, stdout, stderr, termination)


class PairFixture:
    def __init__(
        self,
        root: Path,
        *,
        arm: str = "docs",
        toolcalls: int = 3,
        trial_settings: dict[str, object] | None = None,
    ) -> None:
        self.root = root
        root.mkdir(parents=True, exist_ok=False)
        self.arm = arm
        self.limits = _limits(toolcalls=toolcalls)
        self.outputs = ["main.c", "guide.txt"]
        self.source_data = {
            "main.c": b"int main(void) { return 0; }\n",
            "guide.txt": b"controller supplied guide\n",
        }
        source = root / "main.c"
        source.write_bytes(self.source_data["main.c"])
        guide = root / "guide.txt"
        guide.write_bytes(self.source_data["guide.txt"])
        rule = root / "rule.txt"
        rule_data = b"treatment rule\n"
        rule.write_bytes(rule_data)
        self.pair = root / "pair"
        trial_settings_value = {
            "image": IMAGE,
            "limits": vars(self.limits).copy(),
            "outputs": list(self.outputs),
            "max_source_bytes": MAX_SOURCE_BYTES,
        }
        if trial_settings is not None:
            trial_settings_value.update(trial_settings)
        self.settings = {
            "trial": trial_settings_value,
            "fixture_metadata": {"purpose": "portable trial test"},
        }
        materials.freeze_pair(
            self.pair,
            {"main.c": source, "guide.txt": guide},
            {"rule.txt": rule},
            "Run the frozen trial fixture.",
            self.settings,
        )
        self.manifest = hashlib.sha256((self.pair / "manifest.json").read_bytes()).hexdigest()


def _collector_payload(source_data: dict[str, bytes]) -> bytes:
    records = [
        {
            "path": path,
            "data": base64.b64encode(data).decode("ascii"),
            "executable": False,
        }
        for path, data in source_data.items()
    ]
    return json.dumps(
        {"version": 1, "artifacts": records},
        ensure_ascii=True,
        separators=(",", ":"),
    ).encode("ascii")


class FakeSandbox:
    def __init__(
        self,
        source_data: dict[str, bytes],
        *,
        agent_result: sandbox.RunResult | None = None,
        collector_result: sandbox.RunResult | None = None,
        stage_error: BaseException | None = None,
        exit_error: BaseException | None = None,
        run_error: BaseException | None = None,
        agent_results: tuple[sandbox.RunResult, ...] | None = None,
        mutate_after_agent: bool = False,
    ) -> None:
        self.source_data = source_data
        self.agent_result = agent_result or _result(0, b"agent result\n")
        self.collector_result = collector_result or _result(0, _collector_payload(source_data))
        self.stage_error = stage_error
        self.exit_error = exit_error
        self.run_error = run_error
        self.agent_results = agent_results
        self.agent_run_count = 0
        self.mutate_after_agent = mutate_after_agent
        self.entered = False
        self.exited = False
        self.staged: tuple[Path, str, str] | None = None
        self.commands: list[tuple[str, ...]] = []
        self.events: list[str] = []

    def __enter__(self) -> "FakeSandbox":
        self.entered = True
        self.events.append("enter")
        return self

    def __exit__(self, *_: object) -> bool:
        self.exited = True
        self.events.append("exit")
        if self.exit_error is not None:
            raise self.exit_error
        return False

    def stage(self, pair: Path, arm: str, manifest_sha256: str) -> None:
        self.events.append("stage")
        if self.stage_error is not None:
            raise self.stage_error
        self.staged = (pair, arm, manifest_sha256)

    def run(self, argv: list[str]) -> sandbox.RunResult:
        sandbox._validate_argv(argv)
        self.commands.append(tuple(argv))
        if argv[0] != "/usr/bin/python3":
            if self.run_error is not None:
                raise self.run_error
            if self.mutate_after_agent:
                assert self.staged is not None
                source = self.staged[0] / self.staged[1] / "main.c"
                source.chmod(0o644)
                source.write_bytes(source.read_bytes() + b"tampered\n")
                source.chmod(0o444)
            if self.agent_results is not None:
                if self.agent_run_count >= len(self.agent_results):
                    raise AssertionError("fake agent result sequence exhausted")
                result = self.agent_results[self.agent_run_count]
                self.agent_run_count += 1
                return result
            return self.agent_result
        return self.collector_result


class SandboxFactory:
    def __init__(self, **kwargs: object) -> None:
        self.kwargs = kwargs
        self.instances: list[FakeSandbox] = []

    def __call__(self, image: str, limits: sandbox.Limits) -> FakeSandbox:
        self.instances.append(FakeSandbox(**self.kwargs))
        return self.instances[-1]


class TrialSessionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _session(
        self,
        fixture: PairFixture,
        factory: SandboxFactory,
        *,
        manifest: str | None = None,
        image: str = IMAGE,
        outputs: list[str] | None = None,
        limits: sandbox.Limits | None = None,
        maximum: int = MAX_SOURCE_BYTES,
    ) -> trial.TrialSession:
        return trial.TrialSession(
            fixture.pair,
            fixture.manifest if manifest is None else manifest,
            fixture.arm,
            image,
            fixture.limits if limits is None else limits,
            fixture.outputs if outputs is None else outputs,
            maximum,
        )

    def test_docs_and_skills_select_the_requested_arm_and_collect_exact_sources(self) -> None:
        for arm in ("docs", "skills"):
            with self.subTest(arm=arm):
                fixture = PairFixture(self.root / arm, arm=arm)
                factory = SandboxFactory(source_data=fixture.source_data)
                with mock.patch.object(trial.sandbox, "Sandbox", side_effect=factory):
                    with self._session(fixture, factory) as session:
                        agent_argv = ["/bin/echo", "agent-controlled-token"]
                        agent_result = session.run(agent_argv)
                        submission = session.finish()

                self.assertEqual(agent_result, factory.instances[0].agent_result)
                isolated = factory.instances[0]
                self.assertTrue(isolated.entered)
                self.assertTrue(isolated.exited)
                self.assertEqual(isolated.staged, (fixture.pair, arm, fixture.manifest))
                self.assertEqual(isolated.commands[0], tuple(agent_argv))
                self.assertGreaterEqual(len(isolated.commands), 2)
                collector = isolated.commands[-1]
                self.assertEqual(collector[0], "/usr/bin/python3")
                self.assertNotIn("agent-controlled-token", collector)
                self.assertEqual(submission.manifest_sha256, fixture.manifest)
                self.assertEqual(submission.arm, arm)
                self.assertEqual(submission.image, IMAGE)
                self.assertEqual(submission.agent_toolcalls, 1)
                self.assertEqual(submission.commands, (tuple(agent_argv),))
                self.assertEqual(submission.results, (agent_result,))
                self.assertEqual(
                    [(source.path, source.data, source.sha256) for source in submission.sources],
                    [
                        (path, data, hashlib.sha256(data).hexdigest())
                        for path, data in fixture.source_data.items()
                    ],
                )

    def test_collector_is_reserved_and_rejected_agent_requests_are_counted(self) -> None:
        fixture = PairFixture(self.root / "budget", toolcalls=2)
        factory = SandboxFactory(source_data=fixture.source_data)
        with mock.patch.object(trial.sandbox, "Sandbox", side_effect=factory):
            with self._session(fixture, factory) as session:
                first = session.run(["/bin/echo", "first"])
                with self.assertRaises(sandbox.SandboxError):
                    session.run(["/bin/echo", "second"])
                submission = session.finish()

        isolated = factory.instances[0]
        self.assertEqual(first, isolated.agent_result)
        self.assertEqual(len(isolated.commands), 2)
        self.assertEqual(submission.agent_toolcalls, 2)
        self.assertEqual(submission.commands, (("/bin/echo", "first"),))
        self.assertEqual(submission.results, (first,))

        invalid_fixture = PairFixture(self.root / "invalid-request", toolcalls=2)
        invalid_factory = SandboxFactory(source_data=invalid_fixture.source_data)
        with mock.patch.object(trial.sandbox, "Sandbox", side_effect=invalid_factory):
            with self._session(invalid_fixture, invalid_factory) as session:
                with self.assertRaises(ValueError):
                    session.run([])
                submission = session.finish()
        self.assertEqual(submission.agent_toolcalls, 1)
        self.assertEqual(submission.commands, ())
        self.assertEqual(submission.results, ())

    def test_terminal_session_rejects_run_and_finish(self) -> None:
        fixture = PairFixture(self.root / "terminal")
        factory = SandboxFactory(source_data=fixture.source_data)
        with mock.patch.object(trial.sandbox, "Sandbox", side_effect=factory):
            with self._session(fixture, factory) as session:
                session.run(["/bin/true"])
                session.finish()
                with self.assertRaises(sandbox.SandboxError):
                    session.run(["/bin/true"])
                with self.assertRaises(sandbox.SandboxError):
                    session.finish()

            closed_fixture = PairFixture(self.root / "explicit-close")
            with self._session(closed_fixture, factory) as session:
                session.close()
                with self.assertRaises(sandbox.SandboxError):
                    session.run(["/bin/true"])
                with self.assertRaises(sandbox.SandboxError):
                    session.finish()

    def test_constructor_validates_pin_settings_and_source_paths_before_sandbox(self) -> None:
        fixture = PairFixture(self.root / "constructor")
        factory = SandboxFactory(source_data=fixture.source_data)
        with mock.patch.object(trial.sandbox, "Sandbox", side_effect=factory):
            with self.assertRaises(sandbox.SandboxError):
                self._session(fixture, factory, manifest="0" * 64)
        self.assertEqual(factory.instances, [])

        bad_settings = PairFixture(
            self.root / "bad-settings",
            trial_settings={"image": OTHER_IMAGE},
        )
        with self.assertRaises(ValueError):
            self._session(bad_settings, factory)
        self.assertEqual(factory.instances, [])

        with self.assertRaises(ValueError):
            self._session(fixture, factory, outputs=["../main.c"])
        self.assertEqual(factory.instances, [])

    def test_reserved_output_paths_are_rejected_for_both_arms(self) -> None:
        for arm in ("docs", "skills"):
            with self.subTest(arm=arm):
                fixture = PairFixture(self.root / f"reserved-{arm}", arm=arm)
                factory = SandboxFactory(source_data=fixture.source_data)
                with mock.patch.object(trial.sandbox, "Sandbox", side_effect=factory):
                    for path in ("TASK.txt", "nested/SETTINGS.json", ".agent/skills/rule.txt"):
                        with self.subTest(path=path):
                            with self.assertRaises(ValueError):
                                self._session(fixture, factory, outputs=[path])
                self.assertEqual(factory.instances, [])

    def test_repinned_valid_pair_with_settings_mismatch_is_rejected_before_sandbox(self) -> None:
        fixture = PairFixture(
            self.root / "repinned-settings",
            trial_settings={"image": OTHER_IMAGE},
        )
        # The modified settings are part of a freshly generated, internally
        # consistent manifest; only the requested trial configuration differs.
        materials.verify_pair(fixture.pair)
        self.assertEqual(
            fixture.manifest,
            hashlib.sha256((fixture.pair / "manifest.json").read_bytes()).hexdigest(),
        )
        factory = SandboxFactory(source_data=fixture.source_data)
        with mock.patch.object(trial.sandbox, "Sandbox", side_effect=factory):
            with self.assertRaises(ValueError):
                self._session(fixture, factory, image=IMAGE)
        self.assertEqual(factory.instances, [])

    def test_stage_and_collector_failures_propagate_without_submission(self) -> None:
        fixture = PairFixture(self.root / "stage-error")
        stage_factory = SandboxFactory(
            source_data=fixture.source_data,
            stage_error=sandbox.SandboxError("stage failed"),
        )
        with mock.patch.object(trial.sandbox, "Sandbox", side_effect=stage_factory):
            with self.assertRaises(sandbox.SandboxError):
                with self._session(fixture, stage_factory):
                    pass
        self.assertTrue(stage_factory.instances[0].exited)
        self.assertEqual(stage_factory.instances[0].commands, [])

        fixture = PairFixture(self.root / "collector-error")
        collector_factory = SandboxFactory(
            source_data=fixture.source_data,
            collector_result=_result(1, b"", b"collector failed"),
        )
        with mock.patch.object(trial.sandbox, "Sandbox", side_effect=collector_factory):
            with self.assertRaises(build.BuildError):
                with self._session(fixture, collector_factory) as session:
                    session.run(["/bin/true"])
                    session.finish()
        self.assertTrue(collector_factory.instances[0].exited)

        fixture = PairFixture(self.root / "cleanup-error")
        cleanup_factory = SandboxFactory(
            source_data=fixture.source_data,
            exit_error=sandbox.SandboxError("cleanup failed"),
        )
        with mock.patch.object(trial.sandbox, "Sandbox", side_effect=cleanup_factory):
            with self.assertRaises(sandbox.SandboxError):
                with self._session(fixture, cleanup_factory) as session:
                    session.run(["/bin/true"])
                    session.finish()
        self.assertTrue(cleanup_factory.instances[0].exited)

    def test_pair_mutation_after_agent_run_is_detected_after_cleanup(self) -> None:
        fixture = PairFixture(self.root / "mutation")
        factory = SandboxFactory(source_data=fixture.source_data, mutate_after_agent=True)
        with mock.patch.object(trial.sandbox, "Sandbox", side_effect=factory):
            with self.assertRaises(sandbox.SandboxError):
                with self._session(fixture, factory) as session:
                    session.run(["/bin/true"])
                    session.finish()
        self.assertTrue(factory.instances[0].exited)

    def test_agent_hard_limits_make_finish_fail_and_close(self) -> None:
        for termination in ("timeout", "output_limit"):
            with self.subTest(termination=termination):
                fixture = PairFixture(self.root / f"hard-{termination}")
                factory = SandboxFactory(
                    source_data=fixture.source_data,
                    agent_result=_result(None, termination=termination),
                )
                with mock.patch.object(trial.sandbox, "Sandbox", side_effect=factory):
                    with self._session(fixture, factory) as session:
                        self.assertEqual(session.run(["/bin/true"]).termination, termination)
                        with self.assertRaises(sandbox.SandboxError):
                            session.finish()
                isolated = factory.instances[0]
                self.assertTrue(isolated.exited)
                self.assertEqual(len(isolated.commands), 1)

    def test_agent_sandbox_failure_cannot_produce_submission(self) -> None:
        fixture = PairFixture(self.root / "agent-failure")
        factory = SandboxFactory(
            source_data=fixture.source_data,
            run_error=sandbox.SandboxError("agent dispatch failed"),
        )
        with mock.patch.object(trial.sandbox, "Sandbox", side_effect=factory):
            with self._session(fixture, factory) as session:
                with self.assertRaises(sandbox.SandboxError):
                    session.run(["/bin/true"])
                with self.assertRaises(sandbox.SandboxError):
                    session.finish()
        self.assertTrue(factory.instances[0].exited)

    def test_ordinary_nonzero_and_signaled_results_remain_recoverable(self) -> None:
        fixture = PairFixture(self.root / "ordinary-agent-failures", toolcalls=4)
        results = (
            _result(9, b"compile failed\n"),
            _result(-15, b"", b"interrupted\n", "signaled"),
            _result(0, b"corrected\n"),
        )
        factory = SandboxFactory(source_data=fixture.source_data, agent_results=results)
        commands = (
            ("/bin/false", "first"),
            ("/bin/kill", "second"),
            ("/bin/true", "fixed"),
        )
        with mock.patch.object(trial.sandbox, "Sandbox", side_effect=factory):
            with self._session(fixture, factory) as session:
                observed = tuple(session.run(list(command)) for command in commands)
                submission = session.finish()

        self.assertEqual(observed, results)
        self.assertEqual(submission.results, results)
        self.assertEqual(submission.commands, commands)
        self.assertEqual(submission.agent_toolcalls, 3)
        self.assertTrue(factory.instances[0].exited)

    def test_each_session_uses_a_fresh_sandbox_and_closes_it(self) -> None:
        fixture_one = PairFixture(self.root / "one")
        fixture_two = PairFixture(self.root / "two")
        factory = SandboxFactory(source_data=fixture_one.source_data)
        with mock.patch.object(trial.sandbox, "Sandbox", side_effect=factory):
            for fixture in (fixture_one, fixture_two):
                with self._session(fixture, factory) as session:
                    session.run(["/bin/true"])
                    session.finish()
        self.assertEqual(len(factory.instances), 2)
        self.assertIsNot(factory.instances[0], factory.instances[1])
        self.assertEqual(
            [instance.staged[1] for instance in factory.instances if instance.staged],
            ["docs", "docs"],
        )
        self.assertTrue(all(instance.exited for instance in factory.instances))


if __name__ == "__main__":
    unittest.main()
