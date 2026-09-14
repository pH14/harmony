<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# vmm-backend

`vmm-backend` owns the substrate boundary below `vmm-core`. The `Backend`
trait maps guest memory, runs a vCPU, exposes normalized exits, completes
read/write operations, injects events, and saves/restores vCPU state. It is
generic over an architecture type, so the upper VMM does not name KVM, HVF, or
an ISA-specific exit enum.

## Implementations

- `MockBackend` and `MockArm64Backend` provide scripted, deterministic
  backends for portable tests behind the `mock` feature.
- `KvmBackend` is the stock Linux x86-64 KVM implementation.
- `Arm64KvmBackend` and `HvfBackend` implement the arm64 KVM and macOS
  Hypervisor.framework paths where their platform APIs are available.

Backends install a guest-visible CPU policy before the first run. Read-style
exits require the matching completion response. The x86 KVM backend completes
PIO and MSR callbacks eagerly with an immediate-exit entry. `finish_exit` also
completes MMIO callbacks and returns any further device access required by the
same instruction. Callers service these continuations until `finish_exit`
returns `None` before exposing a stopped execution. No completion entry executes
the successor instruction or injects an interrupt; snapshot capture performs no
entry at all. An MSR fault queues its exception without executing the handler.

KVM construction enables the exception-payload API so pending exceptions remain
distinct from injected ones and restores replace the complete exception record.
Snapshots retain general registers verbatim, including `RFLAGS.RF`: that flag
suppresses the next instruction breakpoint and can change the continuation.
The live resume-flag test compares original, saved, and cold execution against
an RF-cleared control that must enter the guest debug handler.

Exit counters include continuation accesses exactly once. Virtual-time policy,
device models, snapshot formats, and entropy live above this crate.

ARM64 snapshot capture rejects a pending exit or staged completion before it
reads the vCPU. Completed MMIO reads and eagerly completed MMIO writes remain
capturable; placeholder ARM64 sysreg exits remain uncapturable while their
completion is pending.

The `contract-tests` feature exposes the shared backend contract exam, and the
`mock` feature enables portable fixtures:

```sh
cargo test -p vmm-backend --features mock,contract-tests
cargo clippy -p vmm-backend --all-targets -- -D warnings
```

KVM restoration invalidates cached guest translations before resuming a reused
VM. The restore sequence writes a transient CR0 write-protection value and then
the exact saved special registers, without entering the guest between writes.
This forces KVM to reset its MMU context after the caller replaces page-table
memory while retaining the same paging registers. Linux performs this reset
conditionally in [`__set_sregs2`](https://github.com/torvalds/linux/blob/v6.12/arch/x86/kvm/x86.c#L11986). Synthetic tests check the
write sequence and error handling; KVM integration coverage exercises restored
continuations across a branching snapshot tree.

The HVF state oracle uses the default policy with virtual timer masking enabled
and a zero timer offset. It round trips valid general, SIMD/floating-point,
system-register, debug, timer, and pending-interrupt records, and rejects
unmasked, nonzero-offset, or reserved timer-control states before mutating the
vCPU. ARM KVM and HVF expose pure restore-shape checks through the `Backend`
trait, so portable snapshot import rejects their known invalid vCPU records
before guest RAM or backend state is changed.

The ignored Linux x86 test `kvm_sys::xsave_diagnostic::raw_xsave_presence_phases`
is a bounded XSAVE provenance diagnostic. Set `XSAVE_RAW_REPORT_DIR` and run
`cargo test --locked --release -p vmm-backend --lib kvm_sys::xsave_diagnostic::raw_xsave_presence_phases -- --ignored --exact --nocapture`
on a KVM host, using a fresh report directory for each run. It records complete raw `KVM_GET_XSAVE2` images at a stopped
MMIO boundary, canonicalized copies, a raw SET/GET round trip, and one HLT
continuation for an initialized SSE case and an otherwise identical zero-SSE
control. It then runs two sets of three fresh VMs with guest `FNINIT` before the same
MMIO boundary and integer setup followed by guest `XSAVE` before any further
x87 operation: unobserved, two repeated boundary GETs, and GET/SET/GET. These phases retain
raw and canonical guest and vCPU images, including restore-BV values, so raw
presence differences remain visible. The FNINIT observation sets use two
separate fresh-VM x87 cohorts with the same `XSTATE_BV=3`, restore presence,
`FCW=0x037f`, zero status, and nonempty tags before `FNINIT`: a
payload-preservation cohort seeds ten `0xa7` bytes in each ST slot with zero
padding, while a zero-state cohort seeds ten zero bytes with zero padding. The
first cohort records whether an owned payload survives the guest's `FNINIT`;
the second supplies an init-valued x87 witness. A third fresh-VM cohort uses a
standard-format guest `XRSTOR` source image with mask `0x3`, `XSTATE_BV=2`,
`XCOMP_BV=0`, `MXCSR=0x1f80`, and the active XMM0 payload. Its 64-byte-aligned
source and guest `XSAVE` output buffers are disjoint, and it establishes x87
initialization through the absent x87 presence bit without executing `FNINIT`.
All cohorts have separate phase directories and comparison files, and each
report records its actual source image and seed. The report is evidence about
the host/KVM path and does not change snapshot semantics.
The fourteen-phase diagnostic also runs an early-boot integer cohort in three
independent fresh VMs: no boundary observation, repeated GET-only observation,
and GET/SET/GET observation. These VMs start with requested `XCR0=1`, clear
`CR4.OSXSAVE` and `CR4.OSFXSR`, and restore a canonical architectural-init
XSAVE image with requested restore presence `Some(2)`. Their guest executes
only the existing integer MMIO write and HLT sequence, with no guest FPU,
XSAVE, or XRSTOR instruction. Each phase records the requested XCR0/CR4 and
XSAVE presence, every observed raw/canonical image, the actual endpoint tuple,
and pairwise comparison metadata without requiring hardware presence to remain
`2`.

The ignored Linux x86 test
`kvm_sys::xsave_diagnostic::x86_pae_sregs2_phase_observations` is a bounded
PAE cache diagnostic for issue [#314](https://github.com/pH14/harmony/issues/314).
Set `PAE_PHASE_REPORT_DIR` and run
`cargo test --locked --release -p vmm-backend --lib kvm_sys::xsave_diagnostic::x86_pae_sregs2_phase_observations -- --ignored --exact --nocapture`
on a KVM host, using a fresh report directory for each run. It starts the existing guest witness three times and arms each
phase after a common pre-write `KVM_GET_SREGS2` and the common `KVM_GET_REGS`
RIP check at the guest-written PDPT B boundary: no SREGS2 read at B, one raw
GET at B, or one raw GET followed by raw SET of the pre-write-A record. The
x86 backend auto-completes the visible PIO boundary before the phase arm; the
report labels backend-visible exits separately from completion checkpoints.
AMD NPT is the target observation path and Intel EPT is a control path. Raw
SREGS2 bytes, flags, cached PDPTRs, CR0/CR3/CR4, PDPT RAM, UART, and RIP are
retained. The result is host/KVM observation evidence and is not a consistency
proof or a change to restore semantics.
The x86 workflow runs this as a continue-on-error informational step on every
supported NPT/EPT host, including when the required snapshot gate fails, and
keeps its report with the existing snapshot artifacts.


## Preparing x86 KVM snapshot boundaries

`Backend::prepare_snapshot` reconciles backend execution state before a logical
boundary is published. The default implementation is inert. KVM uses a guarded
`KVM_RUN` with `immediate_exit` after all userspace I/O completions have retired.
It requires `EINTR`, refuses staged/queued completions and armed synchronized
register inputs, and clears its immediate-exit request on success or error.
It mirrors CR8 from the current CPU state into the shared run page before entry;
restore also synchronizes that field, preventing a stale run page from replacing
restored CR8. This path does not inject queued interrupts or count a guest exit.

The FPU load/save round-trip makes captured XSAVE presence bits reflect the
hardware representation. No bits are removed from snapshot identity. `save()`
and hashing remain reads; callers prepare a boundary explicitly after restoring
RAM and CPU state or servicing an exit. Pending CPU events remain present;
unretired userspace instruction completion is a different condition and must
not be consumed by preparation. KVM may refresh shared run-page output metadata.
The tentative supported execution requirement is a single host core type for all
related boots, forks, and restores. On hybrid Intel Linux hosts, launch the worker
under `taskset -c <pool>` or a cpuset containing only P-cores or only E-cores.
Choose the same pool type for every process participating in a snapshot lineage.
The backend checks the creating thread's affinity against Linux's `cpu_core/cpus`
and `cpu_atom/cpus` masks and rejects mixed or uncovered masks when those masks
are exposed. It does not choose a pool or change caller affinity. Preserve that
affinity for the backend lifetime; externally changing affinity or CPU topology
requires stopping and requalifying the worker. The check is admission-time only.
Hosts that hide hybrid topology, including nested VMs, require operator
qualification of the underlying scheduling placement; absent masks do not prove
homogeneity. Snapshot data does not yet encode a host core-type admission token,
so cross-process and cross-host compatibility remain deployment requirements.

On the measured Core Ultra 9 285HX, preparation on a P-core retained presence `2`,
while moving the same vCPU to an E-core changed it to `3` with no guest instruction.
Within each CPUID-verified core-type pool, 15,600 complete-state comparisons passed
across seeds `0`, `2`, and `3`, including repeated restores. Fixed-core guest XSAVE
and full-VMM continuation tests passed on both types. This supports the tentative
restriction; it is not a universal hardware or extended-state qualification.

## XSAVE entry differential

`snapshot_entry_restores_match_uninterrupted_execution` compares independently
prepared initial states, repeated capture, capture-and-continue, cold restore,
and restore into a vCPU dirtied by another continuation. It covers presence
seeds 0/2/3 and XCR0 3/7 with HLT, XSAVE, and XRSTOR-to-init followed by XSAVE.
Every endpoint comparison includes the complete modeled CPU state, raw restore
presence, RAM (including guest-written XSAVE bytes), and exit counts. A changed
RBX and nonzero XMM0 are the negative control; the XSAVE case must also distinguish
the guest output itself. `XSAVE_ENTRY_REPORT_DIR` must name a fresh directory;
raw KVM XSAVE images, state dumps, RAM, and each comparison result are retained.
The matrix continues through comparison failures to report all seed/mode cases.
It does not assume raw presence stays constant across actual guest execution.

The additional `snapshot_entry_debug_reentry_preserves_guest_observation`
diagnostic inserts a hardware execution breakpoint between guest XRSTOR-to-init
and XSAVE. It retains the stopped and resumed state and compares the complete
endpoint, including guest RAM, with uninterrupted execution. This intervention
is not yet qualified as equivalent to arbitrary host interruption. The hardware
gate runs both this diagnostic and the entry/restore differential on one fixed
allowed CPU, alongside the original unrestricted differential, to distinguish
placement effects without replacing first-failure evidence.

The current same-core-type restriction is insufficient for every XSAVE sequence:
a fixed P-core debug-entry witness changes the guest-written header, and hosted
AMD reuse can change that header despite equal final modeled CPU state. Snapshot
consistency remains under investigation; preparation is not a proven universal
fixed point.

A separate test-only `XSAVE_ENTRY_WARMUP` arm executes guest XRSTOR-to-init,
XSAVE, and HLT once in each fresh fixture, then restores the intended initial
CPU state and all RAM before any reference execution. It tests whether a first
guest FPU transition explains the AMD fresh/reused difference. Production
construction does not execute this warmup; the original differential remains
unchanged when the variable is absent.

`XSAVE_ENTRY_LONG_MODE` selects a separate fixed-CPU control with identity-mapped
64-bit code and XSAVE64/XRSTOR64, matching the shipped Linux execution mode.
The original entry fixtures use real mode, so their failures alone do not
establish that the same instruction sequence diverges in 64-bit mode. Both
sets retain full CPU and RAM comparisons; the mode is recorded in CI evidence.

D1 selects one exact cohort with `XSAVE_ENTRY_CASE` and records a scoped tracefs
instance using `tests/xsave-exit-trace.sh`. Phase markers bracket each actual
guest run so in-kernel `kvm:kvm_exit` events can be assigned to reference, cold,
and reused execution. The trace includes the event format, guest RIP and exit
reason; missing tracing support is an unavailable diagnostic, not confirmation
of the proposed hidden nested-page fault. The trace instance is removed after
its contents are retained, including when the raw oracle fails.
