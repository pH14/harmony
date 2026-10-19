#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import hashlib
import json
import os
from pathlib import Path
import resource
import subprocess
import sys

EXPECTED = {
    "linux": "fe078d6149769230fa5abc4183c445bb1f12bc56cc8ae15719952f116ccc7d2f",
    "harmony-uml-qualify": "131b4552d58c3bc4349430116b2b3b413ca24731493fb6a354e87d240c6a2727",
    "initramfs.cpio.gz": "9887a37a27dec6ee457e7bbadbb91791648315e47d61007302ec39cff878aa9f",
    "config": "79d41d786ac0217a4f022abe2e6ec966621bd8c35f8d37b92cfe6f34b6fdf99a",
    "profile.json": "679acf18fab2591704596ba094e4affe10ac43781e986e5c6beee5c17281261b",
}


def hashes(bundle):
    result = {}
    for name in EXPECTED:
        path = bundle / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"not a regular immutable input: {name}")
        with path.open("rb") as source:
            result[name] = hashlib.file_digest(source, "sha256").hexdigest()
    if result != EXPECTED:
        raise ValueError("input bytes differ from the failed run's exact artifact")
    return result


def run_suite(bundle, output, environment, suite, timeout):
    output.mkdir()
    work = output / "work"
    work.mkdir()
    command = ["/usr/bin/timeout", "--kill-after=10", str(timeout),
               str(bundle / "harmony-uml-qualify"), "--suite", suite,
               "--profile", str(bundle), "--work", str(work),
               "--report", str(output / "report.json")]
    (output / "command.json").write_text(json.dumps(command, indent=2) + "\n")
    with (output / "execution.log").open("wb") as log:
        result = subprocess.run(command, env=environment, stdout=log, stderr=subprocess.STDOUT)
    report = output / "report.json"
    reported = json.loads(report.read_text()) if report.is_file() else None
    failures = ([check["name"] for check in reported["checks"] if not check["passed"]]
                if reported is not None else [])
    return {"suite": suite, "exit_code": result.returncode,
            "report_result": reported.get("result") if reported else None,
            "failed_checks": failures,
            "import_values_signaled_11": bool(reported and any(
                check["name"] == "checkpoint_import_values" and not check["passed"]
                and "Signaled(11)" in check.get("detail", {}).get("error", "")
                for check in reported["checks"]))}


def main():
    bundle, output = (Path(value).resolve() for value in sys.argv[1:3])
    attempts = int(sys.argv[3])
    if not 1 <= attempts <= 3:
        raise ValueError("attempt count must be between one and three")
    output.mkdir(parents=True, exist_ok=True)
    metadata = {"diagnostic_only": True, "artifact_run": 39259407907,
                "artifact": "uml-artifacts-7e18c3820d644d44aa3e55e4655391895cfcd784-x86_64",
                "uid": os.getuid(), "euid": os.geteuid(), "kernel": os.uname().release,
                "max_checkpoint_attempts": attempts, "aslr_modified": False}
    for name, path in {
        "cpuinfo": "/proc/cpuinfo", "status": "/proc/self/status",
        "limits": "/proc/self/limits", "personality": "/proc/self/personality",
        "coredump_filter": "/proc/self/coredump_filter",
        "aslr": "/proc/sys/kernel/randomize_va_space",
        "core_pattern": "/proc/sys/kernel/core_pattern",
        "core_pipe_limit": "/proc/sys/kernel/core_pipe_limit",
        "suid_dumpable": "/proc/sys/fs/suid_dumpable",
    }.items():
        (output / f"host-{name}.txt").write_text(Path(path).read_text())
    metadata["input_sha256"] = hashes(bundle)
    status = Path("/proc/self/status").read_text().splitlines()
    capabilities = next(line.split()[1] for line in status if line.startswith("CapEff:"))
    vendors = {line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
               if line.startswith("vendor_id")}
    if (metadata["uid"] == 0 or metadata["euid"] != metadata["uid"]
            or int(capabilities, 16) != 0 or vendors != {"AuthenticAMD"}):
        metadata["blocked"] = "requires an ordinary UID with no capabilities on an AMD host"
        (output / "outcome.json").write_text(json.dumps(metadata, indent=2) + "\n")
        return 2
    resource.setrlimit(resource.RLIMIT_CORE, (resource.RLIM_INFINITY, resource.RLIM_INFINITY))
    (output / "host-active-limits.txt").write_text(Path("/proc/self/limits").read_text())
    home = output / "home"
    temporary = output / "tmp"
    home.mkdir()
    temporary.mkdir()
    environment = {"PATH": "/usr/bin:/bin", "HOME": str(home), "TMPDIR": str(temporary),
                   "LANG": "C", "LC_ALL": "C"}
    metadata["qualifier_environment_keys"] = sorted(environment)
    metadata["runs"] = []
    metadata["original_import_values_signaled_11"] = False
    try:
        for suite in ("launch", "replay"):
            metadata["active_suite"] = suite
            result = run_suite(bundle, output / suite, environment, suite, 180)
            metadata["runs"].append(result)
            if result["exit_code"] != 0 or result["report_result"] != "pass":
                return 1
        for attempt in range(1, attempts + 1):
            metadata["active_suite"] = f"checkpoint-{attempt}"
            result = run_suite(bundle, output / f"checkpoint-{attempt}", environment, "checkpoint", 300)
            metadata["runs"].append(result)
            if result["exit_code"] != 0 or result["report_result"] != "pass":
                metadata["checkpoint_failed"] = True
                metadata["original_import_values_signaled_11"] = result["import_values_signaled_11"]
                return 1
        metadata["checkpoint_failed"] = False
        return 0
    finally:
        metadata["inputs_unchanged"] = hashes(bundle) == metadata["input_sha256"]
        (output / "outcome.json").write_text(json.dumps(metadata, indent=2) + "\n")


if __name__ == "__main__":
    sys.exit(main())
