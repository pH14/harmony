# SPDX-License-Identifier: AGPL-3.0-or-later
from __future__ import annotations

import math
import json
import os
import re
import stat
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

try:
    from . import materials, qualify_sandbox, sandbox
except ImportError:  # unittest discovery loads this directory as top-level modules.
    import materials  # type: ignore[no-redef]
    import qualify_sandbox  # type: ignore[no-redef]
    import sandbox  # type: ignore[no-redef]


class SandboxValidationTests(unittest.TestCase):
    def limits(self) -> sandbox.Limits:
        return sandbox.Limits(
            wall=2.0,
            toolcalls=2,
            memorybytes=16 * 1024 * 1024,
            workbytes=8 * 1024 * 1024,
            cpus=0.5,
            pids=32,
            outputbytes=1024,
        )

    def test_limits_reject_nonfinite_and_nonpositive_values(self) -> None:
        fields = {
            "wall": math.nan,
            "toolcalls": 0,
            "memorybytes": -1,
            "workbytes": 0,
            "cpus": math.inf,
            "pids": 0,
            "outputbytes": 0,
        }
        for name, value in fields.items():
            values = dict(
                wall=2.0,
                toolcalls=2,
                memorybytes=16 * 1024 * 1024,
                workbytes=8 * 1024 * 1024,
                cpus=0.5,
                pids=32,
                outputbytes=1024,
            )
            values[name] = value
            with self.subTest(name=name):
                with self.assertRaises(ValueError):
                    sandbox.Limits(**values)

    def test_image_and_argv_validation_are_strict(self) -> None:
        image = "sha256:" + "a" * 64
        limits = self.limits()
        sandbox.Sandbox(image, limits)
        for bad in ("latest", "sha256:" + "A" * 64, "sha256:" + "a" * 63):
            with self.subTest(image=bad), self.assertRaises(ValueError):
                sandbox.Sandbox(bad, limits)
        for bad in ([], ["relative"], ["/bin/echo\x00bad"], ["/bin/echo", "x\x00y"]):
            with self.subTest(argv=bad), self.assertRaises(ValueError):
                sandbox._validate_argv(bad)
        self.assertEqual(sandbox._validate_argv(["/bin/sh", "-c", "printf '%s' ok"])[0], "/bin/sh")

    def test_create_and_exec_argv_have_only_fixed_boundary_inputs(self) -> None:
        image = "sha256:" + "b" * 64
        limits = self.limits()
        create = sandbox._create_argv(image, limits)
        self.assertIn("--read-only", create)
        self.assertIn("--network=none", create)
        self.assertFalse(any(arg.startswith("--pid=") for arg in create))
        self.assertIn("--cap-drop=ALL", create)
        self.assertIn("--security-opt=no-new-privileges:true", create)
        self.assertIn("--entrypoint=/usr/bin/env", create)
        self.assertEqual(create.count(image), 1)
        self.assertNotIn("--volume", create)
        self.assertNotIn("-v", create)
        execute = sandbox._exec_argv("c" * 64, ["/bin/echo", "hello"], workdir="/work/workspace")
        self.assertIn("/usr/bin/env", execute)
        self.assertIn("-i", execute)
        self.assertNotIn("DOCKER_CONFIG=/", execute)
        self.assertEqual(execute[-2:], ["/bin/echo", "hello"])
        interactive = sandbox._exec_argv(
            "c" * 64,
            ["/usr/bin/python3", "-c", "pass"],
            workdir="/",
            interactive=True,
        )
        self.assertIn("-i", interactive[2:6])

    def test_image_boundary_rejects_volumes_and_non_linux(self) -> None:
        image = "sha256:" + "c" * 64
        good = {"Id": image, "Os": "linux", "Config": {"Volumes": None}}
        sandbox._validate_image_inspect(good, image)
        for volumes in ({"/data": {}}, [], {"/tmp": None}):
            bad = {"Id": image, "Os": "linux", "Config": {"Volumes": volumes}}
            with self.subTest(volumes=volumes), self.assertRaises(sandbox.SandboxError):
                sandbox._validate_image_inspect(bad, image)
        bad_os = {"Id": image, "Os": "windows", "Config": {"Volumes": None}}
        with self.assertRaises(sandbox.SandboxError):
            sandbox._validate_image_inspect(bad_os, image)

    def _container_info(self, image: str, limits: sandbox.Limits) -> dict:
        return {
            "Id": "d" * 64,
            "Config": {
                "Image": image,
                "Entrypoint": ["/usr/bin/env"],
                "Cmd": ["-i", *sandbox._ENV_ASSIGNMENTS, *sandbox._KEEPALIVE],
                "User": "65534:65534",
                "Volumes": None,
            },
            "HostConfig": {
                "ReadonlyRootfs": True,
                "NetworkMode": "none",
                "Privileged": False,
                "PidsLimit": limits.pids,
                "Memory": limits.memorybytes,
                "MemorySwap": limits.memorybytes,
                "NanoCpus": sandbox._nano_cpus(limits.cpus),
                "CapDrop": ["ALL"],
                "SecurityOpt": ["no-new-privileges:true"],
                "Binds": None,
                "Devices": None,
                "PidMode": "",
                "IpcMode": "private",
                "CgroupnsMode": "private",
                "UTSMode": "",
                "UsernsMode": "",
                "LogConfig": {"Type": "none"},
                "Tmpfs": {
                    "/work": sandbox._tmpfs_arg("/work", limits.workbytes).split(":", 1)[1],
                    "/tmp": sandbox._tmpfs_arg("/tmp", limits.workbytes).split(":", 1)[1],
                },
            },
            "Mounts": [],
            "NetworkSettings": {"Networks": {"none": {}}},
            "State": {"Running": True},
        }

    def test_realized_boundary_accepts_docker_none_network_and_private_pid(self) -> None:
        image = "sha256:" + "d" * 64
        limits = self.limits()
        sandbox._validate_container_inspect(
            self._container_info(image, limits),
            container_id="d" * 64,
            image=image,
            limits=limits,
        )

    def test_realized_boundary_rejects_network_or_tmpfs_drift(self) -> None:
        image = "sha256:" + "e" * 64
        limits = self.limits()
        for mutate in (
            lambda info: info["NetworkSettings"].update(Networks={"bridge": {}}),
            lambda info: info.update(Mounts=[{"Type": "bind", "Destination": "/private"}]),
            lambda info: info["HostConfig"].update(PidMode="host"),
            lambda info: info["HostConfig"].update(PidMode="container:other"),
            lambda info: info["HostConfig"].update(IpcMode="container:other"),
            lambda info: info["HostConfig"].update(CgroupnsMode="host"),
            lambda info: info["HostConfig"]["Tmpfs"].update(
                {"/work": sandbox._tmpfs_arg("/tmp", limits.workbytes).split(":", 1)[1]}
            ),
        ):
            info = self._container_info(image, limits)
            mutate(info)
            with self.subTest(info=info):
                with self.assertRaises(sandbox.SandboxError):
                    sandbox._validate_container_inspect(
                        info,
                        container_id="d" * 64,
                        image=image,
                        limits=limits,
                    )

    def test_wall_watchdog_poisoning_destroys_idle_container(self) -> None:
        image = "sha256:" + "a" * 64
        instance = sandbox.Sandbox(image, self.limits())
        instance._entered = True
        instance._container_id = "c" * 64
        with mock.patch.object(instance, "_destroy") as destroy:
            instance._expire_wall_budget()
        destroy.assert_called_once_with()
        self.assertTrue(instance._watchdog_expired)
        with self.assertRaises(sandbox.SandboxError):
            instance._ensure_open()

    def test_controller_and_run_dispatch_one_docker_prefix(self) -> None:
        image = "sha256:" + "f" * 64
        instance = sandbox.Sandbox(image, self.limits())
        instance._prepare_docker_config()
        self.addCleanup(instance._cleanup_docker_config)
        instance._entered = True
        instance._container_id = "c" * 64
        instance._staged = True
        instance._run_started = time.monotonic()
        result = sandbox._ProcessResult(0, b"", b"", False, False)
        with mock.patch.object(sandbox, "_run_bounded", return_value=result) as call:
            instance._controller_exec(["/bin/true"])
            instance.run(["/bin/true"])
        self.assertEqual(call.call_count, 2)
        for invocation in call.call_args_list:
            argv = invocation.args[0]
            self.assertEqual(argv[0], "docker")
            self.assertNotEqual(argv[1], "docker")

    def test_docker_client_ignores_inherited_config_and_endpoint(self) -> None:
        image = "sha256:" + "1" * 64
        instance = sandbox.Sandbox(image, self.limits())
        instance._prepare_docker_config()
        self.addCleanup(instance._cleanup_docker_config)
        with mock.patch.dict(
            os.environ,
            {
                "HOME": "/host/home",
                "DOCKER_CONFIG": "/host/docker-config",
                "DOCKER_HOST": "tcp://attacker.invalid:2375",
                "DOCKER_CONTEXT": "attacker-context",
            },
            clear=False,
        ):
            with mock.patch.object(
                sandbox,
                "_run_bounded",
                return_value=sandbox._ProcessResult(0, b"", b"", False, False),
            ) as call:
                instance._docker_call(["version"], timeout=1.0, output_limit=128)
        environment = call.call_args.kwargs["env"]
        self.assertEqual(environment["DOCKER_CONFIG"], str(instance._docker_config_dir))
        self.assertEqual(environment["DOCKER_HOST"], "unix:///var/run/docker.sock")
        self.assertEqual(environment["DOCKER_CONTEXT"], "default")
        self.assertNotIn("HOME", environment)


_MOUNTINFO_WORK_BYTES = 32 * 1024 * 1024


def _mountinfo_fixture() -> str:
    # These rows follow the kernel's mountinfo shape: optional fields precede
    # the separator, while tmpfs size is a superblock option after it.
    return "\n".join(
        (
            "25 20 0:20 / / ro,relatime - overlay overlay rw,lowerdir=/base",
            "26 20 0:21 / /work rw,nosuid,nodev,relatime shared:5 - tmpfs tmpfs rw,size=32m,mode=700,uid=65534,gid=65534",
            "27 20 0:22 / /tmp rw,nosuid,nodev,noexec,relatime master:5 - tmpfs tmpfs rw,size=32m,mode=700,uid=65534,gid=65534",
        )
    )


class MountInfoCanaryTests(unittest.TestCase):
    def check_mountinfo(self, text: str) -> dict[str, object]:
        def fail(message: str) -> None:
            raise AssertionError(message)

        namespace: dict[str, object] = {"fail": fail}
        # Run the exact helper prepended to the Linux canary.  The injected
        # failure hook lets portable tests exercise rejection paths without
        # pretending to have observed a Linux container.
        exec(qualify_sandbox._MOUNTINFO_CHECK, namespace)
        namespace["check_mountinfo"](text, _MOUNTINFO_WORK_BYTES)
        return namespace

    def assert_rejected(self, text: str) -> None:
        with self.assertRaises(AssertionError):
            self.check_mountinfo(text)

    def test_actual_mountinfo_accepts_implicit_exec_and_superblock_size(self) -> None:
        self.check_mountinfo(_mountinfo_fixture())

    def test_missing_duplicate_and_malformed_rows_are_rejected(self) -> None:
        rows = _mountinfo_fixture().splitlines()
        self.assert_rejected("\n".join(rows[:2]))
        self.assert_rejected("\n".join(rows + [rows[1]]))
        self.assert_rejected("\n".join(rows + [rows[2]]))
        self.assert_rejected(
            _mountinfo_fixture().replace("shared:5 - tmpfs tmpfs", "shared:5 tmpfs tmpfs", 1)
        )
        self.assert_rejected(_mountinfo_fixture() + " trailing")

    def test_type_flags_and_exec_modes_are_checked(self) -> None:
        rows = _mountinfo_fixture().splitlines()
        variants = (
            (1, rows[1].replace("- tmpfs tmpfs", "- overlay overlay")),
            (2, rows[2].replace("- tmpfs tmpfs", "- overlay overlay")),
            (1, rows[1].replace("rw,nosuid,nodev,relatime", "nosuid,nodev,relatime")),
            (1, rows[1].replace("rw,nosuid,nodev,relatime", "rw,nodev,relatime")),
            (1, rows[1].replace("rw,nosuid,nodev,relatime", "rw,nosuid,relatime")),
            (1, rows[1].replace("rw,nosuid,nodev,relatime", "rw,nosuid,nodev,noexec,relatime")),
            (2, rows[2].replace("rw,nosuid,nodev,noexec,relatime", "rw,nosuid,nodev,relatime")),
            (0, rows[0].replace("ro,relatime", "rw,relatime")),
        )
        for index, variant in variants:
            with self.subTest(variant=variant):
                altered = rows.copy()
                altered[index] = variant
                self.assert_rejected("\n".join(altered))

    def test_size_must_be_one_matching_tmpfs_super_option(self) -> None:
        rows = _mountinfo_fixture().splitlines()
        mount_size_only = rows.copy()
        mount_size_only[1] = mount_size_only[1].replace(
            "rw,nosuid,nodev,relatime shared:5 - tmpfs tmpfs rw,size=32m,",
            "rw,nosuid,nodev,size=32m,relatime shared:5 - tmpfs tmpfs rw,",
        )
        mount_size_only[2] = mount_size_only[2].replace(
            "rw,nosuid,nodev,noexec,relatime master:5 - tmpfs tmpfs rw,size=32m,",
            "rw,nosuid,nodev,noexec,size=32m,relatime master:5 - tmpfs tmpfs rw,",
        )
        self.assert_rejected("\n".join(mount_size_only))
        self.assert_rejected(_mountinfo_fixture().replace("size=32m", "size=16m"))
        self.assert_rejected(_mountinfo_fixture().replace("size=32m,", "size=32m,size=16m,"))


class StagePayloadTests(unittest.TestCase):
    def test_payload_contains_only_selected_arm_and_preserves_exec_mode(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source = root / "source"
            source.write_text("common")
            executable = root / "script"
            executable.write_text("#!/bin/sh\nexit 0\n")
            os.chmod(executable, 0o755)
            pair = root / "pair"
            materials.freeze_pair(
                pair,
                {"common.txt": source},
                {"script": executable},
                "prompt",
                {"setting": True},
            )
            payload, digest = sandbox._stage_payload(pair, "skills", 1024 * 1024)
            document = json.loads(payload)
            self.assertEqual(document["arm"], "skills")
            self.assertEqual(document["digest"], digest)
            paths = {item["path"] for item in document["files"]}
            self.assertIn(".agent/skills/script", paths)
            self.assertIn("TASK.txt", paths)
            self.assertNotIn("manifest.json", paths)
            docs_payload, _ = sandbox._stage_payload(pair, "docs", 1024 * 1024)
            docs_paths = {item["path"] for item in json.loads(docs_payload)["files"]}
            self.assertNotIn(".agent/skills/script", docs_paths)
            script = next(item for item in document["files"] if item["path"] == ".agent/skills/script")
            self.assertEqual(script["mode"], 0o111)
            self.assertLessEqual(len(payload), 1024 * 1024)

    def test_fixed_stager_verifies_frozen_and_writable_copies(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source = root / "source"
            source.write_text("common")
            executable = root / "script"
            executable.write_text("#!/bin/sh\nexit 0\n")
            os.chmod(executable, 0o755)
            pair = root / "pair"
            materials.freeze_pair(
                pair,
                {"common.txt": source},
                {"script": executable},
                "prompt",
                {"setting": True},
            )
            payload, digest = sandbox._stage_payload(pair, "skills", 1024 * 1024)
            work = root / "work"
            temporary_dir = root / "tmp"
            work.mkdir()
            temporary_dir.mkdir()
            # Substitute once: Linux temporary roots themselves start with /tmp.
            directories = {"work": work, "tmp": temporary_dir}
            script = re.sub(
                r'"/(work|tmp)(?=/|")',
                lambda match: json.dumps(str(directories[match[1]]))[:-1],
                sandbox._STAGE_SCRIPT,
            )
            input_file = tempfile.TemporaryFile(mode="w+b")
            try:
                input_file.write(payload)
                input_file.flush()
                input_file.seek(0)
                result = sandbox._run_bounded(
                    [sys.executable, "-c", script, "skills", str(1024 * 1024)],
                    timeout=2.0,
                    output_limit=4096,
                    env={"PATH": "/usr/bin:/bin"},
                    stdin=input_file,
                )
            finally:
                input_file.close()
            self.assertEqual(result.exit_code, 0)
            self.assertEqual(result.stdout.decode().strip(), digest)
            frozen_script = work / "frozen/skills/.agent/skills/script"
            workspace_script = work / "workspace/.agent/skills/script"
            self.assertEqual(frozen_script.stat().st_mode & 0o777, 0o555)
            self.assertEqual(workspace_script.stat().st_mode & 0o777, 0o711)


class BoundedDrainTests(unittest.TestCase):
    def _python(self, source: str) -> list[str]:
        self.assertTrue(Path(sys.executable).is_absolute())
        return [sys.executable, "-c", source]

    def test_output_cap_kills_producer_and_keeps_captured_bytes_bounded(self) -> None:
        result = sandbox._run_bounded(
            self._python("import sys; sys.stdout.write('x' * 1000000)"),
            timeout=2.0,
            output_limit=128,
            env={"PATH": "/usr/bin:/bin"},
        )
        self.assertTrue(result.output_overflow)
        self.assertLessEqual(len(result.stdout) + len(result.stderr), 128)

    def test_timeout_kills_client_without_unbounded_pipe_drain(self) -> None:
        result = sandbox._run_bounded(
            self._python("import time; time.sleep(10)"),
            timeout=0.05,
            output_limit=128,
            env={"PATH": "/usr/bin:/bin"},
        )
        self.assertTrue(result.timed_out)
        self.assertLessEqual(len(result.stdout) + len(result.stderr), 128)

    def test_closed_pipes_do_not_grant_a_second_wall_timeout(self) -> None:
        result = sandbox._run_bounded(
            self._python("import os,time; os.close(1); os.close(2); time.sleep(10)"),
            timeout=0.05,
            output_limit=128,
            env={"PATH": "/usr/bin:/bin"},
        )
        self.assertTrue(result.timed_out)


if __name__ == "__main__":
    unittest.main()
