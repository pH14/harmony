#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""The registry describes CI that exists, owns every check, and stays consistent."""

import json
import importlib.util
import tempfile
import tomllib
import re
import os
import subprocess
import unittest
from pathlib import Path
from unittest import mock

import ci_contract
import miri_scope
from ci_scope import SCENARIOS


ROOT = ci_contract.ROOT


class StructureTests(unittest.TestCase):
    def test_uml_qualifier_artifact_is_static_musl_on_both_architectures(self):
        import yaml

        jobs = yaml.safe_load((ROOT / ci_contract.CONSONANCE_UML.path).read_text())["jobs"]
        build = next(step for step in jobs["artifacts"]["steps"]
                     if step.get("name") == "Build the qualification binary")
        self.assertEqual(build["env"]["CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER"],
                         "musl-gcc")
        self.assertIn("--target ${{ matrix.architecture }}-unknown-linux-musl", build["run"])
        self.assertIn("target/${{ matrix.architecture }}-unknown-linux-musl/release/harmony-uml-qualify",
                      build["run"])

    def test_uml_launcher_refuses_dynamic_loaded_code_once(self):
        import yaml

        jobs = yaml.safe_load((ROOT / ci_contract.CONSONANCE_UML.path).read_text())["jobs"]
        runs = [(job, step.get("run", "")) for job, details in jobs.items()
                for step in details.get("steps", [])
                if "scripts/test-loaded-code-identity.py" in step.get("run", "")]
        self.assertEqual(runs, [("launcher", "python3 scripts/test-loaded-code-identity.py")])

    def test_reviewed_uml_live_cases_use_sealed_musl_tests(self):
        import yaml

        jobs = yaml.safe_load((ROOT / ci_contract.CONSONANCE_UML.path).read_text())["jobs"]
        artifacts = jobs["artifacts"]
        steps = {step.get("name"): step for step in artifacts["steps"]}
        panic = steps["Report guest panics and distinguish setup host failures"]
        for architecture in ("X86_64", "AARCH64"):
            self.assertEqual(
                panic["env"][f"CARGO_TARGET_{architecture}_UNKNOWN_LINUX_MUSL_RUNNER"],
                "${{ github.workspace }}/scripts/linux-sealed-test-runner.py",
            )
        self.assertIn("--target ${{ matrix.architecture }}-unknown-linux-musl", panic["run"])
        self.assertIn("reviewed_kernel_panic_is_a_public_client_crash", panic["run"])
        self.assertIn("setup_terminal_status_preserves_host_provenance_and_guest_controls", panic["run"])
        self.assertIn("setup terminal provenance: signals/87 infrastructure; 86/0 guest; cleanup empty", panic["run"])
        self.assertIn("sealed_worker_sigkill_is_infrastructure_and_guest_stops_are_framed", panic["run"])
        self.assertIn("worker transport provenance: SIGKILL infrastructure; framed guest panic/shutdown preserved", panic["run"])
        fsgsbase = steps["Check native FS and GS bases across reused and fresh restores"]
        self.assertEqual(fsgsbase["if"], "matrix.architecture == 'x86_64'")
        self.assertEqual(
            fsgsbase["env"]["CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUNNER"],
            "${{ github.workspace }}/scripts/linux-sealed-test-runner.py",
        )
        self.assertIn("--target x86_64-unknown-linux-musl", fsgsbase["run"])
        self.assertIn("direct_fs_gs_bases_survive_reused_and_fresh_restore", fsgsbase["run"])
        for step in (panic, fsgsbase):
            self.assertIn("grep -Fxq 'running 1 test'", step["run"])
            self.assertIn("grep -Eq 'test result: ok\\. 1 passed;'", step["run"])

    def test_linux_strict_host_exercises_public_apis_from_sealed_musl(self):
        import yaml

        registered = next(job for job in ci_contract.HARMONY_CHECKS.jobs
                          if job.name == "Consonance Host Public API")
        self.assertEqual((registered.trigger, registered.timeout_minutes, registered.select),
                         ("pr", 15, "rust_checks"))
        jobs = yaml.safe_load((ROOT / ci_contract.HARMONY_CHECKS.path).read_text())["jobs"]
        job = jobs["consonance-public-api"]
        self.assertEqual(job["name"], registered.name)
        self.assertEqual(job["timeout-minutes"], registered.timeout_minutes)
        self.assertEqual(job["needs"], "scope")
        self.assertEqual(job["if"], "needs.scope.outputs.rust_checks == 'true'")
        step = next(step for step in job["steps"]
                    if step.get("name") == "Exercise public APIs inside a sealed static host")
        self.assertEqual(step["env"]["CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUNNER"],
                         "${{ github.workspace }}/scripts/linux-sealed-test-runner.py")
        commands = step["run"]
        for expected in (
            "-p consonance-client --features in-process,uml --lib",
            "-p host-code-identity --lib",
            "-p uml --lib",
            "grep -Fxq 'running 1 test'",
            "grep -Eq 'test result: ok\\. 1 passed;'",
            "client_test session::tests::sealed_host_admits_public_session_identity",
            "client_test session::worker::tests::sealed_host_reexecs_a_public_worker",
            "client_test session::uml::tests::clean_guest_exit_remains_a_terminal_shutdown",
            "client_test session::uml::tests::host_signal_abandons_instead_of_reporting_guest_shutdown",
            "client_test session::uml::tests::host_clone_status_abandons_instead_of_reporting_guest_shutdown",
            "client_test session::uml::tests::capture_timeout_abandons_with_a_typed_watchdog_error",
            "client_test session::uml::tests::paused_restore_timeout_abandons_with_a_typed_watchdog_error",
            "client_test session::uml::tests::rejected_service_branch_cannot_leave_a_usable_rewound_uml_session",
            "client_test session::uml::tests::service_factory_aliasing_the_live_handler_abandons_the_session",
            "client_test session::uml::tests::root_import_checks_the_independently_pinned_machine_commitment",
            "client_test session::worker::tests::worker_launcher_refuses_a_child_without_image_hello",
            "client_test session::worker::tests::killed_worker_processes_release_their_leases_and_keep_the_cache_consistent",
            "client_test image_identity::sealed_guard_tests::unexpected_executable_mapping_revokes_a_published_lease",
            "sealed_static::tests::cached_guard_rejects_unexpected_executable_mapping -- --exact",
            "recording::tests::sealed_host_admits_public_recording_round_trip -- --exact",
            "-p uml --test terminal_console_limit console_limit_is_settled_after_a_provisional_terminal_stop -- --exact",
        ):
            with self.subTest(expected=expected):
                self.assertIn(expected, commands)
        self.assertTrue((ROOT / "scripts/linux-sealed-test-runner.py").is_file())

    def test_oci_runtime_qualification_uses_sealed_static_hosts(self):
        import yaml

        cases = (
            (ci_contract.HARMONY_OCI_CHECKS.path, "container-execution",
             "Run and restore containers inside the guest", "x86_64-unknown-linux-musl"),
            (ci_contract.CONSONANCE_RUNTIME.path, "runtime-artifacts",
             "Extended platform replay", "x86_64-unknown-linux-musl"),
            (ci_contract.RELEASE.path, "guest",
             "Qualify the released runtime on hardware", '"$HOST_TARGET"'),
        )
        for path, job_name, step_name, target in cases:
            with self.subTest(workflow=path):
                job = yaml.safe_load((ROOT / path).read_text())["jobs"][job_name]
                steps = job["steps"]
                qualification = next(step for step in steps if step.get("name") == step_name)
                runner = "${{ github.workspace }}/scripts/linux-sealed-test-runner.py"
                self.assertEqual(
                    qualification["env"]["CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUNNER"],
                    runner,
                )
                self.assertIn(f"--target {target}", qualification["run"])
                self.assertIn("-p oci-support --test platform --test process_platform",
                              re.sub(r"\s+", " ", qualification["run"].replace("\\", "")))
                commands = "\n".join(step.get("run", "") for step in steps)
                self.assertIn("rustup target add", commands)
                self.assertIn("musl-tools", commands)
                if job_name == "guest":
                    self.assertEqual(
                        qualification["env"]["CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUNNER"],
                        runner,
                    )
                    self.assertEqual(
                        qualification["env"]["CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER"],
                        "musl-gcc",
                    )

    def test_linux_arm64_strict_host_package_has_its_own_bounded_job(self):
        import yaml

        registered = next(job for job in ci_contract.HARMONY_CHECKS.jobs
                          if job.name == "Consonance Host Arm64")
        self.assertEqual((registered.trigger, registered.timeout_minutes, registered.select),
                         ("pr", 15, "rust_checks"))
        jobs = yaml.safe_load((ROOT / ci_contract.HARMONY_CHECKS.path).read_text())["jobs"]
        job = jobs["consonance-host-arm64"]
        self.assertEqual(job["name"], registered.name)
        self.assertEqual(job["runs-on"], "ubuntu-24.04-arm")
        self.assertEqual(job["env"]["CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER"],
                         "musl-gcc")
        self.assertEqual(job["env"]["CC_aarch64_unknown_linux_musl"], "musl-gcc")
        self.assertEqual(job["timeout-minutes"], registered.timeout_minutes)
        self.assertEqual(job["needs"], "scope")
        self.assertEqual(job["if"], "needs.scope.outputs.rust_checks == 'true'")
        commands = "\n".join(step.get("run", "") for step in job["steps"])
        for expected in (
            "rustup target add aarch64-unknown-linux-musl",
            "cargo build --locked -p harmony-cli --bin harmony",
            "--target aarch64-unknown-linux-musl",
            "--features consonance-host --bin harmony-consonance-host",
            "scripts/test-consonance-host-package.py",
            "--platform linux",
            "target/debug/harmony check --offline --json",
        ):
            with self.subTest(expected=expected):
                self.assertIn(expected, commands)
        self.assertIn("Check the paired CLI on Arm64",
                      [step.get("name") for step in job["steps"]])
        fixture = (ROOT / "scripts/test-consonance-host-package.py").read_text()
        self.assertIn('package="faults"', fixture)

    def test_linux_cli_consumers_restore_a_paired_strict_host(self):
        import yaml

        builder = (ROOT / "scripts/build-consonance-cli-pair.sh").read_text()
        self.assertIn("--manifest-path \"$manifest\"", builder)
        self.assertIn("--target x86_64-unknown-linux-musl", builder)
        self.assertIn("--features consonance-host --bin harmony-consonance-host", builder)
        self.assertIn('source="$target/x86_64-unknown-linux-musl/release/$binary"', builder)

        def workflow(path):
            return yaml.safe_load((ROOT / path).read_text())["jobs"]

        def commands(job):
            return "\n".join(str(step.get("run", "")) for step in job["steps"])

        oci = workflow(ci_contract.HARMONY_OCI_CHECKS.path)
        package = oci["postgresql-cli-package"]
        self.assertIn("PostgreSQL CLI Package", [job.name for job in ci_contract.HARMONY_OCI_CHECKS.jobs])
        self.assertEqual(package["timeout-minutes"], 45)
        self.assertIn("scripts/build-consonance-cli-pair.sh paired-cli", commands(package))
        self.assertIn("scripts/test-consonance-host-package.py", commands(package))
        self.assertIn("postgresql-cli-package", oci["postgresql"]["needs"])
        self.assertIn("postgresql-cli-package-${{ github.run_id }}",
                      str(oci["postgresql"]["steps"]))
        self.assertIn("chmod +x target/release/harmony target/release/harmony-consonance-host",
                      commands(oci["postgresql"]))
        self.assertNotIn("cargo build", commands(oci["postgresql"]))
        self.assertIn("scripts/build-consonance-cli-pair.sh target/release",
                      commands(oci["nested-runtime"]))

        languages = workflow(ci_contract.HARMONY_LANGUAGES_CHECKS.path)
        self.assertIn("Language CLI Tools", [job.name for job in ci_contract.HARMONY_LANGUAGES_CHECKS.jobs])
        self.assertEqual(languages["tools"]["timeout-minutes"], 45)
        self.assertIn("scripts/build-consonance-cli-pair.sh paired-cli", commands(languages["tools"]))
        for consumer in ("check", "uml-command"):
            with self.subTest(consumer=consumer):
                job = languages[consumer]
                self.assertIn("tools", job["needs"])
                downloads = [step.get("with", {}).get("name") for step in job["steps"]
                             if step.get("uses", "").startswith("actions/download-artifact")]
                self.assertIn("language-cli-tools", downloads)
                self.assertIn("chmod +x target/release/harmony target/release/harmony-consonance-host",
                              commands(job))
                self.assertNotIn("cargo build", commands(job))

        for registered, artifact, consumer in (
            (ci_contract.HARMONY_HISTORICAL_BENCHMARKS, "historical-tools", "search"),
            (ci_contract.HARMONY_UML_CAMPAIGN, "uml-campaign-tools", "search"),
        ):
            with self.subTest(workflow=registered.name):
                jobs = workflow(registered.path)
                self.assertIn("scripts/build-consonance-cli-pair.sh tools", commands(jobs["tools"]))
                uploads = [step.get("with", {}).get("path") for step in jobs["tools"]["steps"]
                           if step.get("uses", "").startswith("actions/upload-artifact")]
                self.assertIn("tools", uploads)
                downloads = [step.get("with", {}).get("name") for step in jobs[consumer]["steps"]
                             if step.get("uses", "").startswith("actions/download-artifact")]
                self.assertTrue(any(artifact in name for name in downloads))
                self.assertIn("chmod +x tools/harmony tools/harmony-consonance-host",
                              commands(jobs[consumer]))

        nested = workflow(ci_contract.CONSONANCE_NESTED_HOST.path)["nested-host"]
        nested_commands = commands(nested)
        self.assertIn('"$history/source/Cargo.toml"', nested_commands)
        self.assertIn("archived source predates the paired Consonance host", nested_commands)
        self.assertIn("scripts/build-consonance-cli-pair.sh target-cli/release", nested_commands)
        self.assertIn("nested-host-evidence/harmony-consonance-host", nested_commands)

    def test_c_investigation_has_its_own_bounded_job(self):
        import yaml

        workflow = ci_contract.HARMONY_LANGUAGES_CHECKS
        jobs = yaml.safe_load((ROOT / workflow.path).read_text())["jobs"]
        registered = next(job for job in workflow.jobs if job.name == "C Investigation")
        investigation = jobs["c-investigation"]
        self.assertEqual((registered.trigger, registered.timeout_minutes,
                          registered.select), ("pr", 35, "harmony_languages"))
        self.assertTrue(registered.exception)
        self.assertEqual(ci_contract.PR_EXTENDED_CHECK_BUDGETS[
            (workflow.path, "C Investigation")], 35)
        self.assertEqual(investigation["timeout-minutes"], registered.timeout_minutes)
        self.assertEqual(jobs["check"]["timeout-minutes"], 15)
        self.assertNotIn("cli/tests/investigation.sh", str(jobs["check"]["steps"]))
        self.assertEqual(investigation["needs"], ["scope", "guest", "images", "tools"])
        downloads = [step["with"]["name"] for step in investigation["steps"]
                     if step.get("uses", "").startswith("actions/download-artifact")]
        self.assertEqual(downloads,
                         ["language-image-c", "language-guest-runtime", "language-cli-tools"])
        commands = "\n".join(step.get("run", "") for step in investigation["steps"])
        self.assertIn("docker load --input language-image/c.tar", commands)
        self.assertIn("timeout -k 10 1800 bash cli/tests/investigation.sh", commands)
        self.assertNotIn("scripts/historical-search.sh", commands)

    def test_only_the_backend_miri_variant_has_the_extended_budget(self):
        import yaml

        workflow = ci_contract.CONSONANCE_ANALYSIS
        job = yaml.safe_load((ROOT / workflow.path).read_text())["jobs"]["miri"]
        registered = next(job for job in workflow.jobs if job.name == "Miri — <Crate>")
        self.assertEqual(registered.timeout_minutes, 30)
        self.assertTrue(registered.exception)
        self.assertEqual(ci_contract.pull_request_budget(workflow.path, registered.name), 30)
        expression = "${{ matrix.name == 'vmm-backend' && 30 || 15 }}"
        self.assertEqual(job["timeout-minutes"], expression)
        self.assertEqual(ci_contract.PR_TIMEOUT_EXPRESSIONS,
                         {(workflow.path, registered.name): expression})

    def test_release_guest_archive_carries_verified_runtime_pair(self):
        import yaml

        jobs = yaml.safe_load((ROOT / ci_contract.RELEASE.path).read_text())["jobs"]
        self.assertTrue(any("cargo build --locked --release -p harmony-cli" in step.get("run", "")
                            for step in jobs["cli"]["steps"]))
        steps = jobs["guest"]["steps"]
        names = [step.get("name") for step in steps]
        verify_inputs = names.index("Verify exact platform inputs")
        package = names.index("Package the runtime and checksum")
        verify_archive = names.index("Verify packaged runtime pair")
        upload = next(index for index, step in enumerate(steps)
                      if step.get("uses", "").startswith("actions/upload-artifact"))
        self.assertLess(verify_inputs, package)
        self.assertLess(package, verify_archive)
        self.assertLess(verify_archive, upload)
        self.assertIn('"$isa/runtime-manifest.json"', steps[package]["run"])
        for check in ('manifest["version"]', 'manifest["architecture"]',
                      'manifest["source_digest"]', 'manifest["files"][name]'):
            self.assertIn(check, steps[verify_archive]["run"])

    def test_release_cli_archive_signs_and_packages_strict_host(self):
        import yaml

        steps = yaml.safe_load((ROOT / ci_contract.RELEASE.path).read_text())["jobs"]["cli"]["steps"]
        commands = "\n".join(str(step.get("run", "")) for step in steps)
        for required in (
            "--no-default-features --features consonance-host --bin harmony-consonance-host",
            "--options runtime,hard,kill",
            "--entitlements consonance/vmm-backend/hvf.entitlements.plist",
            "--library-constraint scripts/macos-os-only-library-constraint.coderequirement",
            "harmony harmony-consonance-host",
        ):
            with self.subTest(required=required):
                self.assertIn(required, commands)
        signed = next(step for step in steps if step.get("name") == "Build and constrain signed Consonance host")
        self.assertEqual(signed["if"], "runner.os == 'macOS'")

    def test_exact_runtime_consumers_wait_for_its_build(self):
        import yaml

        restore = "./.github/actions/platform-runtime"
        build = "./.github/actions/exact-platform-runtime"
        for workflow in ci_contract.WORKFLOWS:
            if "pull_request" not in workflow.triggers:
                continue
            jobs = yaml.safe_load((ROOT / workflow.path).read_text())["jobs"]
            builders = {job_id for job_id, job in jobs.items()
                        if any(step.get("uses") == build for step in job.get("steps", []))}
            for job_id, job in jobs.items():
                exact = [step for step in job.get("steps", []) if step.get("uses") == restore
                         and str((step.get("with") or {}).get("require-exact", "true")) == "true"]
                if not exact:
                    continue
                with self.subTest(workflow=workflow.name, job=job_id):
                    needs = job.get("needs", [])
                    needs = {needs} if isinstance(needs, str) else set(needs)
                    self.assertTrue(needs & builders)
                    self.assertIn("!cancelled()", str(job.get("if", "")))

    def main_push_paths(self, workflow):
        text = (ROOT / workflow.path).read_text()
        block = re.search(r"^  push:\n    branches: \[main\]\n    paths:\n((?:      - .+\n)+)", text, re.M)
        self.assertIsNotNone(block)
        return {line.strip()[2:].removesuffix("/**") for line in block.group(1).splitlines()}

    def runtime_inputs(self):
        spec = importlib.util.spec_from_file_location(
            "runtime_artifacts", ROOT / "consonance/harmony-linux/scripts/runtime-artifacts.py")
        runtime = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runtime)
        return set(runtime.INPUTS)

    def test_platform_runtime_key_excludes_workload_sources(self):
        self.assertFalse(any(path.startswith("workloads/") for path in self.runtime_inputs()))

    def workspace_root(self, crate):
        for directory in (crate, *crate.parents):
            manifest = directory / "Cargo.toml"
            if manifest.is_file() and "workspace" in tomllib.loads(manifest.read_text()):
                return directory
        raise AssertionError(f"no workspace root above {crate}")

    def path_dependency_closure(self, built, tested):
        seen = set()
        pending = [(ROOT / crate).resolve() for crate in built | tested]
        while pending:
            crate = pending.pop()
            if crate in seen:
                continue
            seen.add(crate)
            manifest = tomllib.loads((crate / "Cargo.toml").read_text())
            kinds = ["dependencies", "build-dependencies"]
            if crate.relative_to(ROOT).as_posix() in tested:
                kinds.append("dev-dependencies")
            tables = [manifest, *manifest.get("target", {}).values()]
            for table in tables:
                for kind in kinds:
                    for name, spec in table.get(kind, {}).items():
                        if isinstance(spec, dict) and spec.get("workspace"):
                            root = self.workspace_root(crate)
                            workspace = tomllib.loads((root / "Cargo.toml").read_text())
                            spec, crate_dir = workspace["workspace"]["dependencies"][name], root
                        else:
                            crate_dir = crate
                        if isinstance(spec, dict) and "path" in spec:
                            pending.append((crate_dir / spec["path"]).resolve())
        return {crate.relative_to(ROOT).as_posix() for crate in seen}

    def test_nested_host_qualification_runs_when_main_changes_its_inputs(self):
        required = (self.runtime_inputs() - {"flake.nix", "flake.lock"}) | {"rust-toolchain.toml"}
        required |= self.path_dependency_closure(
            built={"cli"}, tested={"consonance/vmm-core", "consonance/nested-driver"})
        paths = self.main_push_paths(ci_contract.CONSONANCE_NESTED_HOST)
        def covers(path, item):
            return item == path or item.startswith(path + "/")
        for item in sorted(required):
            with self.subTest(required=item):
                self.assertTrue(any(covers(path, item) for path in paths))
        for path in sorted(paths):
            with self.subTest(path=path):
                self.assertTrue(any(covers(path, item) for item in required))
    def test_uml_finding_replay_stays_with_its_recording_host(self):
        import yaml

        jobs = yaml.safe_load((ROOT / ci_contract.HARMONY_UML_CAMPAIGN.path).read_text())["jobs"]
        replays = 0
        for job in jobs.values():
            steps = job.get("steps", [])
            for index, step in enumerate(steps):
                if "historical-replay.sh reproduce" not in step.get("run", ""):
                    continue
                replays += 1
                earlier = steps[:index]
                self.assertTrue(any("historical-search.sh" in entry.get("run", "") for entry in earlier))
                self.assertFalse(any(entry.get("uses", "").startswith("actions/upload-artifact") for entry in earlier))
                self.assertEqual(step["env"]["BACKEND"], "uml")
        self.assertEqual(replays, 1)

    def test_uml_campaign_reserves_time_for_recorded_finding_replay(self):
        import yaml

        workflow = yaml.safe_load((ROOT / ci_contract.HARMONY_UML_CAMPAIGN.path).read_text())
        jobs = workflow["jobs"]
        step = next(step for step in jobs["manifest"]["steps"] if step.get("id") == "read")
        replay = next(step for step in jobs["search"]["steps"]
                      if "historical-replay.sh reproduce" in step.get("run", ""))
        replay_minutes = (int(replay["env"]["REPLAY_TIMEOUT_SECONDS"]) + 59) // 60
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            environment = dict(os.environ, CASE="etcd-3.5-inconsistency", GITHUB_OUTPUT=str(output))
            environment.update({key: str(value) for key, value in workflow["env"].items()})
            subprocess.run(["bash", "-euc", step["run"]], cwd=ROOT, env=environment,
                           check=True, capture_output=True, text=True)
            lines = output.read_text().splitlines()
            self.assertEqual(len(lines), 1)
            self.assertTrue(lines[0].startswith("matrix="))
            matrix = json.loads(lines[0].removeprefix("matrix="))
        for case in matrix["include"]:
            self.assertGreater(case["wall_minutes"], 0)
            self.assertLessEqual(case["wall_minutes"] + 20 + replay_minutes + 10,
                                 jobs["search"]["timeout-minutes"])
        self.assertLessEqual(jobs["search"]["timeout-minutes"], 360)

    def test_paths_and_names_are_unique_and_present(self):
        paths = [workflow.path for workflow in ci_contract.WORKFLOWS]
        names = [workflow.name for workflow in ci_contract.WORKFLOWS]
        self.assertEqual(len(paths), len(set(paths)))
        self.assertEqual(len(names), len(set(names)))
        for workflow in ci_contract.WORKFLOWS:
            with self.subTest(workflow=workflow.name):
                self.assertTrue(workflow.path.startswith(ci_contract.WORKFLOW_DIR + "/"))
                self.assertTrue((ROOT / workflow.path).is_file())

    def test_a_name_places_a_workflow_under_its_owner(self):
        owners = set(ci_contract.COMPONENTS) | set(ci_contract.COMPOSITIONS)
        for workflow in ci_contract.WORKFLOWS:
            with self.subTest(workflow=workflow.name):
                parts = workflow.name.split(" / ")
                self.assertIn(workflow.category, ci_contract.CATEGORIES)
                self.assertIn(len(parts), (2, 3))
                self.assertIn(workflow.owner, owners)
                self.assertEqual(parts[1], workflow.owner)

    def test_retired_categories_are_gone(self):
        for workflow in ci_contract.WORKFLOWS:
            for retired in ci_contract.RETIRED_CATEGORIES:
                with self.subTest(workflow=workflow.name, retired=retired):
                    self.assertNotIn(f"{retired} /", workflow.name)
                    self.assertNotEqual(workflow.category, retired)

    def test_display_names_are_title_case_with_labelled_variants(self):
        for workflow in ci_contract.WORKFLOWS:
            for text in (workflow.name, *(job.name for job in workflow.jobs)):
                with self.subTest(text=text):
                    self.assertEqual(ci_contract.title_case_violations(text), [])
                    if "<" in text and not text.startswith("<"):
                        self.assertIn(ci_contract.VARIANT_SEPARATOR, text)

    def test_job_names_are_unique_inside_a_workflow(self):
        for workflow in ci_contract.WORKFLOWS:
            names = [job.name for job in workflow.jobs]
            with self.subTest(workflow=workflow.name):
                self.assertEqual(len(names), len(set(names)))
                self.assertTrue(names)


class RoutingTests(unittest.TestCase):
    def test_every_job_declares_a_class_and_a_budget(self):
        for workflow in ci_contract.WORKFLOWS:
            for job in workflow.jobs:
                with self.subTest(job=f"{workflow.name} / {job.name}"):
                    self.assertIn(job.trigger, ci_contract.TRIGGER_CLASSES)
                    self.assertGreater(job.timeout_minutes, 0)

    def test_pull_request_work_stays_inside_the_bound(self):
        for workflow in ci_contract.WORKFLOWS:
            for job in workflow.jobs:
                if job.trigger != "pr":
                    continue
                with self.subTest(job=f"{workflow.name} / {job.name}"):
                    self.assertLessEqual(job.timeout_minutes, ci_contract.pull_request_budget(workflow.path, job.name))

    def test_a_bounded_workflow_carries_full_work_only_by_exception(self):
        for workflow in ci_contract.WORKFLOWS:
            if "pull_request" not in workflow.triggers:
                continue
            for job in workflow.jobs:
                if job.trigger == "full":
                    with self.subTest(job=f"{workflow.name} / {job.name}"):
                        self.assertTrue(job.exception)

    def test_a_pull_request_workflow_declares_the_push_trigger(self):
        for workflow in ci_contract.WORKFLOWS:
            with self.subTest(workflow=workflow.name):
                if "pull_request" in workflow.triggers:
                    self.assertIn("push", workflow.triggers)

    def test_scope_kinds_name_the_workflows_that_use_them(self):
        used = {}
        for workflow in ci_contract.WORKFLOWS:
            for job in workflow.jobs:
                for kind in (job.scope, job.select, *job.selects):
                    if kind:
                        used.setdefault(kind, set()).add(workflow.name)
        self.assertEqual(set(used), set(ci_contract.SCOPE_KINDS))
        for kind, workflows in used.items():
            with self.subTest(kind=kind):
                self.assertEqual(set(ci_contract.SCOPE_KINDS[kind]), workflows)
        self.assertTrue(set(SCENARIOS) <= set(ci_contract.SCOPE_KINDS))


class OwnershipTests(unittest.TestCase):
    def test_every_consonance_crate_has_one_owning_job(self):
        self.assertEqual(ci_contract.unowned_consonance_crates(), ())
        seen = set()
        for workflow in ci_contract.WORKFLOWS:
            for job in workflow.jobs:
                for crate in job.crates:
                    with self.subTest(crate=crate):
                        self.assertNotIn(crate, seen)
                    seen.add(crate)

    def test_every_vmm_core_integration_test_has_an_owner(self):
        self.assertEqual(ci_contract.unowned_vmm_core_tests(), ())
        self.assertTrue(ci_contract.vmm_core_test_targets())

    def test_partitions_split_a_package_without_overlap(self):
        for package, partition in ci_contract.LIB_PARTITIONS.items():
            prefixes = [prefix for group in partition.values() for prefix in group]
            with self.subTest(package=package):
                self.assertEqual(len(prefixes), len(set(prefixes)))
            jobs = {job.name for workflow in ci_contract.WORKFLOWS for job in workflow.jobs}
            for name in partition:
                with self.subTest(package=package, job=name):
                    self.assertIn(name, jobs)

    def test_a_partition_filter_selects_only_its_own_modules(self):
        expression = ci_contract.nextest_filter("searcher", "Archive")
        self.assertIn("test(/^search::archive::/)", expression)
        self.assertNotIn("continuation", expression)
        orphans = ci_contract.nextest_orphan_filter("searcher")
        self.assertTrue(orphans.startswith("not ("))
        self.assertIn("search::campaign", orphans)

    def test_every_miri_target_has_an_owning_component(self):
        self.assertEqual({target["name"] for target in miri_scope.TARGETS},
                         set(ci_contract.MIRI_OWNERS))
        for name, owner in ci_contract.MIRI_OWNERS.items():
            with self.subTest(target=name):
                self.assertIn(owner, ci_contract.MIRI_ANALYSIS_WORKFLOWS)
        for owner, name in ci_contract.MIRI_ANALYSIS_WORKFLOWS.items():
            with self.subTest(owner=owner):
                self.assertEqual(ci_contract.by_name(name).owner, owner)

    def test_registered_manifests_and_dependency_checks_exist(self):
        for manifest in ci_contract.CARGO_MANIFESTS:
            with self.subTest(manifest=manifest):
                self.assertTrue((ROOT / manifest).is_file())
        for command in ci_contract.DENY_COMMANDS:
            manifest = command.split()[1]
            with self.subTest(command=command):
                self.assertIn(manifest, ci_contract.CARGO_MANIFESTS + (
                    "consonance/control-proto/fuzz/Cargo.toml",))
                self.assertTrue((ROOT / manifest).is_file())


class CompositionTests(unittest.TestCase):
    def test_the_nes_composition_stays_registered(self):
        self.assertEqual(set(ci_contract.NES_COMPOSITIONS), {"Dissonance Workloads"})
        backends = set()
        for entry in ci_contract.NES_COMPOSITIONS.values():
            backends.add(entry["backend"])
            for role in ("checks", "benchmarks"):
                workflow = ci_contract.by_name(entry[role])
                with self.subTest(workflow=workflow.name):
                    self.assertTrue(workflow.name.endswith("NES"))
                    self.assertEqual(workflow.category,
                                     "Checks" if role == "checks" else "Benchmarks")
        self.assertEqual(backends, {"native"})

    def test_required_media_names_a_registered_capture(self):
        registered = ci_contract.CAPTURE_ACTIONS + ci_contract.CAPTURE_SCRIPTS
        self.assertTrue(ci_contract.MEDIA_REQUIRED)
        for (workflow, job), media in ci_contract.MEDIA_REQUIRED.items():
            for capture in media:
                with self.subTest(job=f"{workflow} / {job}"):
                    self.assertIn(capture, registered)
                    self.assertTrue((ROOT / capture).exists())

    def test_historical_scenarios_are_named_after_their_bug(self):
        names = ci_contract.historical_display_names()
        self.assertTrue(names)
        for case_id, display in names.items():
            with self.subTest(case=case_id):
                self.assertEqual(ci_contract.title_case_violations(display), [])
                self.assertNotIn("fixed", display.lower())

    def test_a_case_never_declares_a_comparison_arm(self):
        for path in sorted((ROOT / ci_contract.HISTORICAL_CASE_ROOT).glob("*/case.json")):
            case = json.loads(path.read_text())
            for key in ci_contract.FORBIDDEN_HISTORICAL_KEYS:
                with self.subTest(case=path.parent.name, key=key):
                    self.assertNotIn(key, case)


class HostCompatibilityTests(unittest.TestCase):
    def setUp(self):
        import yaml

        self.workflow = ci_contract.by_name("Checks / Harmony Host Compatibility")
        self.data = yaml.safe_load((ROOT / self.workflow.path).read_text())
        self.hosts = {key: job for key, job in self.data["jobs"].items() if key != "scope"}

    def test_darwin_rights_qualification_requires_actual_pass_records(self):
        job = self.hosts["macos-arm64"]
        step = next(step for step in job["steps"]
                    if step.get("name") == "Verify Darwin descriptor receive qualification")
        script = step["run"]
        self.assertIn('test "$(uname -m)" = arm64', script)
        self.assertNotIn("cargo ", script)
        for name in ci_contract.DARWIN_RIGHTS_QUALIFICATION_TESTS:
            self.assertIn(name, script)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            uname = root / "uname"
            uname.write_text("#!/bin/sh\nprintf 'arm64\\n'\n")
            uname.chmod(0o755)
            env = {**os.environ, "PATH": f"{root}:{os.environ['PATH']}"}
            names = ci_contract.DARWIN_RIGHTS_QUALIFICATION_TESTS
            cases = [
                ("pass", [f"PASS [0.01s] consonance-client session::worker::tests::{name}"
                          for name in names], True),
                ("progress", [f"PASS [0.01s] ( 1/2) consonance-client session::worker::tests::{name}"
                              for name in names], True),
                ("colored", [
                    "\033[32;1m        PASS\033[0m [   0.021s] (   1/1998) "
                    "\033[35;1mconsonance-client\033[0m "
                    "\033[36msession::worker::tests\033[0m\033[36m::\033[0m"
                    f"\033[34;1m{name}\033[0m" for name in names
                ], True),
                ("missing", [], False),
                ("failed", [f"FAIL [0.01s] consonance-client session::worker::tests::{name}"
                            for name in names], False),
                ("skipped", [f"SKIP [0.01s] consonance-client session::worker::tests::{name}"
                             for name in names], False),
            ]
            for missing in names:
                cases.append((missing, [
                    f"PASS [0.01s] consonance-client session::worker::tests::{name}"
                    for name in names if name != missing
                ], False))
            for label, lines, expected in cases:
                with self.subTest(case=label):
                    (root / "host-tests.log").write_text("\n".join(lines) + "\n")
                    result = subprocess.run(["bash", "-c", script], cwd=root, env=env,
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode == 0, expected, result.stderr)

    def test_host_jobs_are_selected_and_bounded_on_every_pull_request(self):
        self.assertEqual(
            [job.name for job in self.workflow.jobs if not job.selects],
            [
                "macOS Arm64",
                "macOS Consonance Host Package",
                "Linux Arm64",
                "Linux Arm64 Dissonance",
                "Linux Arm64 Qualification",
            ],
        )
        for job in self.workflow.jobs:
            with self.subTest(job=job.name):
                self.assertEqual(job.trigger, "pr")
                self.assertLessEqual(job.timeout_minutes, 15)
                if job.select:
                    self.assertEqual(job.select, "rust_checks")
                    actual = next(host for host in self.hosts.values() if host["name"] == job.name)
                    self.assertEqual(actual["needs"], "scope")
                    self.assertEqual(actual["if"], "needs.scope.outputs.rust_checks == 'true'")
                    self.assertEqual(actual["timeout-minutes"], job.timeout_minutes)

    def test_each_job_names_an_explicit_runner(self):
        runners = {job["name"]: job["runs-on"] for job in self.hosts.values()}
        self.assertEqual(
            runners,
            {
                "macOS Arm64": "macos-14",
                "macOS Consonance Host Package": "macos-14",
                "Linux Arm64": "ubuntu-24.04-arm",
                "Linux Arm64 Dissonance": "ubuntu-24.04-arm",
                "Linux Arm64 Qualification": "ubuntu-24.04-arm",
            },
        )
        for name, runner in runners.items():
            with self.subTest(job=name):
                self.assertNotIn("latest", runner)

    def test_portable_and_qualification_checks_remain_split(self):
        commands = {
            key: "\n".join(str(step.get("run", "")) for step in job["steps"])
            for key, job in self.hosts.items()
        }
        for key in ("macos-arm64", "linux-arm64"):
            with self.subTest(job=key):
                self.assertIn("cargo nextest run --workspace", commands[key])
                self.assertIn("scripts/check-portable-tests.sh", commands[key])
        self.assertNotIn("--manifest-path dissonance/Cargo.toml", commands["linux-arm64"])
        self.assertIn("--manifest-path dissonance/Cargo.toml", commands["linux-arm64-dissonance"])
        self.assertNotIn("cargo nextest run --workspace", commands["linux-arm64-dissonance"])
        self.assertIn("scripts/check-portable-tests.sh", commands["linux-arm64-dissonance"])
        self.assertEqual(self.data["env"]["DISSONANCE_PORTABLE_TEST_FLOOR"], 300)
        self.assertNotIn("qualify-host-sha256.py", commands["linux-arm64"])
        self.assertNotIn("cargo nextest run --workspace", commands["linux-arm64-qualification"])
        for key in ("macos-arm64", "linux-arm64-qualification"):
            with self.subTest(job=key):
                self.assertIn("scripts/qualify-host-sha256.py --check", commands[key])
                self.assertNotIn("check --offline --json", commands[key])
        portable_cache = next(
            step["with"] for step in self.hosts["linux-arm64"]["steps"]
            if step.get("uses") == "Swatinem/rust-cache@v2"
        )
        dissonance_cache = next(
            step["with"] for step in self.hosts["linux-arm64-dissonance"]["steps"]
            if step.get("uses") == "Swatinem/rust-cache@v2"
        )
        qualification_cache = next(
            step["with"] for step in self.hosts["linux-arm64-qualification"]["steps"]
            if step.get("uses") == "Swatinem/rust-cache@v2"
        )
        self.assertEqual(len({cache["shared-key"] for cache in (
            portable_cache, dissonance_cache, qualification_cache)}), 3)
        self.assertEqual(
            {key: value for key, value in portable_cache.items() if key != "shared-key"},
            {key: value for key, value in qualification_cache.items() if key != "shared-key"},
        )

    def test_macos_package_job_builds_and_signs_the_strict_host(self):
        commands = "\n".join(
            str(step.get("run", ""))
            for step in self.hosts["macos-consonance-host-package"]["steps"]
        )
        for required in (
            "--bin harmony",
            "--no-default-features",
            "--features consonance-host --bin harmony-consonance-host",
            "--options runtime",
            "--entitlements consonance/vmm-backend/hvf.entitlements.plist",
            "--library-constraint scripts/macos-os-only-library-constraint.coderequirement",
            "scripts/test-consonance-host-package.py",
            "--platform macos",
            "target/debug/harmony check --offline --json",
        ):
            with self.subTest(required=required):
                self.assertIn(required, commands)

    def test_a_host_job_claims_no_live_hypervisor(self):
        text = (ROOT / self.workflow.path).read_text()
        self.assertIn("not evidence that HVF or Arm KVM executes a guest", text)
        for job in self.hosts.values():
            commands = "\n".join(
                line for step in job["steps"]
                for line in str(step.get("run", "")).splitlines()
                if not line.lstrip().startswith("#"))
            with self.subTest(job=job["name"]):
                self.assertIsNone(re.search(r"test .*hypervisor.*=", commands))
                self.assertNotIn("/dev/kvm", commands)


class IgnoredTestRegistryTests(unittest.TestCase):
    PATTERN = re.compile(r"[a-z][\w-]*(?:::(?:[\w-]+|bin/[\w-]+))? (?:\*|\w+(?:::\w+)*)")

    def test_every_pattern_names_a_binary_and_a_test(self):
        for pattern, runners in ci_contract.ignored_test_runners().items():
            with self.subTest(pattern=pattern):
                self.assertIsNotNone(self.PATTERN.fullmatch(pattern))
                self.assertEqual(len(runners), len(set(runners)))

    def test_host_tests_are_keyed_by_a_machine(self):
        for host in ci_contract.HOST_TESTS:
            with self.subTest(host=host):
                self.assertRegex(
                    host,
                    r"^(?:(?:macos|linux)-(?:aarch64|x86_64)|linux-x86_64-hybrid)$",
                )

    def test_a_pattern_selects_its_whole_binary_or_one_exact_test(self):
        with mock.patch.object(ci_contract, "ignored_test_runners", lambda: {
                "vmm-backend::kvm_smoke *": ("A",), "vmm-core live::one": ("B",)}):
            self.assertEqual(ci_contract.runners_of("vmm-backend::kvm_smoke", "any"), ("A",))
            self.assertEqual(ci_contract.runners_of("vmm-core", "live::one"), ("B",))
            self.assertEqual(ci_contract.runners_of("vmm-core", "live::one_more"), ())
            self.assertEqual(ci_contract.runners_of("vmm-core::live", "one"), ())

    def test_the_host_filter_selects_each_machines_tests(self):
        with mock.patch.object(ci_contract, "HOST_TESTS", {
                "macos-aarch64": ("vmm-backend::hvf_smoke *", "vmm-core live::one")}):
            self.assertEqual(ci_contract.host_filter("macos-aarch64"),
                             "binary_id(vmm-backend::hvf_smoke) | "
                             "(binary_id(vmm-core) & test(=live::one))")
            self.assertEqual(ci_contract.host_filter("linux-x86_64"), "")

    def test_this_host_uses_the_host_test_spelling(self):
        for system, machine, expected in (("Darwin", "arm64", "macos-aarch64"),
                                          ("Linux", "aarch64", "linux-aarch64"),
                                          ("Linux", "x86_64", "linux-x86_64")):
            with self.subTest(expected=expected), \
                 mock.patch.object(ci_contract.platform, "system", lambda: system), \
                 mock.patch.object(ci_contract.platform, "machine", lambda: machine), \
                 mock.patch.object(ci_contract, "_hybrid_kvm_pair_available", return_value=False):
                self.assertEqual(ci_contract.this_host(), expected)

        with mock.patch.object(ci_contract.platform, "system", return_value="Linux"), \
             mock.patch.object(ci_contract.platform, "machine", return_value="x86_64"), \
             mock.patch.object(ci_contract, "_hybrid_kvm_pair_available", return_value=True):
            self.assertEqual(ci_contract.this_host(), "linux-x86_64-hybrid")

    def test_hybrid_pool_parser_rejects_malformed_ranges(self):
        self.assertEqual(ci_contract._cpu_list("0-3,8,10-11\n"), {0, 1, 2, 3, 8, 10, 11})
        for value in ("", "3-1", "1,,2", "x", "4-"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                ci_contract._cpu_list(value)


class NamingTests(unittest.TestCase):
    def test_title_case_accepts_canonical_spellings(self):
        for name in ("Checks / Harmony Workloads / OCI", "PostgreSQL Index Corruption",
                     "etcd Data Inconsistency", "Snapshot and Restore", "macOS Arm64",
                     "Mutation Testing — Shard <N>/16", "Miri — <Crate> (Whole Crate)"):
            with self.subTest(name=name):
                self.assertEqual(ci_contract.title_case_violations(name), [])

    def test_title_case_rejects_lowercase_words_and_misspelled_terms(self):
        for name in ("Guest memory", "guest Memory", "Public Api", "Postgresql Corruption",
                     "Checks / consonance"):
            with self.subTest(name=name):
                self.assertTrue(ci_contract.title_case_violations(name))

    def test_a_variant_placeholder_is_recognized(self):
        self.assertTrue(ci_contract.is_variant_placeholder("STB — <Difficulty>"))
        self.assertFalse(ci_contract.is_variant_placeholder("STB — Full Game"))


class CommandTests(unittest.TestCase):
    def test_the_command_line_prints_what_workflows_consume(self):
        import contextlib
        import io

        for argv, expected in ((["manifests"], "Cargo.toml"),
                               (["deny"], "--manifest-path"),
                               (["workflows"], "Checks / Consonance"),
                               (["package-flags", "Dissonance"], "-p searcher"),
                               (["nextest-filter", "vmm-core", "Virtual Time"], "virtual_time"),
                               (["nextest-orphans", "vmm-core"], "not ("),
                               (["host-filter", "macos-aarch64"], "binary_id(vmm-backend::hvf_smoke)")):
            with self.subTest(argv=argv):
                output = io.StringIO()
                with contextlib.redirect_stdout(output):
                    self.assertEqual(ci_contract._main(argv), 0)
                self.assertIn(expected, output.getvalue())

    def test_an_unknown_command_fails(self):
        with self.assertRaises(SystemExit):
            ci_contract._main(["unknown"])


if __name__ == "__main__":
    unittest.main()
