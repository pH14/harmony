#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Examples pin the PR fan-out contract across component and composition boundaries."""

import unittest

import ci_contract
from ci_scope import SCENARIOS, selected


class ScopeTests(unittest.TestCase):
    def active(self, *paths):
        return {key for key, value in selected(paths).items() if value}

    def test_every_scenario_is_a_registered_change_selection_kind(self):
        for scenario in SCENARIOS:
            with self.subTest(scenario=scenario):
                self.assertIn(scenario, ci_contract.SCOPE_KINDS)
        registered = {job.scope
                      for workflow in ci_contract.WORKFLOWS
                      for job in workflow.jobs
                      if job.scope}
        self.assertEqual(set(SCENARIOS) | {"public_api", "kani", "miri"}, registered)

    def test_a_searcher_change_runs_both_nes_compositions_and_nothing_else(self):
        self.assertEqual(self.active("dissonance/searcher/src/search/archive.rs"),
                         {"dissonance_nes", "harmony_nes"})

    def test_stb_changes_select_their_own_game(self):
        for path in ("workloads/nes/src/stb/target.rs", "workloads/nes/src/bin/stb-probe.rs",
                     "workloads/nes/scripts/build-stb-rom.sh",
                     ".github/actions/stb-evaluation/action.yml"):
            with self.subTest(path=path):
                self.assertEqual(self.active(path), {"dissonance_stb"})

    def test_fault_changes_select_the_oci_composition(self):
        self.assertEqual(self.active("workloads/faults/src/target.rs"), {"harmony_oci"})

    def test_historical_publisher_and_report_select_the_oci_composition(self):
        for path in (".github/workflows/harmony-workloads-historical-bugs.yml",
                     "scripts/render-historical-bugs.py"):
            with self.subTest(path=path):
                self.assertEqual(self.active(path), {"harmony_oci"})

    def test_backend_changes_select_both_execution_scenarios(self):
        self.assertEqual(self.active("consonance/vmm-backend/src/kvm.rs"),
                         {"consonance_platform", "consonance_kvm", "harmony_nes", "public_api"})

    def test_the_shared_process_protocol_selects_its_consumers(self):
        self.assertEqual(self.active("consonance/process-proto/src/events.rs"),
                         {"consonance_platform", "harmony_nes", "harmony_oci", "public_api"})

    def test_the_nes_guest_runs_through_both_compositions(self):
        self.assertEqual(self.active("workloads/nes-guest/src/agent.rs"),
                         {"dissonance_nes", "harmony_nes"})

    def test_docs_and_report_tooling_start_no_scenario(self):
        self.assertFalse(self.active("consonance/vmm-core/README.md", "docs/WORKFLOWS.md",
                                     "benchmarks/search/eval.py"))

    def test_dependency_and_selector_changes_expand_conservatively(self):
        for path in ("Cargo.lock", "workloads/nes/Cargo.toml", "scripts/ci_scope.py",
                     "scripts/ci_contract.py", ".github/actions/platform-runtime/action.yml",
                     ".github/workflows/consonance-checks.yml"):
            with self.subTest(path=path):
                self.assertEqual(self.active(path), set(SCENARIOS) | {"public_api"})

    def test_the_shared_emulator_build_selects_every_source_built_game(self):
        self.assertEqual(self.active("scripts/build-quicknes-core.sh"),
                         {"dissonance_nes", "dissonance_stb", "harmony_nes"})


if __name__ == "__main__":
    unittest.main()
