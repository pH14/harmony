# Workload validation tools

These composition binaries exercise the platform with concrete NES and Postgres
workloads. Build with `cargo build --manifest-path workloads/tools/Cargo.toml
--release`. The execution core remains independently buildable; tool-specific
startup and evidence conventions live here.

The `kvm_x86_nova_probe` restore oracle builds a 50-action branching snapshot
tree first checks the original 200 in-place restored continuations without
inserting cold restores into that history. Eight subsequent controls, including
the historical comparison-16 edge, import portable snapshots into independent
servers and compare their continuations, requiring zero in-place fallbacks.
Those controls compare raw VMM state and complete portable snapshot bytes,
covering VMST, guest RAM, service state, and control state. The backend README
documents the generic KVM restoration invariant; this workload-specific
validation remains with its tool.

Set `HARMONY_CONSONANCE_ORACLE_REPORT_DIR` to retain expected and actual raw
artifacts on a mismatch. The PR smoke enforces a 240-second execution bound and
requires all 200 history comparisons and eight complete-state cold controls.
For extended qualification, `HARMONY_CONSONANCE_ORACLE_TREE_SEED` selects a
fixed decimal seed for tree construction and edge replay. Leaving it unset
preserves the historical sequence; every success report records the seed.
