# Workload validation tools

These composition binaries exercise the platform with concrete NES and Postgres
workloads. Build with `cargo build --manifest-path workloads/tools/Cargo.toml
--release`. The execution core remains independently buildable; tool-specific
startup and evidence conventions live here.

Native AArch64 ARM oracle builds may opt into the host SHA-256 backend with
`--features arm-sha2-asm`. For example, append that feature to the
`workload-tools` build command when running the Nova oracle on an ARM host.
The default ARM build keeps its current software backend. The feature only
accelerates host hashing; it leaves the guest CPU contract, full-state checks,
and oracle coverage unchanged. Use it for native ARM builds, where the
`sha2` backend checks the host SHA2 capability and falls back to software when
the capability is absent.

The `kvm_x86_nova_probe` restore oracle builds a 50-action branching snapshot
tree, first checks the original 200 in-place restored continuations without
inserting cold restores into that history. Eight subsequent controls, including
the historical comparison-16 edge, import portable snapshots into independent
servers and compare their continuations, requiring zero in-place fallbacks.
Those controls compare raw VMM state and complete portable snapshot bytes,
covering VMST, guest RAM, service state, and control state. The backend README
documents the generic KVM restoration invariant; this workload-specific
validation remains with its tool.

On Apple Silicon macOS the same binary boots the arm64 Nova image through
Hypervisor.framework. Build it for the native target and pass the arm64
`Image-nova` and `initramfs-nova.cpio.gz` pair:

```sh
cargo build --locked --release --target aarch64-apple-darwin \
  --manifest-path workloads/tools/Cargo.toml --bin kvm_x86_nova_probe
codesign --force --sign - \
  --entitlements consonance/vmm-backend/hvf.entitlements.plist \
  workloads/tools/target/aarch64-apple-darwin/release/kvm_x86_nova_probe
HARMONY_CONSONANCE_RESTORE_ORACLE=1 \
  workloads/tools/target/aarch64-apple-darwin/release/kvm_x86_nova_probe \
  path/to/Image-nova path/to/initramfs-nova.cpio.gz
```

The HVF path keeps the eight cold controls in separate child processes because
Hypervisor.framework provides one VM per process; the root server and the D
source/destination controls retain their existing process identities.

After those controls, the oracle runs one additional comparison at the
historical comparison-16 edge. A reaches the boundary without an additional
oracle capture or state read at S, while normal production checkpoints remain
active, then captures only the continuation endpoint. B1 captures the boundary
once before the same continuation. B3 rebranches to the
same boundary, captures it three times, and takes the same continuation. C runs
a different branch before replaying B1's boundary snapshot, and D exports that
boundary to a private artifact from a source child process, waits for that
process to exit, then imports it into a separately spawned destination child
before continuing. D records both child PIDs and requires them to be distinct
from each other and the probe process. All five endpoints compare their
moment, SDK events, raw state, and whole-state hash. Same-history
captures require identical portable bytes; differing-history C and D
comparisons use the portable execution-state comparator and report both trace
counters. E then traverses the same 50-edge tree in reverse-sibling depth-first
order, remaps each parent to a newly sealed snapshot, and requires each hash
and complete portable artifact to match the original tree. This fixed
reordering covers 50 edges; broader randomized exploration remains outside
the bounded postpass.

Set `HARMONY_CONSONANCE_ORACLE_REPORT_DIR` to retain expected and actual raw
artifacts on a mismatch. The PR smoke enforces a 240-second execution bound and
requires all 200 history comparisons, eight complete-state cold controls, and
the single A/B1/B3/C/D/E postpass (`a_controls=1 b1_controls=1 b3_controls=1
b3_captures=3 c_controls=1 d_controls=1 d_processes=2 e_controls=50
e_reordered_positions>0`, with the two D child PIDs recorded in the success
line).
For extended qualification, `HARMONY_CONSONANCE_ORACLE_TREE_SEED` selects a
fixed decimal seed for tree construction and edge replay. Leaving it unset
preserves the historical sequence; every success report records the seed.
