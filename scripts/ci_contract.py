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
import platform
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

# The concurrency group key of every workflow a push reaches. Each push gets a
# group of its own, so a later push never cancels or replaces its run, and
# checks that read only what a push changed see every pushed commit.
PUSH_CONCURRENCY_KEY = "${{ github.event_name == 'push' && github.sha || github.ref }}"


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
    # Ignored tests this job runs with `--ignored`, as `<binary-id> <test>`
    # where `<test>` is `*` when the job runs the whole binary.
    ignored_tests: tuple[str, ...] = ()
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
    "workloads/nes/src/bin/nes-film.rs",
    "scripts/verify-nes-films.py",
)

# Commands that run a full capability search. A job a pull request reaches
# never invokes one, whatever budget it declares.
FULL_SEARCH_COMMANDS = (
    "benchmarks/search/eval.py run",
    "nova-consonance-campaign",
    "scripts/historical-search.sh",
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
            "snapshot", "portable_snapshot", "engine_state",
            "vendor::x86::logical_identity_live_tests",
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

ARM64_BASELINE_TEST = (
    "vmm-core vendor::arm64::contract::tests::"
    "this_host_implements_the_baseline_and_the_guest_reads_it"
)

# Ignored tests that need a hypervisor no hosted runner offers, keyed by the
# `<os>-<arch>` of a machine that has one. The pre-push hook runs the entry for
# the machine it is on.
HOST_TESTS = {
    "macos-aarch64": ("vmm-backend::hvf_smoke *", ARM64_BASELINE_TEST),
    "linux-aarch64": (ARM64_BASELINE_TEST,),
}

KVM_SERVICED_EXIT_TESTS = (
    "vmm-backend::kvm_smoke serviced_pio_is_exactly_snapshottable_without_guest_execution",
    "vmm-backend::kvm_smoke serviced_msr_is_exactly_snapshottable_without_guest_execution",
    "vmm-backend::kvm_smoke serviced_exception_payload_round_trips_and_empty_restore_clears_it",
    "vmm-backend::kvm_smoke serviced_mmio_is_exactly_snapshottable_across_scalar_rmw_and_movdqu",
)

PUBLIC_SNAPSHOT_IDENTITY_TEST = (
    "vmm-core vendor::x86::logical_identity_live_tests::"
    "public_snapshot_replay_recapture_preserves_xsave_identity"
)

OCI_PLATFORM_TESTS = ("oci-support::platform *", "oci-support::process_platform *")


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
                          "vmm-backend:kvm_smoke"),
            ignored_tests=KVM_SERVICED_EXIT_TESTS + ("vmm-backend::contract_kvm *",),
            scope="consonance_kvm"),
        Job("Device State", "pr", 15,
            crates=("lapic", "gicv3", "telemetry")),
        Job("Virtual Time", "pr", 15,
            crates=("vtime",),
            test_targets=("vmm-core:virtual_time", "vmm-core:x86_kvm_virtual_time",
                          "vmm-core:x86_kvm_linux_virtual_time"),
            ignored_tests=(
                "vmm-core::x86_kvm_virtual_time "
                "x1_ten_same_seed_runs_produce_one_normalized_log",
                "vmm-core::x86_kvm_linux_virtual_time x2_same_seed_boots_one_normalized_log",
            ),
            scope="consonance_platform"),
        Job("Snapshot and Restore", "pr", 15,
            crates=("snapshot-store", "unison"),
            test_targets=("vmm-core:snapshot_branch", "vmm-core:event_loop"),
            ignored_tests=("vmm-backend::kvm_smoke save_restore_round_trips_on_real_kvm",
                           PUBLIC_SNAPSHOT_IDENTITY_TEST),
            scope="consonance_kvm"),
        Job("Execution Protocol", "pr", 15,
            crates=("consonance-client", "control-proto", "environment",
                    "execution-proto", "hypercall-doorbell", "hypercall-proto",
                    "process-proto"),
            test_targets=("vmm-core:protocol",)),
        Job("Guest Runtime", "pr", 15),
        Job("Public API", "pr", 15,
            test_targets=("vmm-core:public_api",),
            ignored_tests=tuple(
                f"{package}::public_api *" for package in (
                    "hypercall-proto", "hypercall-doorbell", "snapshot-store", "unison",
                    "vtime", "vm-state", "vmm-backend", "vmm-core", "lapic", "gicv3",
                    "telemetry", "environment", "control-proto", "harmony-sdk",
                )),
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
        Job("Runner CPU Features — Replica <N>", "full", 45,
            ignored_tests=("vmm-backend::kvm_smoke *",)),
        Job("Minimal Guest Determinism — Replica <N>", "full", 45,
            ignored_tests=("vmm-core::x86_kvm_virtual_time *",)),
        Job("Linux Guest Image", "full", 90),
        Job("Instruction Timing Sweep", "full", 60,
            test_targets=("vmm-core:n6_x86_instruction_sweep",),
            ignored_tests=("vmm-core::n6_x86_instruction_sweep *",)),
        Job("Go Runtime Determinism — Replica <N>", "full", 60,
            test_targets=("vmm-core:go_runtime_x86",),
            ignored_tests=("vmm-core::go_runtime_x86 *",)),
        Job("Linux Virtual Time — Replica <N>", "full", 90,
            test_targets=("vmm-core:x86_kvm_linux_virtual_time",),
            ignored_tests=tuple(
                f"vmm-core::x86_kvm_linux_virtual_time {test}" for test in (
                    "x2_virtual_time_stock_boot_smoke",
                    "x2_same_seed_boots_one_normalized_log",
                    "x2_component_diff_first_checkpoint",
                    "x2_component_diff_two_boots",
                ))),
        Job("Intel Determinism Search — Replica <N>", "full", 45,
            ignored_tests=("vmm-core::x86_kvm_linux_virtual_time "
                           "x2_component_diff_first_checkpoint",)),
        Job("Snapshot Identity — Replica <N>", "full", 15,
            test_targets=("vmm-core:x86_cpu_snapshots",),
            ignored_tests=KVM_SERVICED_EXIT_TESTS + (
                "vmm-backend::kvm_smoke resume_flag_preserves_instruction_breakpoint_continuation",
                PUBLIC_SNAPSHOT_IDENTITY_TEST,
            ) + tuple(
                f"vmm-core::x86_cpu_snapshots live_kvm::{test}" for test in (
                    "mmio_rmw_finishes_before_full_vmm_snapshot",
                    "mmio_init_presence_snapshot_characterization",
                    "pae_pdpt_reload_preserves_snapshot_continuation",
                    "pae_cached_pdptrs_survive_full_vmm_snapshot_restore",
                    "amd_default_npt_pae_guest_write_observations",
                    "xsave_guest_bytes_survive_cold_continuation",
                    "xsave_live_registers_survive_cold_continuation",
                )
            ) + tuple(
                f"vmm-backend kvm_sys::xsave_diagnostic::{test}" for test in (
                    "snapshot_preparation_preserves_state_except_raw_presence",
                    "snapshot_preparation_raw_presence_stability",
                    "snapshot_restore_rejects_each_pending_state_without_mutation",
                    "snapshot_restore_drains_an_acknowledged_write_completion",
                    "ymm_without_sse_uabi_mxcsr_bytes_survive_capture_and_restore",
                    "natural_avx_mxcsr_survives_capture_and_restore",
                    "snapshot_entry_restores_match_uninterrupted_execution",
                    "snapshot_canonical_entry_restores_match_uninterrupted_execution",
                    "snapshot_entry_debug_reentry_preserves_guest_observation",
                    "canonical::snapshot_guest_canonicalization_preserves_complete_endpoints",
                ))),
        Job("Results", "full", 45),
    ),
)

CONSONANCE_RUNTIME = Workflow(
    path=f"{WORKFLOW_DIR}/consonance-runtime-qualification.yml",
    name="Checks / Consonance / Guest Runtime Qualification",
    owner="Consonance",
    triggers=("schedule", "workflow_dispatch"),
    jobs=(
        Job("Exact Runtime Artifacts", "full", 120, ignored_tests=OCI_PLATFORM_TESTS),
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
            test_targets=("vmm-core:x86_kvm_xsave_kernel",),
            ignored_tests=("vmm-core::x86_kvm_xsave_kernel *",)),
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
        Job("Ignored Tests — <Host>", "pr", 15),
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
        Job("Nova", "pr", 15, media=("workloads/nes/src/bin/nes-film.rs",),
            ignored_tests=("machine::cartridge_ram "
                           "declared_cartridge_ram_survives_real_core_restore",),
            scope="dissonance_nes"),
        Job("STB", "pr", 15, media=(".github/actions/stb-evaluation",), scope="dissonance_stb"),
    ),
)

HARMONY_NES_CHECKS = Workflow(
    path=f"{WORKFLOW_DIR}/harmony-workloads-nes-checks.yml",
    name="Checks / Harmony Workloads / NES",
    owner="Harmony Workloads",
    triggers=("pull_request", "push", "schedule", "workflow_dispatch"),
    jobs=(
        Job("Nova", "pr", 15, media=("workloads/nes/src/bin/nes-film.rs",),
            scope="harmony_nes"),
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
            ignored_tests=OCI_PLATFORM_TESTS,
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
        Job("Nova — Level <N>", "full", 210, media=(".github/actions/nes-film",)),
        Job("Nova — Full Game", "full", 210, media=(".github/actions/nes-film",)),
        Job("STB — <Difficulty>", "full", 210, media=(".github/actions/nes-film",)),
        Job("Results", "full", 5),
    ),
)

HARMONY_NES_BENCHMARKS = Workflow(
    path=f"{WORKFLOW_DIR}/harmony-workloads-nes-benchmarks.yml",
    name="Benchmarks / Harmony Workloads / NES",
    owner="Harmony Workloads",
    triggers=("schedule", "workflow_dispatch"),
    jobs=(
        Job("Nova — Replica <N>", "full", 120,
            media=("workloads/nes/src/bin/nes-film.rs",)),
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
        Job("Guest Runtime — <Architecture>", "full", 120, ignored_tests=OCI_PLATFORM_TESTS),
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

# How a workflow file shows which backend it executes a composition on. A
# composition keeps at least one marker for the backend it is registered with.
BACKEND_MARKERS = {
    "native": ("--backend native", "nes-eval"),
    "consonance": ("--backend consonance", "--features consonance",
                   "nova-consonance-campaign"),
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


def ignored_test_runners() -> dict[str, tuple[str, ...]]:
    """Map every registered ignored-test pattern to what runs it."""
    runners: dict[str, tuple[str, ...]] = {}

    def add(pattern: str, runner: str) -> None:
        runners[pattern] = runners.get(pattern, ()) + (runner,)

    for workflow in WORKFLOWS:
        for job in workflow.jobs:
            for pattern in job.ignored_tests:
                add(pattern, f"{workflow.name} / {job.name}")
    for host, patterns in HOST_TESTS.items():
        for pattern in patterns:
            add(pattern, f"pre-push hook on {host}")
    return runners


def runners_of(binary_id: str, test: str) -> tuple[str, ...]:
    """What runs one ignored test, from the patterns that select it."""
    found: list[str] = []
    for pattern, runners in ignored_test_runners().items():
        pattern_binary, pattern_test = pattern.split(" ", 1)
        if pattern_binary == binary_id and pattern_test in ("*", test):
            found.extend(runners)
    return tuple(found)


def this_host() -> str:
    """This machine as a `HOST_TESTS` key."""
    system = {"Darwin": "macos"}.get(platform.system(), platform.system().lower())
    machine = {"arm64": "aarch64", "AMD64": "x86_64"}.get(platform.machine(), platform.machine())
    return f"{system}-{machine}"


def host_filter(host: str) -> str:
    """The nextest expression selecting a host's tests, empty when it has none."""
    terms = []
    for pattern in HOST_TESTS.get(host, ()):
        binary_id, test = pattern.split(" ", 1)
        term = f"binary_id({binary_id})"
        terms.append(term if test == "*" else f"({term} & test(={test}))")
    return " | ".join(terms)


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
    if command == "host-filter":
        print(host_filter(argv[1] if len(argv) > 1 else this_host()))
        return 0
    if command == "workflows":
        for workflow in WORKFLOWS:
            print(f"{workflow.path}\t{workflow.name}")
        return 0
    raise SystemExit(
        "usage: ci_contract.py "
        "{manifests|deny|workflows|package-flags <component>|"
        "nextest-filter <package> <job>|"
        "nextest-orphans <package>|host-filter [<os>-<arch>]}"
    )


if __name__ == "__main__":
    import sys

    raise SystemExit(_main(sys.argv[1:]))
