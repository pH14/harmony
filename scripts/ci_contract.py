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
import os
import platform
import re
from pathlib import Path
from typing import NamedTuple


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW_DIR = ".github/workflows"

# The top-level categories. A workflow name is a category followed by its
# owner and, where a composition or grouping owns it, further qualifiers.
CATEGORIES = ("Checks", "Benchmarks", "Release", "Maintenance")

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
    "STB", "Docker", "Linux", "Go", "Intel", "Harmony", "Consonance",
    "Dissonance", "N6", "UML",
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
PR_ARTIFACT_BUILD_BUDGETS = {
    (f"{WORKFLOW_DIR}/consonance-uml.yml", "UML Artifacts — <Architecture>"): 45,
    (f"{WORKFLOW_DIR}/consonance-checks.yml", "Exact Runtime Artifacts"): 45,
    (f"{WORKFLOW_DIR}/harmony-workloads-oci-checks.yml", "Exact Runtime Artifacts"): 45,
    (f"{WORKFLOW_DIR}/harmony-workloads-oci-checks.yml", "PostgreSQL CLI Package"): 45,
    (f"{WORKFLOW_DIR}/harmony-workloads-languages-checks.yml", "Language Guest Runtime"): 45,
    (f"{WORKFLOW_DIR}/harmony-workloads-languages-checks.yml", "Language CLI Tools"): 45,
    (f"{WORKFLOW_DIR}/harmony-workloads-languages-checks.yml", "Language UML Profile"): 45,
    (f"{WORKFLOW_DIR}/harmony-workloads-languages-checks.yml", "Language Image — <Language>"): 45,
}
PR_EXTENDED_CHECK_BUDGETS = {
    (f"{WORKFLOW_DIR}/harmony-workloads-languages-checks.yml", "C Investigation"): 35,
    (f"{WORKFLOW_DIR}/consonance-analysis.yml", "Miri — <Crate>"): 30,
}
PR_TIMEOUT_EXPRESSIONS = {
    (f"{WORKFLOW_DIR}/consonance-analysis.yml", "Miri — <Crate>"):
        "${{ (matrix.name == 'vmm-backend' || matrix.name == 'snapshot-store') && 30 || 15 }}",
}


def pull_request_budget(workflow_path: str, job_name: str) -> int:
    return PR_ARTIFACT_BUILD_BUDGETS.get(
        (workflow_path, job_name),
        PR_EXTENDED_CHECK_BUDGETS.get((workflow_path, job_name), PR_BOUNDED_MINUTES),
    )


# Trigger classes. `pr` jobs run on pull requests and on pushes to main and
# carry the bounded budget. `full` jobs run on a schedule, a manual dispatch,
# or a path-filtered push to main, and declare their own ceiling.
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
    # Why full work lives inside Checks, or why a separately registered
    # pull-request job needs a longer budget.
    exception: str = ""
    # Cargo packages whose lint and unit tests this job owns.
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
    # Change-selection kind the workflow's `scope` job decides for this job. The
    # job needs `scope` and runs only when that output is true, so an unselected
    # job never starts a runner.
    select: str = ""
    # Kinds this job decides for the other jobs of its workflow.
    selects: tuple[str, ...] = ()


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
    "consonance/host-code-identity/tests/witness/Cargo.toml",
    "dissonance/Cargo.toml",
    "workloads/fault-policy/Cargo.toml",
    "workloads/faults/Cargo.toml",
    "workloads/nes/Cargo.toml",
    "workloads/nes-guest/Cargo.toml",
    "workloads/nes-machine/Cargo.toml",
    "workloads/nes-protocol/Cargo.toml",
    "workloads/tiny-worlds/Cargo.toml",
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
    "--manifest-path workloads/tiny-worlds/Cargo.toml check --config deny.toml",
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
            "control", "control_state", "service_transcript", "session_trace", "host_telemetry", "sdk_telemetry",
            "vendor::x86::dispatch", "vendor::arm64::dispatch",
        ),
    },
    "searcher": {
        "Archive": (
            "search::archive", "search::draw", "search::draw_tables",
            "search::weighted_set",
        ),
        "Scheduling": (
            "search::duration", "search::empirical_steps", "search::parallel",
        ),
        "Search Coordination": (
            "search::continuation", "search::rand", "search::job_observation",
        ),
        "Campaign Recording and Replay": (
            "search::campaign", "search::checkpoint", "search::telemetry",
            "search::executable_identity",
        ),
    },
}

ARM64_BASELINE_TEST = (
    "vmm-core vendor::arm64::contract::tests::"
    "this_host_implements_the_baseline_and_the_guest_reads_it"
)

# Ignored tests that need a hypervisor no hosted runner offers, keyed by the
# `<os>-<arch>` of a machine that has one. A `-hybrid` suffix selects a Linux
# x86-64 KVM host with both core-type pools accessible to the caller. The
# pre-push hook runs the entry for the machine it is on.
HOST_TESTS = {
    "macos-aarch64": (
        "vmm-backend::hvf_smoke *", ARM64_BASELINE_TEST,
        "vmm-backend hvf::tests::fixed_register_inventory_refuses_capture_and_resets_restore",
        "vmm-backend hvf::tests::sme_capture_requires_inactive_state_and_restore_clears_detour",
        "vmm-backend hvf::tests::failed_translation_flush_retires_mutated_vcpu",
        "vmm-backend hvf::tests::restore_rejects_hvc_from_guest_exception_vector",
        "vmm-backend hvf::tests::failed_guest_run_retires_uncounted_progress",
        "vmm-backend hvf::tests::rejected_policy_preserves_live_identity",
        "vmm-backend hvf::tests::failed_policy_write_retires_mutated_vcpu",
        "vmm-backend hvf::tests::failed_fault_completion_retires_partly_changed_vcpu",
        "vmm-backend::arm64_sysreg_snapshot guest_system_registers_survive_restore_with_fresh_fallback",
        "vmm-backend::arm64_sysreg_snapshot guest_actlr_state_refuses_capture_and_clean_restore_resets_it",
        "vmm-backend::arm64_sysreg_snapshot guest_pauth_keys_refuse_capture_and_clean_restore_resets_them",
        "vmm-core::arm64_hvf_mdccint guest_mdccint_el1_survives_a_cold_public_snapshot_restore",
        "vmm-backend::arm64_sysreg_snapshot smccc_workaround_features_match_backend_levels",
        "vmm-backend::arm64_sysreg_snapshot psci_profile_matches_guest_hvc",
    ),
    "linux-aarch64": (
        "vmm-backend arm64_kvm::inventory::tests::live_inventory_identity_and_fixed_state_are_enforced",
        "vmm-core::arm64_executable_restore live::executable_ram_restores_match_guest_continuations",
        ARM64_BASELINE_TEST,
        "vmm-core vendor::arm64::dispatch::tests::live_in_kernel_preemption_matches_guest_acknowledgment",
        "vmm-backend::arm64_sysreg_snapshot guest_system_registers_survive_restore_with_fresh_fallback",
        "vmm-backend::arm64_restore_translation live::empty_dirty_detour_refuses_reuse_and_fresh_restore_discards_abandoned_translation",
        "vmm-backend::arm64_restore_translation live::executable_bytes_restore_requires_fresh_vm_after_warmed_detour",
        "vmm-backend::arm64_sysreg_snapshot guest_mdccint_survives_fresh_restore",
        "vmm-backend::arm64_sysreg_snapshot guest_gic_cpu_interface_survives_fresh_restore",
        "vmm-backend::arm64_sysreg_snapshot guest_gic_spi_trigger_survives_fresh_restore",
        "vmm-backend::arm64_sysreg_snapshot guest_gicr_bases_survive_restore_with_fresh_fallback",
        "vmm-backend::arm64_sysreg_snapshot guest_nonzero_spi_route_rejects_capture_and_zero_route_restores",
        "vmm-backend::arm64_sysreg_snapshot guest_physical_timer_state_refuses_capture_and_legacy_restore",
        "vmm-backend::arm64_sysreg_snapshot guest_timer_owned_ppi_line_refuses_terminal_cut_capture_and_restore",
        "vmm-backend::arm64_sysreg_snapshot guest_disr_state_refuses_capture_and_legacy_restore",
        "vmm-backend::arm64_sysreg_snapshot guest_os_lock_refuses_capture_and_clean_restore_unlocks",
        "vmm-backend arm64_kvm::tests::routed_away_pending_spi_refuses_capture_and_restore",
        "vmm-backend::arm64_sysreg_snapshot kvm_irq_mask_read_matches_full_capture",
        "vmm-backend::arm64_sysreg_snapshot smccc_workaround_features_match_backend_levels",
        "vmm-backend::arm64_sysreg_snapshot psci_profile_matches_guest_hvc",
    ),
    "linux-x86_64-hybrid": (
        "vmm-core vendor::x86::logical_identity_live_tests::public_snapshot_restore_rejects_a_different_host_core_class",
    ),
}

KVM_EXIT_BOUNDARY_TESTS = (
    "vmm-backend::kvm_smoke rejected_rep_input_retires_uncounted_guest_progress",
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
        "gicv3", "guest-image", "host-code-identity", "hypercall-doorbell", "hypercall-proto",
        "lapic", "nested-driver", "oci-support", "process-proto", "snapshot-store", "telemetry",
        "uml", "unison", "vm-state", "vmm-backend", "vmm-core", "vtime",
    )


CONSONANCE_CHECKS = Workflow(
    path=f"{WORKFLOW_DIR}/consonance-checks.yml",
    name="Checks / Consonance",
    owner="Consonance",
    triggers=("pull_request", "push"),
    jobs=(
        Job("Change Selection", "pr", 5, selects=("rust_checks",)),
        Job("Exact Runtime Artifacts", "pr", 45,
            exception="A cold build of the exact guest kernel and runtime exceeds "
                      "the pull request check budget.",
            select="rust_checks"),
        Job("Guest Memory", "pr", 15,
            crates=("guest-image", "oci-support", "nested-driver", "vmm-core"),
            test_targets=("vmm-core:linux_loader_proptest",),
            select="rust_checks"),
        Job("CPU State", "pr", 15,
            crates=("vm-state", "vmm-backend"),
            test_targets=("vmm-core:x86_cpu_snapshots", "vmm-core:arm64_skeleton",
                          "vmm-core:arm64_hvf_mdccint",
                          "vmm-backend:kvm_smoke"),
            ignored_tests=KVM_EXIT_BOUNDARY_TESTS + (
                "vmm-backend kvm_sys::xsave_diagnostic::changed_affinity_refuses_cpu_operations_without_mutation",
                "vmm-backend kvm_sys::policy_diagnostic::rejected_msr_filter_retires_partly_reconfigured_vcpu",
                "vmm-backend kvm_sys::policy_diagnostic::missing_cpuid_features_reject_before_policy_mutation",
                "vmm-backend kvm_sys::policy_diagnostic::missing_nested_cpuid_feature_rejects_before_mutation",
                "vmm-core::x86_cpu_snapshots live_kvm::production_cpuid_is_preserved_by_native_capability_admission",
                "vmm-backend kvm_sys::policy_diagnostic::rejected_vmx_capability_read_retires_partly_initialized_vcpu",
                "vmm-backend::contract_kvm *",
            ),
            scope="consonance_kvm",
            select="rust_checks"),
        Job("Device State", "pr", 15,
            crates=("lapic", "gicv3", "telemetry"),
            ignored_tests=(
                "gicv3 device::performance_tests::qualify_arbitration",
                "gicv3 device::performance_tests::qualify_delivery_cycle",
            ),
            select="rust_checks"),
        Job("Virtual Time", "pr", 15,
            crates=("vtime",),
            test_targets=("vmm-core:virtual_time", "vmm-core:x86_kvm_virtual_time",
                          "vmm-core:x86_kvm_linux_virtual_time"),
            ignored_tests=(
                "vmm-core::x86_kvm_virtual_time "
                "x1_ten_same_seed_runs_produce_one_normalized_log",
                "vmm-core::x86_kvm_linux_virtual_time x2_same_seed_boots_one_normalized_log",
            ),
            scope="consonance_platform",
            select="rust_checks"),
        Job("Snapshot and Restore", "pr", 15,
            crates=("snapshot-store", "unison"),
            test_targets=("vmm-core:snapshot_branch", "vmm-core:event_loop",
                          "vmm-core:arm64_executable_restore",
                          "vmm-core:x86_kvm_pcide_preflight"),
            ignored_tests=("vmm-backend::kvm_smoke save_restore_round_trips_on_real_kvm",
                           "vmm-backend::kvm_smoke sysenter_restore_rejects_values_kvm_cannot_preserve",
                           "vmm-backend::kvm_smoke cr8_restore_rejects_out_of_range_priority_before_mutation",
                           "vmm-backend::kvm_smoke rflags_restore_rejects_nonarchitectural_bits_before_mutation",
                           "vmm-backend::kvm_smoke rflags_vm_restore_rejects_real_mode_before_mutation",
                           "vmm-backend::kvm_smoke segment_encoding_restore_rejects_oversized_fields_before_mutation",
                           "vmm-backend::kvm_smoke segment_one_bit_restore_rejects_oversized_flags_before_mutation",
                           "vmm-backend::kvm_smoke xsave_reserved_header_restore_rejects_before_mutation",
                           "vmm-backend::kvm_smoke xsave_header_inputs_restore_rejects_before_mutation",
                           "vmm-backend::kvm_smoke event_flags_restore_checks_effective_set_mask_before_mutation",
                           "vmm-backend::kvm_smoke event_shape_restore_rejects_noncanonical_fields_before_mutation",
                           "vmm-backend::kvm_smoke event_dependent_fields_restore_rejects_unrestorable_values_before_mutation",
                           "vmm-backend::kvm_smoke unusable_segment_restore_requires_canonical_shape_before_mutation",
                           "vmm-backend::kvm_smoke nonnested_failed_restore_poisoned_after_partial_mutation",
                           "vmm-core::x86_kvm_pcide_preflight pcide_restore_requires_ia32e_mode_before_entry",
                           "vmm-backend kvm_sys::xsave_diagnostic::failed_dirty_drain_cannot_retry_after_prior_slot_was_cleared",
                           PUBLIC_SNAPSHOT_IDENTITY_TEST),
            scope="consonance_kvm",
            select="rust_checks"),
        Job("Execution Protocol", "pr", 15,
            crates=("consonance-client", "control-proto", "environment",
                    "execution-proto", "hypercall-doorbell", "hypercall-proto",
                    "process-proto"),
            test_targets=("vmm-core:protocol",),
            select="rust_checks"),
        Job("Guest Runtime", "pr", 15, select="rust_checks"),
    ),
)

CONSONANCE_ANALYSIS = Workflow(
    path=f"{WORKFLOW_DIR}/consonance-analysis.yml",
    name="Checks / Consonance / Analysis",
    owner="Consonance",
    triggers=("pull_request", "push", "schedule", "workflow_dispatch"),
    jobs=(
        Job("Change Selection", "pr", 5, selects=("miri_matrix", "kani")),
        Job("Miri — <Crate>", "pr", 30, select="miri_matrix",
            ignored_tests=("consonance-client cache::segments::tests::backing_length_borrows_the_descriptor_without_closing_it",),
            exception="The backend's interpreted snapshot/register inventory and "
                      "the Store's full resident-verification suite exceed 15 minutes; "
                      "other targets retain 15 minutes."),
        Job("Miri — <Crate> (Whole Crate)", "full", 320,
            ignored_tests=("consonance-client cache::segments::tests::backing_length_borrows_the_descriptor_without_closing_it",),
            exception="Interpreting a whole unsafe crate under Miri takes hours, "
                      "so pull requests get the tests the change reaches."),
        Job("Coverage", "full", 30,
            exception="An instrumented build and run of every Consonance crate "
                      "exceeds the pull request budget."),
        Job("Mutation Testing — Shard <N>/16", "full", 320,
            exception="Mutation testing rebuilds the component once per mutant."),
        Job("Proofs", "pr", 15, select="kani"),
    ),
)

CONSONANCE_NATIVE_MATH = Workflow(
    path=f"{WORKFLOW_DIR}/consonance-native-math-qualification.yml",
    name="Checks / Consonance / Native Math Qualification",
    owner="Consonance",
    triggers=("schedule", "workflow_dispatch"),
    jobs=(
        Job("Retained Native Math Executable", "full", 10),
        Job("Pinned Native Math — Replica <N>", "full", 10),
        Job("Bounded Native Math Results", "full", 10),
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
        Job("Retained Minimal Guest Executable", "full", 45),
        Job("Minimal Guest Determinism — Replica <N>", "full", 45,
            ignored_tests=("vmm-core::x86_kvm_virtual_time *",)),
        Job("Linux Guest Image", "full", 90),
        Job("Retained Linux Continuation Executable", "full", 45),
        Job("Linux Composed Continuation Export", "full", 30,
            test_targets=("vmm-core:linux_snapshot_continuation",),
            ignored_tests=("vmm-core::linux_snapshot_continuation composed_linux_snapshot_continuation",)),
        Job("Linux Composed Continuation Import — Replica <N>", "full", 15,
            ignored_tests=("vmm-core::linux_snapshot_continuation composed_linux_snapshot_continuation",)),
        Job("Linux Composed Continuation Results", "full", 5),
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
            ignored_tests=KVM_EXIT_BOUNDARY_TESTS + (
                "vmm-backend::kvm_smoke resume_flag_preserves_instruction_breakpoint_continuation",
                PUBLIC_SNAPSHOT_IDENTITY_TEST,
            ) + tuple(
                f"vmm-core::x86_cpu_snapshots live_kvm::{test}" for test in (
                    "mmio_rmw_finishes_before_full_vmm_snapshot",
                    "mmio_init_presence_snapshot_characterization",
                    "pae_pdpt_reload_preserves_snapshot_continuation",
                    "pae_cached_pdptrs_survive_full_vmm_snapshot_restore",
                    "amd_default_npt_pae_guest_write_observations",
                    "amd_pae_capture_refuses_and_retires_public_paths",
                    "long_mode_capture_preserves_continuation",
                    "xsave_guest_bytes_survive_cold_continuation",
                    "xsave_live_registers_survive_cold_continuation",
                    "xrstor_init_guest_bytes_survive_reused_continuation",
                    "xrstor_init_long_mode_guest_bytes_survive_reused_continuation",
                )
            ) + tuple(
                f"vmm-backend kvm_sys::xsave_diagnostic::{test}" for test in (
                    "snapshot_preparation_preserves_state_except_raw_presence",
                    "snapshot_preparation_raw_presence_stability",
                    "snapshot_restore_rejects_each_pending_state_without_mutation",
                    "snapshot_restore_drains_an_acknowledged_write_completion",
                    "ymm_without_sse_noninit_mxcsr_restore_rejects_atomically",
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

CONSONANCE_NESTED_HOST = Workflow(
    path=f"{WORKFLOW_DIR}/consonance-nested-host-qualification.yml",
    name="Checks / Consonance / Nested Host Qualification",
    owner="Consonance",
    triggers=("push", "schedule", "workflow_dispatch"),
    jobs=(
        Job("Nested Host", "full", 90,
            test_targets=("vmm-core", "vmm-core:x86_kvm_nested_host", "nested-driver:live",
                          "nested-driver:nested_restore", "nested-driver:sdk_operations"),
            ignored_tests=("vmm-core::x86_kvm_nested_host l1_creates_kvm_vm",
                           "vmm-core vendor::x86::contract::nested::tests::nested_restore_preserves_unsynchronized_vmcs_fields",
                           "vmm-core vendor::x86::contract::nested::tests::interrupt_raised_before_nested_entry_reaches_the_nested_host",
                           "vmm-core vendor::x86::contract::nested::tests::restore_before_nested_operation_onto_a_nested_host",
                           "nested-driver::live inner_consonance_runs_inner_guest",
                           "nested-driver::live inner_operation_api_smoke",
                           "nested-driver::sdk_operations outer_operation_sdk_smoke",
                           "nested-driver::nested_restore outer_nested_state_snapshot_matrix",
                           "nested-driver::nested_restore cold_snapshot_child",)),
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
        Job("Change Selection", "pr", 5, selects=("rust_checks",)),
        Job("Archive", "pr", 15, crates=("searcher",), select="rust_checks"),
        Job("Scheduling", "pr", 15, test_targets=("searcher:adaptive_durations",), select="rust_checks"),
        Job("Search Coordination", "pr", 15, test_targets=("searcher:interfaces",), select="rust_checks"),
        Job("Campaign Recording and Replay", "pr", 15,
            test_targets=("searcher:adaptive_campaign",),
            select="rust_checks"),
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
        Job("Change Selection", "pr", 5, selects=("rust_checks",)),
        Job("CLI", "pr", 15, crates=("harmony-cli",), test_targets=("harmony-cli:staging",), select="rust_checks"),
        Job("Consonance Host", "pr", 15, select="rust_checks"),
        Job("Consonance Host Arm64", "pr", 15, select="rust_checks"),
        Job("Consonance Host Public API", "pr", 15, select="rust_checks"),
        Job("NES Adapter", "pr", 15, select="rust_checks"),
        Job("Fault Injection", "pr", 15, select="rust_checks"),
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
        Job("Change Selection", "pr", 5, selects=("rust_checks",)),
        Job("Formatting, Boundaries and Tooling", "pr", 15),
        Job("Semantic Lints", "pr", 15),
        Job("Ignored Tests — <Host>", "pr", 15, select="rust_checks"),
    ),
)

DARWIN_RIGHTS_QUALIFICATION_TESTS = (
    "darwin_control_bound_recovers_all_installed_rights_before_protocol_refusal",
    "darwin_frame_remainders_own_and_refuse_unexpected_rights",
)

HARMONY_HOST_COMPATIBILITY = Workflow(
    path=f"{WORKFLOW_DIR}/harmony-host-compatibility.yml",
    name="Checks / Harmony Host Compatibility",
    owner="Harmony Host Compatibility",
    triggers=("pull_request", "push"),
    jobs=(
        Job("Change Selection", "pr", 5, selects=("rust_checks",)),
        Job("macOS Arm64", "pr", 15, select="rust_checks"),
        Job("macOS Consonance Host Package", "pr", 15, select="rust_checks"),
        Job("Linux Arm64", "pr", 15, select="rust_checks"),
        Job("Linux Arm64 Dissonance", "pr", 15, select="rust_checks"),
        Job("Linux Arm64 Qualification", "pr", 15, select="rust_checks"),
    ),
)

DISSONANCE_NES_CHECKS = Workflow(
    path=f"{WORKFLOW_DIR}/dissonance-workloads-nes-checks.yml",
    name="Checks / Dissonance Workloads / NES",
    owner="Dissonance Workloads",
    triggers=("pull_request", "push"),
    jobs=(
        Job("STB", "pr", 15, media=(".github/actions/stb-evaluation",), scope="dissonance_stb"),
    ),
)

DISSONANCE_TINY_WORLDS_CHECKS = Workflow(
    path=f"{WORKFLOW_DIR}/dissonance-workloads-tiny-worlds-checks.yml",
    name="Checks / Dissonance Workloads / Tiny Worlds",
    owner="Dissonance Workloads",
    triggers=("pull_request", "push"),
    jobs=(
        Job("Change Selection", "pr", 5, selects=("rust_checks",)),
        Job("World Mechanics", "pr", 15, crates=("tiny-worlds",), select="rust_checks"),
    ),
)

HARMONY_LANGUAGES_CHECKS = Workflow(
    path=f"{WORKFLOW_DIR}/harmony-workloads-languages-checks.yml",
    name="Checks / Harmony Workloads / Languages",
    owner="Harmony Workloads",
    triggers=("pull_request", "push", "schedule", "workflow_dispatch"),
    jobs=(
        Job("Change Selection", "pr", 5, selects=("harmony_languages",)),
        Job("Language Guest Runtime", "pr", 45,
            exception="A guest runtime missing from the cache is built from source once before bounded consumers.",
            select="harmony_languages"),
        Job("Language CLI Tools", "pr", 45,
            exception="The paired native frontend and static musl host are built once before bounded language consumers.",
            select="harmony_languages"),
        Job("Language Image — <Language>", "pr", 45,
            exception="A language layer missing from the cache is built from source, including CPython and OpenJDK.",
            select="harmony_languages"),
        Job("Check — <Language>", "pr", 15, select="harmony_languages"),
        Job("C Investigation", "pr", 35,
            exception="The bounded C branch and search investigation takes longer under live sealed-host code checks; it runs separately from the 15-minute language smokes.",
            select="harmony_languages"),
        Job("Language UML Profile", "pr", 45,
            exception="The pinned UML runtime is built before its bounded command replay consumer.",
            select="harmony_languages"),
        Job("UML Command Replay", "pr", 15, select="harmony_languages"),
    ),
)

HARMONY_OCI_CHECKS = Workflow(
    path=f"{WORKFLOW_DIR}/harmony-workloads-oci-checks.yml",
    name="Checks / Harmony Workloads / OCI",
    owner="Harmony Workloads",
    triggers=("pull_request", "push", "schedule", "workflow_dispatch"),
    jobs=(
        Job("Change Selection", "pr", 5, selects=("rust_checks",)),
        Job("Exact Runtime Artifacts", "pr", 45,
            exception="A cold build of the exact guest kernel and runtime exceeds "
                      "the pull request check budget.",
            select="rust_checks"),
        Job("PostgreSQL CLI Package", "pr", 45,
            exception="The paired native frontend and static musl host are built before the bounded PostgreSQL smoke.",
            scope="harmony_oci", select="rust_checks"),
        Job("Container Execution", "pr", 15,
            test_targets=("oci-support:platform", "oci-support:process_platform"),
            ignored_tests=OCI_PLATFORM_TESTS,
            scope="consonance_platform",
            select="rust_checks"),
        Job("PostgreSQL", "pr", 15, scope="harmony_oci", select="rust_checks"),
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
        Job("STB — <Difficulty>", "full", 210, media=(".github/actions/nes-film",)),
        Job("Results", "full", 5),
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

HARMONY_UML_CAMPAIGN = Workflow(
    path=f"{WORKFLOW_DIR}/harmony-workloads-uml-campaign.yml",
    name="Benchmarks / Harmony Workloads / UML",
    owner="Harmony Workloads",
    triggers=("workflow_dispatch",),
    jobs=(
        Job("Case Manifest", "full", 5),
        Job("Guest Runtime", "full", 90),
        Job("UML Profile", "full", 45),
        Job("Harmony", "full", 60),
        Job("Workload Image", "full", 90),
        Job("<Scenario>", "full", 360),
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
        Job("UML Runtime — <Architecture>", "full", 120),
        Job("Language SDK", "full", 15),
        Job("Publish", "full", 30),
    ),
)

CONSONANCE_UML = Workflow(
    path=f"{WORKFLOW_DIR}/consonance-uml.yml",
    name="Checks / Consonance / UML",
    owner="Consonance",
    triggers=("pull_request", "push", "workflow_dispatch"),
    jobs=(
        Job("Change Selection", "pr", 5, selects=("rust_checks",)),
        Job("UML Launcher", "pr", 15, crates=("host-code-identity", "uml"), select="rust_checks"),
        Job("UML Artifacts — <Architecture>", "pr", 45,
            exception="Two cold compilations of the pinned UML kernel and locked "
                      "toolchain exceed the pull request check budget.",
            ignored_tests=(
                "uml profile::tests::reviewed_ci_bundle_admits_only_its_approved_bytes",
                "harmony-cli::bin/harmony adapters::faults::runs::tests::loaded_reviewed_uml_profile_binds_the_recorded_kernel",
                "harmony-cli::bin/harmony adapters::faults::workflow::tests::native_uml_transaction_entry_is_refused_before_launch",
                "consonance-client session::uml::tests::reviewed_kernel_panic_is_a_public_client_crash",
                "uml::guest_exitcode guest_cannot_choose_host_reserved_exit_statuses",
                "uml::host_clone_exhaustion host_clone_quota_exhaustion_is_not_a_guest_fork_result",
                "uml::host_failure_provenance *",
                "uml::checkpoint_host_failure dirty_page_failure_after_a_published_checkpoint_is_infrastructure",
                "uml::fsgsbase_restore direct_fs_gs_bases_survive_reused_and_fresh_restore",
                "faults-workload consonance::infrastructure_tests::published_uml_host_failure_cannot_enter_a_campaign",
            ),
            select="rust_checks"),
        Job("UML Qualification — <Target>", "pr", 15, select="rust_checks"),
    ),
)

REPOSITORY_MAINTENANCE = Workflow(
    path=f"{WORKFLOW_DIR}/repository-maintenance.yml",
    name="Maintenance / Repository",
    owner="Repository",
    triggers=("schedule", "workflow_dispatch"),
    jobs=(
        Job("Prune Merged Branches", "full", 10),
    ),
)

WORKFLOWS = (
    REPOSITORY_CHECKS,
    HARMONY_HOST_COMPATIBILITY,
    CONSONANCE_CHECKS,
    CONSONANCE_ANALYSIS,
    CONSONANCE_HARDWARE,
    CONSONANCE_NATIVE_MATH,
    CONSONANCE_RUNTIME,
    CONSONANCE_NESTED_HOST,
    CONSONANCE_XSAVE,
    CONSONANCE_UML,
    DISSONANCE_CHECKS,
    DISSONANCE_ANALYSIS,
    HARMONY_CHECKS,
    HARMONY_ANALYSIS,
    DISSONANCE_NES_CHECKS,
    DISSONANCE_TINY_WORLDS_CHECKS,
    HARMONY_OCI_CHECKS,
    HARMONY_LANGUAGES_CHECKS,
    DISSONANCE_NES_BENCHMARKS,
    HARMONY_HISTORICAL_BENCHMARKS,
    HARMONY_UML_CAMPAIGN,
    RELEASE,
    REPOSITORY_MAINTENANCE,
)

# The NES composition. Native QuickNES exercises the Dissonance adapter. It
# needs a bounded check and a full benchmark.
NES_COMPOSITIONS = {
    "Dissonance Workloads": {
        "checks": DISSONANCE_NES_CHECKS.name,
        "benchmarks": DISSONANCE_NES_BENCHMARKS.name,
        "backend": "native",
    },
}

# How a workflow file shows which backend it executes a composition on. A
# composition keeps at least one marker for the backend it is registered with.
BACKEND_MARKERS = {
    "native": ("--runner quicknes", "nes-eval", ".github/actions/stb-evaluation"),
    "consonance": ("--backend kvm", "--backend hvf", "--backend uml", "--backend consonance", "--features consonance"),
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
    "host-code-identity": "Consonance",
    "vmm-core": "Consonance",
    "nested-driver": "Consonance",
    "hypercall-doorbell": "Consonance",
    "hypercall-doorbell-round-trip": "Consonance",
    "consonance-client": "Consonance",
    "vm-state": "Consonance",
    "vmm-backend": "Consonance",
    "snapshot-store": "Consonance",
    "harmony-supervisor": "Consonance",
    "uml": "Consonance",
    "machine": "Harmony",
    "nes-guest": "Harmony",
}

MIRI_ANALYSIS_WORKFLOWS = {
    "Consonance": CONSONANCE_ANALYSIS.name,
    "Harmony": HARMONY_ANALYSIS.name,
}

# Change-selection kinds the composite action accepts, and the workflows that
# use each one.
SCOPE_KINDS = {
    "dissonance_stb": (DISSONANCE_NES_CHECKS.name,),
    "harmony_oci": (HARMONY_OCI_CHECKS.name,),
    "harmony_languages": (HARMONY_LANGUAGES_CHECKS.name,),
    "consonance_platform": (CONSONANCE_CHECKS.name, HARMONY_OCI_CHECKS.name),
    "consonance_kvm": (CONSONANCE_CHECKS.name,),
    "kani": (CONSONANCE_ANALYSIS.name,),
    "miri": (HARMONY_ANALYSIS.name,),
    "miri_matrix": (CONSONANCE_ANALYSIS.name,),
    "rust_checks": tuple(workflow.name for workflow in WORKFLOWS
                         if any(job.select == "rust_checks" for job in workflow.jobs)),
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


def _cpu_list(text: str) -> set[int]:
    cpus: set[int] = set()
    for part in text.strip().split(","):
        first, separator, last = part.partition("-")
        lo = int(first)
        hi = int(last) if separator else lo
        if lo > hi:
            raise ValueError("invalid CPU range")
        cpus.update(range(lo, hi + 1))
    return cpus


def _hybrid_kvm_pair_available() -> bool:
    if not os.access("/dev/kvm", os.R_OK | os.W_OK):
        return False
    try:
        allowed = os.sched_getaffinity(0)
        core = _cpu_list(Path("/sys/bus/event_source/devices/cpu_core/cpus").read_text())
        atom = _cpu_list(Path("/sys/bus/event_source/devices/cpu_atom/cpus").read_text())
    except (AttributeError, OSError, ValueError):
        return False
    return bool(allowed & core) and bool(allowed & atom)


def this_host() -> str:
    """This machine as a `HOST_TESTS` key."""
    system = {"Darwin": "macos"}.get(platform.system(), platform.system().lower())
    machine = {"arm64": "aarch64", "AMD64": "x86_64"}.get(platform.machine(), platform.machine())
    host = f"{system}-{machine}"
    if host == "linux-x86_64" and _hybrid_kvm_pair_available():
        return f"{host}-hybrid"
    return host


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
