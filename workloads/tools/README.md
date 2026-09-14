# Workload validation tools

These composition binaries exercise the platform with concrete NES and Postgres
workloads. Build with `cargo build --manifest-path workloads/tools/Cargo.toml
--release`. The execution core remains independently buildable; tool-specific
startup and evidence conventions live here.

The `kvm_x86_nova_probe` accepts four required execution inputs:

```sh
cargo run --release --manifest-path workloads/tools/Cargo.toml \
  --bin kvm_x86_nova_probe -- \
  bzImage initramfs-oci.cpio.gz nes.oci nova.nes
```

Every mode prepares the generic NES OCI image and external ROM through
`nes-workload`. Ordinary Linux probes construct `consonance-client::Session`
and read the kernel-owned observation through `Session::read_observation`.
The bounded full-state restore oracle uses `ControlServer` directly to inspect
raw VMM state and portable artifacts, using the same prepared execution,
128 MiB RAM, command line, seed, and deferred checkpoint hashing configuration.
Its observation descriptor resolves the published handle to guest memory;
frame evidence comes from the SDK frame-complete lifecycle event, so boundary
checks do not add observation reads. Apple Silicon uses the direct server path.

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

On Apple Silicon macOS the same binary boots the arm64 platform runtime and
NES OCI execution through Hypervisor.framework. Build it for the native target
and pass the arm64 kernel, platform initramfs, NES OCI image, and Nova ROM:

```sh
cargo build --locked --release --target aarch64-apple-darwin \
  --manifest-path workloads/tools/Cargo.toml --bin kvm_x86_nova_probe
codesign --force --sign - \
  --entitlements consonance/vmm-backend/hvf.entitlements.plist \
  workloads/tools/target/aarch64-apple-darwin/release/kvm_x86_nova_probe
HARMONY_CONSONANCE_RESTORE_ORACLE=1 \
  workloads/tools/target/aarch64-apple-darwin/release/kvm_x86_nova_probe \
  path/to/Image path/to/initramfs-oci.cpio.gz path/to/nes.oci path/to/nova.nes
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

If an in-place history comparison fails, the report directory also receives
`in-place-history-root.bin` and `in-place-history-parent.bin`, which are exports
of the stored root/base and failing parent snapshots identified in
`in-place-history-failure.json`. The existing
`in-place-history-expected.bin` and `in-place-history-actual.bin` files retain
the first failing endpoint pair. The JSON records the tree seed, stable snapshot
IDs, failing edge and comparison, payload, expected and replay hashes, and the
ordered setup/tree/replay sequence through the failure. The root/base and parent
files describe stored snapshots; they are not live backend state captured before
the restore. These files are written only on the failure path, before any fresh
diagnostic restore changes the VM, and a filename collision is reported as an
error.

An opt-in replay can consume this retained report from a compatible Linux KVM
or Apple Silicon HVF boot. Preflight validates the report version, branch
protocol, complete edge table, ordered sequence, snapshot labels, and the
length and SHA-256 binding of every retained artifact before booting. The
replay imports the stored root snapshot, remaps its original IDs to newly
sealed IDs, and invokes the same branch, capture, continuation, and hashing
helpers for the recorded initial A/B setup, unsealed B replay, tree build, and
history sequence. It requires zero in-place fallbacks and stops at the first
hash mismatch, retaining the actual endpoint and a JSON description:

The preflight bounds the report at 1 MiB, requires the fixed 50-edge schema and
52 through 250 sequence entries, and bounds each retained artifact at the
configured 128 MiB probe RAM plus 8 MiB of format allowance.

The replay mismatch path also writes
`in-place-history-replay-actual-sidecar.bin`, produced by exporting the actual
stored snapshot with itself as the sparse base. It must contain no pages; its
length and SHA-256 are recorded in the failure JSON. The sidecar retains the
exact stored state suffix and metadata used by the snapshot hash path. It is a
hash preimage witness for offline comparison and is not asserted to reproduce
the earlier replay hash.

```sh
cargo build --locked --release --manifest-path workloads/tools/Cargo.toml \
  --bin kvm_x86_nova_probe
workloads/tools/target/release/kvm_x86_nova_probe \
  --replay-in-place-history path/to/bzImage \
  path/to/initramfs-oci.cpio.gz path/to/nes.oci path/to/nova.nes path/to/restore-artifacts
```

The mode is diagnostic and leaves the ordinary restore oracle unchanged. The
import replaces the stored memory and VM state when the first branch restores
it, but the disposable boot still establishes the destination VM shape. The
mode therefore verifies replay from a retained cold root on a compatible
configured VM; it does not reconstruct the source bootstrap process or claim
that differing kernel, initramfs, host, or backend inputs are identical.

The qualified D source snapshot and its continuation evidence can be exported
as an opt-in directory bundle. Set an explicit, operator-declared source commit
identity when exporting; this metadata records the claimed source identity and
does not replace binding the built executable to an immutable source archive
during qualification. The bundle also records the build feature set, ISA,
kernel and composed-initramfs SHA-256 digests, exact D branch payload, source boundary
evidence, and expected continuation time, ordered SDK events, raw state, hash,
and portable snapshot:

```sh
HARMONY_CONSONANCE_SOURCE_COMMIT=$(git rev-parse HEAD) \
HARMONY_CONSONANCE_D_BUNDLE_EXPORT_DIR=/path/to/d-bundle \
HARMONY_CONSONANCE_RESTORE_ORACLE=1 \
  workloads/tools/target/release/kvm_x86_nova_probe \
  path/to/bzImage path/to/initramfs-oci.cpio.gz path/to/nes.oci path/to/nova.nes
```

The composed initramfs digest binds the platform runtime, OCI workload, and
external ROM bytes. Qualification provenance should additionally record all
four original input digests. D children and replay/verification modes prepare
the same four inputs; the verifier checks the composed digest before booting.

Verify that bundle on a second host with the same ISA using the explicit CLI
mode. The verifier checks metadata, image digests, file digests, and portable
snapshot structure before booting the destination VM. It then imports the
recorded D source snapshot, replays the same typed continuation, and compares
time, ordered SDK evidence, raw state, whole-state hash, and portable execution
state with zero in-place fallbacks:

```sh
HARMONY_CONSONANCE_SOURCE_COMMIT=$(git rev-parse HEAD) \
  workloads/tools/target/release/kvm_x86_nova_probe \
  --verify-d-bundle \
  path/to/bzImage path/to/initramfs-oci.cpio.gz path/to/nes.oci path/to/nova.nes \
  /path/to/d-bundle
```

If verification finds a continuation mismatch, set
`HARMONY_CONSONANCE_ORACLE_REPORT_DIR` to retain the actual endpoint events,
raw state, whole-state hash, portable snapshot, metadata, and field-level
mismatch report alongside the expected bundle evidence.

The export is performed only after the existing D comparison succeeds, and
the ordinary oracle does not create a bundle unless its export variable is
set. A different ISA or unsupported KVM/HVF state contract produces an
explicit verification error.

### Candidate admission for the Linux x86 Nova A–E oracle

The default-session admission dump does not describe the direct `ControlServer`
A–E oracle. Before reviewing that execution, build the probe and export its named
candidate without booting a VM:

```sh
workloads/tools/target/release/kvm_x86_nova_probe --prepare-admission \
  KERNEL PLATFORM NES_OCI NOVA_ROM NEW_DUMP_DIRECTORY
python3 workloads/guest-images/verify-prepared-admission.py inventory \
  NEW_DUMP_DIRECTORY --output NEW_INVENTORY_DIRECTORY
```

The scope is `nova-ae-linux-x86_64-kvm-oracle-v1`. Boot and dump share RAM,
command line and seed configuration; the actual A–E run and dump also share the
setup payloads, default tree seed and x86 deadline constant. Compared with
`SessionConfig::default()`, the seed is `0x4e4f56415f434931` and deferred
checkpoint hashing is enabled. RAM remains 128 MiB and the command line is
unchanged. The oracle uses direct `ControlServer`, in-place restoration with a
remap factory, and an absolute virtual-time deadline; its serialized session
fields describe those boot parameters, not construction of a default Session.

The manifest binds the exact kernel, composed platform/rootfs/control bytes,
ROM, execution environment, oracle source file and executable. The executable
hash covers its compiled dependencies; the source-file hash alone does not
identify the complete transitive build source. Keep compiler and dependency
source provenance as separate review evidence. A rebuilt executable requires a
new candidate and same-executable verification. Host source changes remain
subject to normal PR review; they do not by themselves invalidate guest approval.

Inventory never approves a candidate. Verification requires a separately
reviewed composition baseline naming the same engine scope, the exact candidate
guest composition/configuration digest, reviewed component baselines, and the
executable that will run:

```sh
python3 workloads/guest-images/verify-prepared-admission.py verify \
  NEW_DUMP_DIRECTORY --output NEW_VERIFICATION_DIRECTORY \
  --baseline workloads/guest-images/admission/nova-oracle-composition.json \
  --oracle-executable workloads/tools/target/release/kvm_x86_nova_probe
```

The oracle baseline uses `composition_sha256` from the inventory report. It is
SHA-256 of the candidate manifest serialized with sorted keys and compact JSON
separators after removing only `oracle.source_sha256` and
`oracle.executable_sha256`. All guest inputs, configuration and control metadata
remain covered. The complete `manifest_sha256` still identifies the candidate
and its host provenance. Default-session baselines retain their existing exact
manifest binding.

The checked-in `workloads/guest-images/admission/nova-oracle-composition.json`
contains a separately reviewed oracle scope. It covers its exact guest bytes and
configuration; newly published kernel/platform bytes are not assumed equivalent
and fail pending review if their composition differs. Default-session baselines
cannot approve the oracle scope.

Both Nova A–E CI runs call
`workloads/guest-images/verify-nova-oracle-admission.sh` immediately before the
oracle. The gate asserts `HARMONY_CONSONANCE_RESTORE_ORACLE=1`, rejects even an
empty tree-seed override, dumps the actual built executable and input array, and
verifies the reviewed baseline against that executable. The same executable and
input array then run with the existing timeout (240 seconds in PR smoke,
600 seconds in the extended job). A failed dump or verification stops execution.

The gate creates large composition archives in a temporary directory, retains
only JSON metadata and logs under the uploaded report, and removes its temporary
files on exit. It does not enforce filesystem immutability between verification
and execution or gate unrelated probe invocations. Trusted code, no runtime code
mutation and the component admission constraints remain review obligations.
