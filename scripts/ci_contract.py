#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""The registry of Harmony's CI: workflow names, owners, triggers and budgets.

Workflows are organized by component and composition boundary, then by workload
family. `custom-lints.py` reads this module to check the checked-in workflows,
`semantic-lints.py` reads it for the policy it hands the judge, and
`docs/WORKFLOWS.md` describes it in prose. Everything mechanical lives here so a
new workflow is registered once.
"""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import NamedTuple


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW_DIR = ".github/workflows"

# The three top-level categories. A workflow name is a category followed by its
# owner and, where a composition or grouping owns it, further qualifiers.
CATEGORIES = ("Checks", "Benchmarks", "Release")

# Names these categories replaced. They described when a workflow ran or how
# thorough it was rather than what owns it.
RETIRED_CATEGORIES = ("Smoke", "Acceptance", "Nightly", "Validation", "Test", "Quality")

# Components and compositions that may own a workflow.
COMPONENTS = ("Repository", "Consonance", "Dissonance", "Harmony")
COMPOSITIONS = (
    "Harmony Host Compatibility",
    "Dissonance Workloads",
    "Harmony Workloads",
)

# Spellings that survive Title Case unchanged, plus the crate and shard
# identifiers that appear after a ` — ` variant separator.
CANONICAL_TERMS = (
    "macOS", "etcd", "K3s", "QuickNES", "PostgreSQL", "NES", "OCI", "API",
    "Arm64", "CLI", "KVM", "XSAVE", "HVF", "VM", "CPU", "RAM", "Miri", "Kani",
    "STB", "Nova", "Docker", "Linux", "Go", "Intel", "Harmony", "Consonance",
    "Dissonance", "N6",
)

# Words Title Case leaves lowercase unless they open or close a name.
SMALL_WORDS = (
    "a", "an", "and", "as", "at", "but", "by", "for", "in", "into", "nor",
    "of", "on", "or", "the", "to", "up", "via", "with",
)

# The separator before a matrix variant, and the budget every job reachable
# from a pull request must declare.
VARIANT_SEPARATOR = " — "
PR_BOUNDED_MINUTES = 15

# Trigger classes. `pr` jobs run on pull requests and on pushes to main and
# carry the bounded budget. `full` jobs run only on a schedule or a manual
# dispatch and declare their own ceiling.
TRIGGER_CLASSES = ("pr", "full")


class Job(NamedTuple):
    """One job's registered responsibility, routing and budget."""

    name: str
    trigger: str
    timeout_minutes: int
    # Why a `full` job lives inside a Checks workflow. Registered exceptions
    # are the only way a Checks workflow mixes trigger classes.
    exception: str = ""
    # Workspace packages whose lint and unit tests this job owns.
    crates: tuple[str, ...] = ()
    # Integration test targets this job owns, as `<package>:<target>`.
    test_targets: tuple[str, ...] = ()
    # Scenario media this job must capture, as registered action or script paths.
    media: tuple[str, ...] = ()
    # Change-selection kind this job passes to `.github/actions/ci-scope`.
    scope: str = ""


class Workflow(NamedTuple):
    """One workflow file and the responsibilities it owns."""

    path: str
    name: str
    owner: str
    jobs: tuple[Job, ...]
    triggers: tuple[str, ...]

    @property
    def category(self) -> str:
        return self.name.split(" / ", 1)[0]


# Media capture that a scenario may use. A job that must publish a film names
# one of these; the linter resolves the path instead of searching for an
# `ffmpeg` substring.
CAPTURE_ACTIONS = (
    ".github/actions/stb-evaluation",
    ".github/actions/nes-film",
)
CAPTURE_SCRIPTS = (
    "benchmarks/search/eval.py",
    "workloads/nes/src/bin/nova-consonance-campaign.rs",
)

MIRI_FLAGS_DEFAULT = "-Zmiri-permissive-provenance"

# Every manifest that owns formatting, lints and tests. A workspace root covers
# its members; the rest are standalone packages outside any workspace.
CARGO_MANIFESTS = (
    "Cargo.toml",
    "consonance/harmony-linux/sdk/Cargo.toml",
    "consonance/harmony-linux/supervisor/Cargo.toml",
    "consonance/harmony-linux/runtime-fixture/Cargo.toml",
    "dissonance/Cargo.toml",
    "workloads/fault-policy/Cargo.toml",
    "workloads/faults/Cargo.toml",
    "workloads/nes/Cargo.toml",
    "workloads/nes-guest/Cargo.toml",
    "workloads/nes-machine/Cargo.toml",
    "workloads/nes-protocol/Cargo.toml",
    "workloads/tools/Cargo.toml",
)

# Every `cargo deny` invocation, as the arguments that follow `cargo deny`.
# The root workspace and the searcher each carry their own `deny.toml`; the
# out-of-workspace guest manifests are checked against the root allow-list for
# licenses alone, because their dependency sets are audited upstream.
DENY_COMMANDS = (
    "--manifest-path Cargo.toml check",
    "--manifest-path dissonance/Cargo.toml check",
    "--manifest-path workloads/nes/Cargo.toml check --config deny.toml",
    "--manifest-path workloads/nes-protocol/Cargo.toml check --config deny.toml",
    "--manifest-path workloads/fault-policy/Cargo.toml check --config deny.toml",
    "--manifest-path workloads/faults/Cargo.toml check --config deny.toml",
    "--manifest-path workloads/tools/Cargo.toml check --config deny.toml",
    "--manifest-path consonance/control-proto/fuzz/Cargo.toml check --config deny.toml licenses",
    "--manifest-path consonance/harmony-linux/sdk/Cargo.toml check --config deny.toml licenses",
    "--manifest-path consonance/harmony-linux/supervisor/Cargo.toml check --config deny.toml licenses",
    "--manifest-path consonance/harmony-linux/runtime-fixture/Cargo.toml check --config deny.toml licenses",
    "--manifest-path workloads/nes-guest/Cargo.toml check --config deny.toml licenses",
)

# Packages whose tests are split across jobs by module, and the module prefixes
# each job owns. The union must cover every test in the package; the owning
# workflow runs a step that fails when a module falls outside it.
LIB_PARTITIONS = {
    "vmm-core": {
        "Guest Memory": (
            "vmm", "exec",
            "vendor::x86::linux_loader",
            "vendor::arm64::image_loader", "vendor::arm64::dtb",
        ),
        "CPU State": (
            "vendor::x86::bringup", "vendor::x86::contract", "vendor::x86::entry",
            "vendor::x86::records", "vendor::x86::profile_tests",
            "vendor::arm64::bringup", "vendor::arm64::contract",
            "vendor::arm64::entry", "vendor::arm64::records",
        ),
        "Device State": (
            "vendor::x86::devices",
            "vendor::arm64::devices", "vendor::arm64::comparator_tests",
        ),
        "Virtual Time": ("virtual_time",),
        "Snapshot and Restore": (
            "snapshot", "portable_snapshot", "engine_state", "controlled_guest",
        ),
        "Execution Protocol": (
            "control", "control_state", "session_trace",
            "vendor::x86::dispatch", "vendor::arm64::dispatch",
        ),
    },
    "searcher": {
        "Archive": ("search::archive", "search::key_counts", "search::draw"),
        "Scheduling": (
            "search::duration", "search::empirical_steps", "search::parallel",
        ),
        "Search Coordination": ("search::continuation", "search::rand"),
        "Campaign Recording and Replay": ("search::campaign",),
    },
}


def _consonance_crates() -> tuple[str, ...]:
    return (
        "consonance-client", "control-proto", "environment", "execution-proto",
        "gicv3", "guest-image", "hypercall-doorbell", "hypercall-proto",
        "lapic", "oci-support", "process-proto", "snapshot-store", "telemetry",
        "unison", "vm-state", "vmm-backend", "vmm-core", "vtime",
    )


CONSONANCE_CHECKS = Workflow(
    path=f"{WORKFLOW_DIR}/consonance-checks.yml",
    name="Checks / Consonance",
    owner="Consonance",
    triggers=("pull_request", "push"),
    jobs=(
        Job("Guest Memory", "pr", 15,
            crates=("guest-image", "oci-support", "vmm-core"),
            test_targets=("vmm-core:linux_loader_proptest",)),
        Job("CPU State", "pr", 15,
            crates=("vm-state", "vmm-backend"),
            test_targets=("vmm-core:x86_cpu_snapshots", "vmm-core:arm64_skeleton",
                          "vmm-core:arm64_tcg_smoke", "vmm-backend:kvm_smoke"),
            scope="consonance_kvm"),
        Job("Device State", "pr", 15,
            crates=("lapic", "gicv3", "telemetry")),
        Job("Virtual Time", "pr", 15,
            crates=("vtime",),
            test_targets=("vmm-core:virtual_time", "vmm-core:x86_kvm_virtual_time",
                          "vmm-core:x86_kvm_linux_virtual_time"),
            scope="consonance_platform"),
        Job("Snapshot and Restore", "pr", 15,
            crates=("snapshot-store", "unison"),
            test_targets=("vmm-core:snapshot_branch", "vmm-core:event_loop"),
            scope="consonance_kvm"),
        Job("Execution Protocol", "pr", 15,
            crates=("consonance-client", "control-proto", "environment",
                    "execution-proto", "hypercall-doorbell", "hypercall-proto",
                    "process-proto"),
            test_targets=("vmm-core:protocol",)),
        Job("Guest Runtime", "pr", 15),
        Job("Public API", "pr", 15,
            test_targets=("vmm-core:public_api",),
            scope="public_api"),
    ),
)

CONSONANCE_ANALYSIS = Workflow(
    path=f"{WORKFLOW_DIR}/consonance-analysis.yml",
    name="Checks / Consonance / Analysis",
    owner="Consonance",
    triggers=("pull_request", "push", "schedule", "workflow_dispatch"),
    jobs=(
        Job("Miri — <Crate>", "pr", 15, scope="miri"),
        Job("Miri — <Crate> (Whole Crate)", "full", 320,
            exception="Interpreting a whole unsafe crate under Miri takes hours, "
                      "so pull requests get the tests the change reaches."),
        Job("Coverage", "full", 30,
            exception="An instrumented build and run of every Consonance crate "
                      "exceeds the pull request budget."),
        Job("Mutation Testing — Shard <N>/16", "full", 320,
            exception="Mutation testing rebuilds the component once per mutant."),
        Job("Proofs", "pr", 15, scope="kani"),
    ),
)

CONSONANCE_HARDWARE = Workflow(
    path=f"{WORKFLOW_DIR}/consonance-hardware-qualification.yml",
    name="Checks / Consonance / Hardware Qualification",
    owner="Consonance",
    triggers=("schedule", "workflow_dispatch"),
    jobs=(
        Job("Runner CPU Features", "full", 45),
        Job("Minimal Guest Determinism", "full", 45),
        Job("Linux Guest Image", "full", 90),
        Job("Instruction Timing Sweep", "full", 60,
            test_targets=("vmm-core:n6_x86_instruction_sweep",)),
        Job("Go Runtime Determinism", "full", 60,
            test_targets=("vmm-core:go_runtime_x86",)),
        Job("Linux Virtual Time", "full", 90,
            test_targets=("vmm-core:x86_kvm_linux_virtual_time",)),
        Job("Intel Determinism Search", "full", 45),
        Job("Snapshot Identity — Replica <N>", "full", 15,
            test_targets=("vmm-core:x86_cpu_snapshots",)),
        Job("Results", "full", 45),
    ),
)

CONSONANCE_RUNTIME = Workflow(
    path=f"{WORKFLOW_DIR}/consonance-runtime-qualification.yml",
    name="Checks / Consonance / Guest Runtime Qualification",
    owner="Consonance",
    triggers=("schedule", "workflow_dispatch"),
    jobs=(
        Job("Exact Runtime Artifacts", "full", 120),
    ),
)

CONSONANCE_XSAVE = Workflow(
    path=f"{WORKFLOW_DIR}/consonance-kernel-xsave-qualification.yml",
    name="Checks / Consonance / Kernel XSAVE Qualification",
    owner="Consonance",
    triggers=("workflow_dispatch",),
    jobs=(
        Job("Kernel Fixture", "full", 90),
        Job("Kernel XSAVE — Replica <N>", "full", 20,
            test_targets=("vmm-core:x86_kvm_xsave_kernel",)),
    ),
)

DISSONANCE_CHECKS = Workflow(
    path=f"{WORKFLOW_DIR}/dissonance-checks.yml",
    name="Checks / Dissonance",
    owner="Dissonance",
    triggers=("pull_request", "push"),
    jobs=(
        Job("Archive", "pr", 15, crates=("searcher",)),
        Job("Scheduling", "pr", 15, test_targets=("searcher:adaptive_durations",)),
        Job("Search Coordination", "pr", 15, test_targets=("searcher:interfaces",)),
        Job("Campaign Recording and Replay", "pr", 15,
            test_targets=("searcher:adaptive_campaign",)),
    ),
)

DISSONANCE_ANALYSIS = Workflow(
    path=f"{WORKFLOW_DIR}/dissonance-analysis.yml",
    name="Checks / Dissonance / Analysis",
    owner="Dissonance",
    triggers=("schedule", "workflow_dispatch"),
    jobs=(
        Job("Coverage", "full", 30,
            exception="An instrumented build and run of the searcher exceeds the "
                      "pull request budget."),
    ),
)

HARMONY_CHECKS = Workflow(
    path=f"{WORKFLOW_DIR}/harmony-checks.yml",
    name="Checks / Harmony",
    owner="Harmony",
    triggers=("pull_request", "push"),
    jobs=(
        Job("CLI", "pr", 15, crates=("harmony-cli",), test_targets=("harmony-cli:staging",)),
        Job("NES Adapter", "pr", 15),
        Job("Fault Injection", "pr", 15),
    ),
)

HARMONY_ANALYSIS = Workflow(
    path=f"{WORKFLOW_DIR}/harmony-analysis.yml",
    name="Checks / Harmony / Analysis",
    owner="Harmony",
    triggers=("pull_request", "push", "schedule", "workflow_dispatch"),
    jobs=(
        Job("Miri — <Crate>", "pr", 15, scope="miri"),
        Job("Miri — <Crate> (Whole Crate)", "full", 240,
            exception="Interpreting a whole adapter crate under Miri takes hours, "
                      "so pull requests get the tests the change reaches."),
        Job("Coverage", "full", 30,
            exception="An instrumented build and run of the CLI exceeds the pull "
                      "request budget."),
        Job("Mutation Testing — Shard <N>/4", "full", 320,
            exception="Mutation testing rebuilds the CLI once per mutant."),
    ),
)

REPOSITORY_CHECKS = Workflow(
    path=f"{WORKFLOW_DIR}/repository-checks.yml",
    name="Checks / Repository",
    owner="Repository",
    triggers=("pull_request", "push"),
    jobs=(
        Job("Formatting and Lints", "pr", 15),
        Job("Dependency Boundaries", "pr", 15),
        Job("Semantic Lints", "pr", 15),
        Job("Tooling", "pr", 15),
    ),
)

HARMONY_HOST_COMPATIBILITY = Workflow(
    path=f"{WORKFLOW_DIR}/harmony-host-compatibility.yml",
    name="Checks / Harmony Host Compatibility",
    owner="Harmony Host Compatibility",
    triggers=("pull_request", "push"),
    jobs=(
        Job("macOS Arm64", "pr", 15),
        Job("Linux Arm64", "pr", 15),
    ),
)

DISSONANCE_NES_CHECKS = Workflow(
    path=f"{WORKFLOW_DIR}/dissonance-workloads-nes-checks.yml",
    name="Checks / Dissonance Workloads / NES",
    owner="Dissonance Workloads",
    triggers=("pull_request", "push"),
    jobs=(
        Job("Nova", "pr", 15, scope="dissonance_nes"),
        Job("STB", "pr", 15, media=(".github/actions/stb-evaluation",), scope="dissonance_stb"),
    ),
)

HARMONY_NES_CHECKS = Workflow(
    path=f"{WORKFLOW_DIR}/harmony-workloads-nes-checks.yml",
    name="Checks / Harmony Workloads / NES",
    owner="Harmony Workloads",
    triggers=("pull_request", "push", "schedule", "workflow_dispatch"),
    jobs=(
        Job("Nova", "pr", 15, scope="harmony_nes"),
        Job("Backend Equivalence", "full", 75,
            exception="Comparing the native and Consonance backends builds the "
                      "exact guest runtime and both game images."),
    ),
)

HARMONY_OCI_CHECKS = Workflow(
    path=f"{WORKFLOW_DIR}/harmony-workloads-oci-checks.yml",
    name="Checks / Harmony Workloads / OCI",
    owner="Harmony Workloads",
    triggers=("pull_request", "push", "schedule", "workflow_dispatch"),
    jobs=(
        Job("Container Execution", "pr", 15,
            test_targets=("oci-support:platform", "oci-support:process_platform"),
            scope="consonance_platform"),
        Job("PostgreSQL", "pr", 15, scope="harmony_oci"),
        Job("Docker", "full", 90,
            exception="Building the pinned Docker workload image and booting it "
                      "twice under nested KVM exceeds the pull request budget."),
        Job("K3s", "full", 90,
            exception="Building the pinned K3s workload image and bringing a "
                      "cluster up twice exceeds the pull request budget."),
    ),
)

DISSONANCE_NES_BENCHMARKS = Workflow(
    path=f"{WORKFLOW_DIR}/dissonance-workloads-nes-benchmarks.yml",
    name="Benchmarks / Dissonance Workloads / NES",
    owner="Dissonance Workloads",
    triggers=("schedule", "workflow_dispatch"),
    jobs=(
        Job("Nova — Level <N>", "full", 210),
        Job("Nova — Full Game", "full", 210),
        Job("STB — <Difficulty>", "full", 210),
        Job("Results", "full", 5),
    ),
)

HARMONY_NES_BENCHMARKS = Workflow(
    path=f"{WORKFLOW_DIR}/harmony-workloads-nes-benchmarks.yml",
    name="Benchmarks / Harmony Workloads / NES",
    owner="Harmony Workloads",
    triggers=("schedule", "workflow_dispatch"),
    jobs=(
        Job("Nova — Replica <N>", "full", 120),
        Job("Results", "full", 15),
    ),
)

HARMONY_HISTORICAL_BENCHMARKS = Workflow(
    path=f"{WORKFLOW_DIR}/harmony-workloads-historical-bugs.yml",
    name="Benchmarks / Harmony Workloads / Historical Bugs",
    owner="Harmony Workloads",
    triggers=("schedule", "workflow_dispatch"),
    jobs=(
        Job("Case Manifests", "full", 5),
        Job("Guest Runtime", "full", 90),
        Job("Harmony", "full", 60),
        Job("Workload Images — <Case>", "full", 90),
        Job("<Scenario>", "full", 360),
        Job("Results", "full", 10),
    ),
)

RELEASE = Workflow(
    path=f"{WORKFLOW_DIR}/release.yml",
    name="Release / Harmony",
    owner="Harmony",
    triggers=("push",),
    jobs=(
        Job("CLI — <Platform>", "full", 60),
        Job("Guest Runtime — <Architecture>", "full", 120),
        Job("Publish", "full", 30),
    ),
)

WORKFLOWS = (
    REPOSITORY_CHECKS,
    HARMONY_HOST_COMPATIBILITY,
    CONSONANCE_CHECKS,
    CONSONANCE_ANALYSIS,
    CONSONANCE_HARDWARE,
    CONSONANCE_RUNTIME,
    CONSONANCE_XSAVE,
    DISSONANCE_CHECKS,
    DISSONANCE_ANALYSIS,
    HARMONY_CHECKS,
    HARMONY_ANALYSIS,
    DISSONANCE_NES_CHECKS,
    HARMONY_NES_CHECKS,
    HARMONY_OCI_CHECKS,
    DISSONANCE_NES_BENCHMARKS,
    HARMONY_NES_BENCHMARKS,
    HARMONY_HISTORICAL_BENCHMARKS,
    RELEASE,
)

# The two NES compositions. Native QuickNES exercises the Dissonance adapter;
# the Harmony composition runs the same game inside a Consonance VM. Each needs
# a bounded check and a full benchmark, and neither substitutes for the other.
NES_COMPOSITIONS = {
    "Dissonance Workloads": {
        "checks": DISSONANCE_NES_CHECKS.name,
        "benchmarks": DISSONANCE_NES_BENCHMARKS.name,
        "backend": "native",
    },
    "Harmony Workloads": {
        "checks": HARMONY_NES_CHECKS.name,
        "benchmarks": HARMONY_NES_BENCHMARKS.name,
        "backend": "consonance",
    },
}

# Jobs that must publish a film with a real audio stream, and the registered
# capture each one uses.
MEDIA_REQUIRED = {
    (workflow.name, job.name): job.media
    for workflow in WORKFLOWS
    for job in workflow.jobs
    if job.media
}

# Component ownership of the Miri targets declared in `miri_scope.py`.
MIRI_OWNERS = {
    "vmm-core": "Consonance",
    "hypercall-doorbell": "Consonance",
    "hypercall-doorbell-round-trip": "Consonance",
    "consonance-client": "Consonance",
    "vm-state": "Consonance",
    "vmm-backend": "Consonance",
    "snapshot-store": "Consonance",
    "harmony-supervisor": "Consonance",
    "machine": "Harmony",
    "nes-guest": "Harmony",
}

MIRI_ANALYSIS_WORKFLOWS = {
    "Consonance": CONSONANCE_ANALYSIS.name,
    "Harmony": HARMONY_ANALYSIS.name,
}

# Change-selection kinds the composite action accepts, and the workflow that
# owns each one.
SCOPE_KINDS = {
    "dissonance_nes": DISSONANCE_NES_CHECKS.name,
    "dissonance_stb": DISSONANCE_NES_CHECKS.name,
    "harmony_nes": HARMONY_NES_CHECKS.name,
    "harmony_oci": HARMONY_OCI_CHECKS.name,
    "consonance_platform": CONSONANCE_CHECKS.name,
    "consonance_kvm": CONSONANCE_CHECKS.name,
    "public_api": CONSONANCE_CHECKS.name,
    "kani": CONSONANCE_ANALYSIS.name,
    "miri": CONSONANCE_ANALYSIS.name,
}

# Historical bug scenarios are named after the bug, never after an execution
# arm. A case declares its own job name in `ci.display_name`.
HISTORICAL_CASE_ROOT = "workloads/bugs/historical"
FORBIDDEN_HISTORICAL_KEYS = ("arms", "search_arms", "replay_arms", "control_version")


def by_name(name: str) -> Workflow:
    for workflow in WORKFLOWS:
        if workflow.name == name:
            return workflow
    raise KeyError(name)


def by_path(path: str) -> Workflow | None:
    for workflow in WORKFLOWS:
        if workflow.path == path:
            return workflow
    return None


def registered_paths() -> tuple[str, ...]:
    return tuple(workflow.path for workflow in WORKFLOWS)


def component_packages(component: str) -> tuple[str, ...]:
    """The workspace packages a component's Checks workflows own."""
    packages: list[str] = []
    for workflow in WORKFLOWS:
        if workflow.owner != component:
            continue
        for job in workflow.jobs:
            packages.extend(job.crates)
    return tuple(sorted(set(packages)))


def owned_crates() -> dict[str, str]:
    """Map every registered workspace package to the job that lints and tests it."""
    owners: dict[str, str] = {}
    for workflow in WORKFLOWS:
        for job in workflow.jobs:
            for crate in job.crates:
                owners[crate] = f"{workflow.name} / {job.name}"
    return owners


def owned_test_targets() -> dict[str, tuple[str, ...]]:
    """Map every registered `<package>:<target>` to the jobs that run it."""
    owners: dict[str, tuple[str, ...]] = {}
    for workflow in WORKFLOWS:
        for job in workflow.jobs:
            for target in job.test_targets:
                owners[target] = owners.get(target, ()) + (f"{workflow.name} / {job.name}",)
    return owners


def unowned_vmm_core_tests() -> tuple[str, ...]:
    """vmm-core integration tests that no registered job runs."""
    owners = owned_test_targets()
    return tuple(
        target for target in vmm_core_test_targets()
        if f"vmm-core:{target}" not in owners
    )


def unowned_consonance_crates() -> tuple[str, ...]:
    owners = owned_crates()
    return tuple(crate for crate in _consonance_crates() if crate not in owners)


def vmm_core_test_targets() -> tuple[str, ...]:
    """The vmm-core integration tests on disk, whose split the registry owns."""
    tests = ROOT / "consonance/vmm-core/tests"
    return tuple(sorted(path.stem for path in tests.glob("*.rs")))


def historical_display_names() -> dict[str, str]:
    """Map each runnable historical case id to its registered scenario job name."""
    names: dict[str, str] = {}
    for path in sorted((ROOT / HISTORICAL_CASE_ROOT).glob("*/case.json")):
        case = json.loads(path.read_text())
        ci = case.get("ci") or {}
        if ci.get("status") == "runnable":
            names[case["id"]] = ci["display_name"]
    return names


_VARIANT = re.compile(r"<[A-Za-z][A-Za-z ]*>$")


def is_variant_placeholder(text: str) -> bool:
    """Whether a registered job name ends in a matrix placeholder."""
    return bool(_VARIANT.search(text))


def title_case_violations(name: str) -> list[str]:
    """Report words in a display name that are not Title Case."""
    problems = []
    words = [word for word in re.split(r"[\s/]+", name) if word]
    for index, word in enumerate(words):
        bare = word.strip("()")
        if not bare or bare.startswith("<") or bare.startswith("$") or bare == "—":
            continue
        canonical = next((term for term in CANONICAL_TERMS if term.lower() == bare.lower()), None)
        if canonical is not None:
            if bare != canonical:
                problems.append(f"{word!r} should be spelled {canonical!r}")
            continue
        if any(term in bare for term in CANONICAL_TERMS):
            continue
        if not bare[0].isalpha():
            continue
        lowered = bare.lower()
        if lowered in SMALL_WORDS and 0 < index < len(words) - 1:
            if bare != lowered:
                problems.append(f"{word!r} should stay lowercase")
            continue
        if bare[0] != bare[0].upper():
            problems.append(f"{word!r} should be capitalized")
    return problems


def _module_expression(prefixes: tuple[str, ...]) -> str:
    return " + ".join(f"test(/^{prefix}::/)" for prefix in prefixes)


def nextest_filter(package: str, job: str) -> str:
    """The nextest expression selecting one job's share of a split package."""
    return _module_expression(LIB_PARTITIONS[package][job])


def nextest_orphan_filter(package: str) -> str:
    """The expression selecting tests no job in the partition claims."""
    owned = tuple(
        prefix
        for prefixes in LIB_PARTITIONS[package].values()
        for prefix in prefixes
    )
    return f"not ({_module_expression(owned)})"


def _main(argv: list[str]) -> int:
    command = argv[0] if argv else ""
    if command == "manifests":
        print("\n".join(CARGO_MANIFESTS))
        return 0
    if command == "package-flags":
        print(" ".join(f"-p {package}" for package in component_packages(argv[1])))
        return 0
    if command == "deny":
        print("\n".join(DENY_COMMANDS))
        return 0
    if command == "nextest-filter":
        print(nextest_filter(argv[1], argv[2]))
        return 0
    if command == "nextest-orphans":
        print(nextest_orphan_filter(argv[1]))
        return 0
    if command == "workflows":
        for workflow in WORKFLOWS:
            print(f"{workflow.path}\t{workflow.name}")
        return 0
    raise SystemExit(
        "usage: ci_contract.py "
        "{manifests|deny|workflows|package-flags <component>|"
        "nextest-filter <package> <job>|"
        "nextest-orphans <package>}"
    )


if __name__ == "__main__":
    import sys

    raise SystemExit(_main(sys.argv[1:]))
