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
PIO reads and MSR callbacks eagerly with an immediate-exit entry. A PIO write
leaves its completion staged and already acknowledged: `run()` accepts it
without a `finish_exit` call and the next real `KVM_RUN` retires it as a side
effect, so a write that never precedes a state read costs no extra entry at
all. `save`, `prepare_snapshot`, `restore`, and `retire_pending_completion`
retire an acknowledged write with one immediate-exit entry before they read or
replace the vCPU, so a restore never leaves an in-kernel completion to land on
the restored registers. The acknowledgement is derived from the decoded exit at
every decode site, so a device access surfaced by that drain is unacknowledged
like any other MMIO exit. An MMIO exit stays unacknowledged until
`finish_exit`, so `run()`, `prepare_snapshot`, and `restore` reject it with
`PendingCompletion` and no guest entry, and `save` reads the vCPU as it stands;
`finish_exit` completes the MMIO callback and returns any further device access
required by the same instruction. Callers service these continuations until `finish_exit` returns
`None` before exposing a stopped execution. No completion entry executes the
successor instruction or injects an interrupt. An MSR fault queues its
exception without executing the handler.

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

Both Linux KVM backends expose a cancellation latch for the session watchdog.
The watchdog interrupts a blocked KVM run with a signal and sets the latch;
the backend refuses subsequent guest entry after cancellation.

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

HVF restoration invalidates the guest's stage-1 translations before it writes
the restored vCPU state. Hypervisor.framework exposes no TLB call, so the
backend maps a private page above the guest's regions holding
`tlbi vmalle1is; ic ialluis; dsb ish; isb; hvc #0` and runs it with the MMU off. Without it
a restore rewinds guest RAM and registers while the hardware keeps translations
the abandoned execution installed, and the guest reads the wrong physical page
through an address the restored page tables map elsewhere. That reads as
narrow, register-shaped corruption in an arbitrary guest process rather than as
a fault. `hvf_tlb_probe` reads a page through a translation it warmed,
replaces the page-table entry, restores, and reads again; it fails when the
guest sees the old page. Its second stage repeats the sequence with a
guest-issued `tlbi` so a stage that cannot observe the replacement at all is
distinguishable from a stale translation. The `ic ialluis` covers code the
abandoned execution wrote and ran on a page whose bytes the restore then left
alone because they already matched the snapshot. Host writes to guest RAM go
through `Backend::invalidate_instruction_cache`, which HVF clips to the host
ranges it mapped. Both probe stages need a real
hypervisor, so they run on a host, not on a CI runner. Stage 2 never changes
across a restore and needs no maintenance: the host allocation behind guest RAM
keeps its address.

A watchdog cancellation requested before the stub's guest entry is consumed by
that entry and retires no instruction, so the stub entry is retried once. The
cancellation latch outlives the exit and `run` still refuses the guest.

The guest's virtual counter runs off the host counter and neither backend can
trap a guest read of it, so a restore that left the counter alone handed the
guest every tick of host time spent between the snapshot and the restore. Both
arm64 backends record the counter the guest was reading at the snapshot and put
it back on restore: HVF derives a CNTVOFF_EL2 from the host counter and ARM KVM
writes KVM_REG_ARM_TIMER_CNT. The counter keeps advancing with host time while
the guest runs, so a save taken after a restore reports a later value than the
one restored, and it stays out of the state hash and the divergence components
for that reason.

`hvf_counter_probe` restores one snapshot, reads the counter, burns a scaling
number of further restores of the same snapshot, and reads again; before the
rewind the second read ran ahead in proportion to the restores burned.
`hvf_roundtrip_probe` runs a loop of integer, memory and SIMD work either
straight through, with a save and restore between every step, or rebranched
from a mid-loop snapshot, and compares the accumulator the guest computed.

The HVF state oracle uses the default policy with virtual timer masking enabled.
It round trips valid general, SIMD/floating-point, system-register, debug,
timer, and pending-interrupt records up to the counter's advance, and rejects
unmasked or reserved timer-control states before mutating the vCPU. ARM KVM and HVF expose pure restore-shape checks through the `Backend`
trait, so portable snapshot import rejects their known invalid vCPU records
before guest RAM or backend state is changed.

ARM KVM saves the guest's system registers as the guest left them and never
normalizes a value the guest wrote. TCR_EL1.AS selects 8-bit or 16-bit ASIDs, and
while the identity baseline advertised 16-bit, clearing it left the guest kernel
issuing ASIDs the hardware no longer distinguished and processes shared TLB
entries. For SCTLR_EL1 and TCR_EL1, `KVM_SET_ONE_REG` and `KVM_GET_ONE_REG`
read and write the saved vCPU context rather than the architectural register, so
a restore cannot tell from those calls whether the host implements a field the
saved value uses. Restoring onto a host that lacks such a feature can therefore
resume the guest with that field reading zero; the feature identity registers
carry their own admission check, and these two do not. ARM KVM does no
stage-1 translation invalidation on restore either; whether it needs the
maintenance HVF needs is unmeasured, and an Arm KVM host is where that is
settled.

## Preparing x86 KVM snapshot boundaries

`Backend::prepare_snapshot` reconciles backend execution state before a logical
boundary is published. The default implementation is inert. KVM uses a guarded
`KVM_RUN` with `immediate_exit`. When a completion is already staged (a PIO
write left pending by the lazy path above), that same guarded entry retires it
first, so preparation and completion share one `KVM_RUN` instead of two. It
still refuses a pending read/MSR value awaiting an explicit completion and an
already-queued completion exit, and requires `EINTR` from the entry it does
run, clearing its immediate-exit request on success or error. It mirrors CR8
from the current CPU state into the shared run page before entry; restore also
synchronizes that field, preventing a stale run page from replacing restored
CR8. This path does not inject queued interrupts or count a guest exit.

The immediate-exit operation does not guarantee stable XSAVE presence bits,
either across repeated preparation or subsequent guest entry. Backend snapshots
retain the complete raw bitmap. The core layer projects only validated init
x87/SSE restoration metadata. The reproduced AMD failure and its contract are documented under
[Published XSAVE identity check](../vmm-core/README.md#published-xsave-identity-check). `save()`
retires an acknowledged write completion with the same guarded entry before it
reads and is otherwise a read; callers prepare a boundary explicitly after
restoring RAM and CPU state or servicing an exit. Pending CPU events remain present; a
staged write completion with no value left to supply is a different condition
and preparation consumes it as part of its own guarded entry, but a pending
read/MSR response awaiting an explicit value is not consumed and still fails
preparation closed. KVM restore retires an acknowledged write completion with
that guarded entry first and rejects pending read/MSR responses, unacknowledged
staged completions and queued completion exits before any CPU ioctl or state
mutation, since committing a stale exit's completion after restore would apply
it to the wrong instruction. A rejected restore leaves completion and interrupt
state intact. Raw backend save remains
available during exit servicing for CPUID resolution and tracing; it is not
itself a sealable snapshot boundary. Snapshot publication must enforce the
completion boundary at the VMM layer. KVM may refresh shared run-page output metadata.
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
check runs both this diagnostic and the entry/restore differential on one fixed
allowed CPU, alongside the original unrestricted differential, to distinguish
placement effects without replacing first-failure evidence.

The current same-core-type restriction is insufficient for every XSAVE sequence:
a fixed P-core debug-entry witness changes the guest-written header, and hosted
AMD reuse can change that header despite equal final modeled CPU state. Snapshot
consistency remains under investigation; preparation is not a proven universal
fixed point.

`XSAVE_ENTRY_LONG_MODE` selects a separate fixed-CPU control with identity-mapped
64-bit code and XSAVE64/XRSTOR64, matching the shipped Linux execution mode.
The original entry fixtures use real mode, so their failures alone do not
establish that the same instruction sequence diverges in 64-bit mode. Both
sets retain full CPU and RAM comparisons; the mode is recorded in CI evidence.

## Synthetic guest XSAVE canonicalization

The ignored Linux x86 test
`kvm_sys::xsave_diagnostic::canonical::snapshot_guest_canonicalization_preserves_complete_endpoints`
executes `xsave_canonical_guest.S` in long mode with XCR0=7. It requires AVX and
XSAVEC support and a fresh `XSAVE_CANONICAL_REPORT_DIR`; set
`XSAVE_ENTRY_LONG_MODE=1`, and run on one fixed
CPU. This is a test fixture, not a kernel or admission-policy change.

`snapshot_canonical_entry_restores_match_uninterrupted_execution` also applies
the same canonicalizer after the raw entry fixture's XSAVE, preserving its
capture, poison, cold restore and reused restore sequence. It covers XCR0=3/7,
each raw seed and both XSAVE-only and XRSTOR-then-XSAVE programs, with and without
prefaulting in required CI. It uses `XSAVE_ENTRY_REPORT_DIR` and long mode. Full
RAM, CPU state and exit counts remain byte-exact; a separate RAM poison keeps
the negative control observable after the canonicalizer clears scratch GPRs.
The original unmodified raw-entry diagnostic remains a distinct control.

Every execution first fills all 832 owned save bytes with a nonzero pattern,
then resets registers and selects init, SSE-active, AVX-active, or MXCSR-only
state. Standard XSAVE64 and compacted XSAVEC64 each have a raw control and a
guest canonicalizer. The canonicalizer materializes absent component init data,
clears presence for init payloads, fixes owned reserved/padding bytes, and records
an explicit format header. MXCSR is saved separately with STMXCSR: SSE presence
cannot determine its value, and compacted init saves may leave its slot unwritten.
Only the three enabled components are supported; for this fixed mask the AVX
payload offset is 576 in both layouts.

Both full-memory cold/reused restores and hardware-breakpoint interventions
compare complete RAM and modeled state only at the final HLT. Interventions
before save, after save, and inside canonicalization are not snapshot comparison
points. A register-value poison must change guest RAM, and every canonical
reference buffer is checked against an independently constructed expected image.
Raw controls report their RAM divergence count without requiring Intel hosts to
reproduce AMD tracking behavior. A zero raw count is not a reproduced negative
control on that host. Reports retain endpoint RAM, modeled state, raw
KVM_GET_XSAVE2, expected images, guest bytes, and breakpoint offsets. Broader host
qualification remains separate from this synthetic test.

The `XSAVE_ENTRY_PREFAULT=1` control populates the entire fixture RAM through
`KVM_PRE_FAULT_MEMORY` after vCPU configuration and host writes that
materialize every RAM page (avoiding shared zero-page CoW), without executing guest
instructions. This full-RAM oracle disables dirty logging before mapping in
prefault mode, because write-protected dirty-log mappings still fault on the
first XSAVE write. Production memory policy and the original control remain
unchanged. It retries partial progress and interruptions. Missing capability
or unsupported vCPU mode fails explicitly with `XSAVE_PREFAULT_UNSUPPORTED`;
that result does not qualify the fixture. Leaving the variable unset preserves
the unfaulted control. The ioctl creates stage-2 read mappings and does not break CoW or set accessed bits, so the
paired endpoint results must establish whether the XSAVE write fault disappeared;
successful prefault completion alone does not establish that result. Bounded CI
retains the single-cohort result and all restore/debug-entry prefault cohorts.

The non-default `xsave-diagnostics` feature exposes a one-shot hardware
execution breakpoint for native guest-kernel tests. KVM verifies the debug
exit RIP, disables debug controls, and resumes through ordinary entry handling.
The debug exit remains outside modeled exit counts and virtual time; an
unexpected debug exit fails. The diagnostic stores hit addresses only on the
host and does not rewrite guest registers or RAM. Default builds omit this
interface and dispatch branch.

Host standard-XSAVE normalization treats MXCSR separately from XMM registers:
Linux's KVM UABI materializes MXCSR when either SSE or YMM is present. With
SSE absent and YMM present, XMM bytes normalize to init while nondefault MXCSR
is preserved. The original restore bitmap remains separately bound. This does
not assert that raw guest XSAVE buffers with both bits absent are returned
unchanged by the host UABI.

The ignored `ymm_without_sse_uabi_mxcsr_bytes_survive_capture_and_restore` KVM regression
round trips a valid YMM-present/SSE-absent state with MXCSR=0x3f80, captures raw
KVM images, checks capture/restore byte preservation, and compares guest STMXCSR
after continued and cold-restored runs. The guest value is reported, not assumed
to match the imported MXCSR: a host may retain nondefault bytes in its KVM UABI
yet initialize live MXCSR on guest entry when SSE presence is absent.
It requires AVX and a fresh `XSAVE_MXCSR_REPORT_DIR`. `XSAVE_ENTRY_LONG_MODE=1`
selects the production-mode SIB-addressed STMXCSR instruction; otherwise it uses
the fixture's 16-bit real mode. This is a valid-state KVM round trip,
not a claim that every host naturally emits that presence bitmap.

`natural_avx_mxcsr_survives_capture_and_restore` requires long mode. The guest
sets nonzero YMM upper lanes with zero XMM lanes, loads MXCSR=0x3f80, and stops
at completed port I/O before capture. Both continued and cold-restored STMXCSR
results must equal 0x3f80. The observed raw presence bitmap is retained rather
than asserted to exclude SSE on every host.

The imported-case observation is consistent with a compacted host restore:
[Intel SDM volume 1, sections 13.8.2, 13.11 and 13.12](https://cdrdv2-public.intel.com/868137/325462-089-sdm-vol-1-2abcd-3abcd-4.pdf)
specifies that compacted restore initializes MXCSR when SSE presence is clear.
Compacted save includes SSE presence for nondefault MXCSR. On ms02, host boot
metadata confirms compacted FPU format; the natural test reports presence 6.
The imported presence-4 case therefore does not establish naturally occurring
register-value corruption. The precise host restore instruction was not traced.

Snapshot preparation coverage compares CPU state excluding only raw presence,
RAM, exit counts, readiness, pending events, and CR8 after each preparation.
Raw transitions are printed explicitly, and the preparation and restore guard
regressions retain their failure evidence. Production preparation is unchanged.

The canonical entry fixture requires exact equality at completed guest HLT
endpoints. Before the first instruction, exact identity comparisons are recorded
as diagnostics; setup still requires identical RAM, canonical CPU data, exit
counts and raw XSAVE bytes except x87/SSE presence bits whose canonical component
is init-valued. Active-component differences remain failures. Repeated capture
purity remains required. The XSAVE negative checks changed XMM output; the
XRSTOR negative supplies non-init MXCSR and checks the guest's saved MXCSR value,
without relying on an unrelated RAM mutation. This fixture distinction does not
change production snapshot identity or restore semantics.
