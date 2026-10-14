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

An x86 KVM entry failure retains the hardware failure reason and CPU reported by
`KVM_EXIT_FAIL_ENTRY`. The structured error reaches the caller and workload
failure trace, distinguishing invalid guest state from other entry failures.

Before the first policy is installed, `KvmBackend::nested_capabilities` selects
KVM-supported VMX or SVM. `initialize_nested` checks `KVM_CAP_NESTED_STATE` and
installs the requested vendor's CPUID model. VMX reads the declared capability
MSRs and sets IA32_FEATURE_CONTROL; SVM uses its supported CPUID capability leaf
and retains VM_CR and VM_HSAVE_PA through the native MSR filter and snapshot
MSR block. Partial MSR operations fail. The VMM's named nested-host policy binds
vendor and every exposed capability into contract identity.

Nested state is captured in a capability-sized initialized buffer. The complete
returned payload is retained with the inactive VMCS12 metadata normalization
described below. SVM uses KVM's format 1 and retains its GIF flag unchanged.
Outside L2 guest mode, SVM returns the 128-byte header; its VMCB is in guest RAM,
and EFER.SVME and the host-save MSR remain architectural state. Raw observations
can include an active L2 payload, but publication and restore require L1 outside
guest mode without a pending nested entry. The validator rejects unknown flags,
malformed vendor layouts and cross-vendor restore before mutation. Ordinary
backends neither capture nor accept nested state.

With a userspace LAPIC, KVM delivers a `KVM_INTERRUPT` vector to L2 whenever
L1 is in nested guest mode. KVM also reports an open interrupt window while
L1's VMLAUNCH, VMRESUME or VMRUN is still pending, even though L1 had
interrupts disabled. Before queuing a vector, the backend reads the nested
state's guest-mode flag. In guest mode the vector stays pending and the window
request is cleared, so a later exit from L1 outside guest mode delivers it.
That delay has no fixed bound. KVM marks a `KVM_INTERRUPT` vector as already
injected, so it re-injects the vector into L2 and never turns it into an exit
to L1. A window request kept in guest mode would arm interrupt-window exiting
in the L2 control state, and an L2 with interrupts enabled would then exit
before every instruction.

X86 restore uses the host Linux 7.1
[KVM selftest](https://github.com/torvalds/linux/blob/v7.1/tools/testing/selftests/kvm/lib/x86/processor.c)
dependency order: special
registers, MSRs, XCRs, XSAVE, MP state, debug registers, general registers,
events, then nested state. Event restoration follows general registers because
KVM_SET_REGS clears the exception queue. The selftest's earlier event write
loses pending #PF and #GP under the enabled exception-payload API; the existing
live exception-payload and serviced-MSR regressions exercise that difference.
EFER, FS base, and GS base occur in both the special-register record and the
stateful MSR block. Restore rejects mismatched copies during preflight, before
retiring a staged write completion or changing KVM state; otherwise the later
MSR write silently replaces the special-register value. Coherent copies remain
restorable and read back with the same values.
SYSENTER CS is a 32-bit VMCS field, while SYSENTER ESP and EIP are virtual
addresses. KVM can accept wider values through `KVM_SET_MSRS` but return a
truncated or sign-extended value on the next save. Restore preflight rejects a
nonzero upper half of CS and noncanonical ESP or EIP values before changing
the vCPU. KVM canonicalizes ESP and EIP at its host LA57 capability width,
independent of the guest CPUID width. SVM also discards their upper halves for
AMD-compatible guests. To cover older kernels, preflight requires 32-bit
values on SVM unless the guest advertises the GenuineIntel vendor.
CR8 holds a four-bit task-priority value. KVM also accepts larger values in
`KVM_SET_SREGS2` but reads them back changed, so restore rejects those values
during preflight.
RFLAGS bit 1 is fixed at one, while bits 3, 5, 15, and 22 through 63 are
reserved. KVM accepts a cleared bit 1 through `KVM_SET_REGS` and reads it
back set. It also accepts reserved bits but rejects guest entry with them set.
Restore preflight rejects both cases before applying any vCPU state.
The VM flag is valid only for virtual-8086 execution in protected mode; KVM
accepts it in real or long mode but fails the next guest entry. Preflight
rejects those mode combinations before restore.
CR4.PCIDE requires IA-32e paging: CR0.PG, CR4.PAE, and EFER.LMA must all be
set. KVM accepts PCIDE without that mode in `KVM_SET_SREGS2`, then fails the
next VM entry before the guest executes. Restore rejects the combination
during preflight; a valid long-mode PCIDE state remains restorable.
Segment type and DPL fields have four-bit and two-bit encodings. KVM accepts
larger values through `KVM_SET_SREGS2` and silently truncates them on the next
save. Restore checks all eight segments before applying any vCPU state.
The present, DB, S, L, G, AVL, and unusable flags are one-bit fields and receive
the same preflight check; KVM otherwise accepts larger values and reads them
back changed.
A segment with the present flag clear must be marked unusable. An unusable
segment keeps only its base and selector; its other fields are zero. KVM
normalizes other combinations during save, so restore checks this shape first.
The XSAVE header's reserved bytes 528 through 575 must be zero. KVM rejects a
nonzero byte at `KVM_SET_XSAVE` after earlier restore writes, so preflight
checks that range before changing the vCPU.
KVM restore also requires standard XSAVE layout, state bits from its supported
CPUID mask, and no reserved MXCSR bits. These inputs likewise fail at
`KVM_SET_XSAVE` after earlier writes, so preflight checks them first. The pure
XSAVE image helper can retain a compacted image for non-KVM callers; the KVM
restore path rejects it because its ioctl accepts standard layout only.
The vCPU event validity flags sent to KVM must match the flags KVM reports
for the VM. The restore adapter always adds the payload-valid bit. Without it,
KVM drops a pending exception and its payload; a typed snapshot may omit that
selector while retaining the same event state. Preflight compares the
effective KVM write mask before any state write.
Event preflight also requires zero software-interrupt and SIPI fields that KVM
never reports, an interrupt shadow using only the STI and MOV-SS bits, at most
two pending NMIs, and canonical one-bit event fields. It also rejects a triple
fault when the VM lacks that event capability. KVM otherwise accepts some of
these values and changes them on the next save.
KVM clears an exception payload marker when no exception is pending and ignores
an inside-NMI marker outside SMM. This backend uses a userspace LAPIC, so KVM
cannot retain a latched INIT event either. Preflight rejects those event forms
before any restore write and accepts KVM-captured canonical events.
A nested-host restore first installs the vendor's inactive nested state.
The live vCPU may be in L2 or in VMX operation, and KVM leaves nested
operation by loading VMCS12 host state or switching to VMCB01. Doing that
first keeps it from overwriting the restored registers, and lets a snapshot
taken before VMXON clear CR4.VMXE. On SVM the backend then clears EFER.SVME
before writing the saved EFER. Leaving nested operation keeps the host's VMCB02
and the last VMCB12 address, and the host reloads L2 segment, descriptor-table
and debug state from VMCB12 only when L1 marks it dirty. Restored guest RAM
rolls VMCB12 back while L1's clean bits stay set, so without that reset L2
would resume with values from after the snapshot. Clearing SVME frees VMCB02.
The complete nested payload is installed after all architectural state. Invalid format, size, mode flags, or contract
presence fails preflight before any restore ioctl. So does a VMXON or VMCS12
address beyond the guest's physical address width, which KVM derives from
CPUID leaf 0x80000008 and rejects only after the architectural state is written.

`Backend::drain_dirty_pages` returns the guest pages written since the last
drain, so snapshots copy and restores reload only those pages. The KVM backends
read the kernel's dirty log. `HvfBackend` maps guest RAM without write
permission. A guest write then exits as a data abort inside RAM, and the
backend records its 16 KiB page, grants write access with `hv_vm_protect`, and
reruns the store without surfacing an exit. A drain removes write access again
and reports each recorded page as four 4 KiB guest pages. Hypervisor.framework
reports these aborts with a translation fault status, so the backend treats any
lower-EL abort inside mapped RAM as a tracked write.

Nested-host dirty drains capture and validate the current nested payload before
clearing dirty bits, then remove and reinstall each outer RAM slot at the same
address and size before reloading the unchanged nested state. Outer restore also
reloads those slots after restoring RAM and before installing CPU state. This
invalidates cached nested mappings, including shadow pages that can survive
a nested-state reload, and starts a fresh dirty bitmap after consuming the old
one. The stopped vCPU never enters between slot removal and replacement.
Without that invalidation, incremental capture can omit L2 writes and restored
L2 execution can read stale translations despite exact outer RAM restoration.
The inner process and VM remain allocated. Ordinary guests use the kernel
bitmap directly. Portable tests and Miri cover payload and mapping preservation,
preflight before mutation, operation order and stopping on errors. The live
matrix checks all RAM bytes, while recorded search exercises longer histories.
Any error during an x86 KVM dirty drain, including a nested-state read or
validation failure, poisons that backend. A later slot failure may leave earlier
dirty bits cleared, so retrying cannot recover the complete page set. Errors
during any x86 CPU-state restore mutation also poison the backend.
Further entry, preparation, capture and restore fail; a deleted slot cannot
be reused after an incomplete reload. Portable fault injection exercises each
slot-removal/replacement error and the final nested-state installation boundary.

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
The live serviced-exit regressions require exact CPU state at the stopped
completion boundary. After guest continuation, they compare logical XSAVE
presence alongside every CPU field; the MMIO and MSR cases also compare guest
RAM bytewise. KVM can change raw presence bits for init-valued x87/SSE
components on entry without changing their state.

KVM construction enables the exception-payload API so pending exceptions remain
distinct from injected ones and restores replace the complete exception record.
Snapshots retain general registers verbatim, including `RFLAGS.RF`: that flag
suppresses the next instruction breakpoint and can change the continuation.
The live resume-flag test compares original, saved, and cold execution against
an RF-cleared control that must enter the guest debug handler.

Exit counters include continuation accesses exactly once. Virtual-time policy,
device models, snapshot formats, and entropy live above this crate.

ARM64 snapshot capture rejects a pending exit or staged completion before it
reads the vCPU. Completed MMIO reads and MMIO writes remain capturable;
placeholder ARM64 sysreg exits remain uncapturable while their completion is
pending. The ARM64 KVM backend leaves an MMIO write, or a read once its data is
supplied, for the next `KVM_RUN` to complete, so each access costs one entry.
`save` and `restore` first complete it with one immediate-exit entry, so the
captured registers are past the access and a restore never leaves a completion
to land on the restored registers. The backend counts and times those entries
and reports the totals through `Backend::store_completions`, since they are
host cost the exit table cannot see.
HVF rejects a restore with PSTATE bits above bit 31 before changing registers:
its CPSR setter accepts those bits but discards them on the next capture.
Its FPCR and FPSR setters likewise discard bits above bit 31, so restore
preflight rejects those values before changing the vCPU.

`Backend::read_irq_mask` optionally reads the CPU's architectural IRQ mask
without capturing a complete snapshot. `Some(true)` means the mask is set;
`None` means the caller must obtain it from full state. This query does not
enter the guest, acknowledge an IRQ, or test the interrupt controller's pending
state. Errors propagate rather than falling back to another read. HVF reads
`PSTATE.I` through one CPSR access. ARM KVM completes any owed MMIO access and
then reads PSTATE through one register ioctl instead of capturing the GIC and
the rest of the vCPU. Both reject pending completions before the read. Other
backends currently return `None`.
The boxed backend forwards the query so live sessions use the same path.

`Backend::read_arm64_gic_idle_state` reads only the in-kernel GIC fields needed
to decide whether a selected PPI can wake an idle ARM guest: distributor Group
1 enable, that PPI's group, enable, active, and priority fields, and the CPU
interface's mask, Group 1 enable, binary point, control, and active-priority
registers. ARM KVM uses this read after settling an owed MMIO access. It does
not capture a vCPU or scan SPI routes. Snapshot capture continues to reject
pending or asserted SPIs and nonzero SPI routes, while idle can still inspect
the clockevent PPI in those guest states. The boxed backend forwards the
targeted read, the ARM mock projects its held GIC state, and HVF and x86
backends return no in-kernel GIC state.

The ignored HVF `irq_mask_polling_matches_full_capture` comparison alternates
full capture and direct reads in one binary, both alone and after each exit of
a synthetic guest loop. It checks every guest output, RAM, exit counts, and
modeled CPU state except the host-driven virtual counter. Run it with:

```sh
cargo test --release -p vmm-backend --test hvf_smoke irq_mask_polling_matches_full_capture -- --ignored --nocapture
```

It reports local timings without a timing assertion. This isolates polling
cost; it does not measure whole-workload throughput or snapshot performance.

Both Linux KVM backends expose a cancellation latch for the session watchdog.
The watchdog interrupts a blocked KVM run with a signal and sets the latch;
the backend refuses subsequent guest entry after cancellation.

The `contract-tests` feature exposes the shared backend contract exam, and the
`mock` feature enables portable fixtures:

```sh
cargo test -p vmm-backend --features mock,contract-tests
cargo clippy -p vmm-backend --all-targets -- -D warnings
```

`tests/contract_kvm.rs` runs the same exam against stock KVM. The CPU State job
in `Checks / Consonance` runs it on the hosted runner's KVM.

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
a fault. The `ic ialluis` covers code the abandoned execution wrote and ran on
a page whose bytes the restore then left alone because they already matched
the snapshot. Host writes to guest RAM go through
`Backend::invalidate_instruction_cache`, which HVF clips to the host ranges it
mapped. Stage 2 never changes across a restore and needs no maintenance: the
host allocation behind guest RAM keeps its address.

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

The owned ARM guest cannot program its architected virtual timer controls.
Both backends set `CNTV_CTL_EL0` to zero before guest entry and reject an
enabled timer during capture or restore. This keeps a restored checkpoint from
arming a host-time interrupt outside the Harmony clockevent path.

ARM KVM and HVF expose pure restore-shape checks through the `Backend` trait,
so portable snapshot import rejects their known invalid vCPU records before
guest RAM or backend state is changed.
ARM KVM also rejects later guest entry, capture, restore and state observation
if a register or GIC write fails during restore: a partially changed vCPU must
not be reused as a coherent replay state. Shape rejection before the write
sequence leaves the backend usable.

`HvfBackend::set_policy` reads HVF's own value of each `ID_AA64*` feature
register before the first policy writes it, and fails with
`BackendError::IdRegisterAboveHost` when any 4-bit field of a policy asks for
more than that host value. The guest sizes its use of the hardware from these
fields, and Hypervisor.framework accepts any value written to them. ARM KVM
makes the same check inside `KVM_SET_ONE_REG`. Fields compare as unsigned
levels, except the signed fields `PFR0.FP`, `PFR0.AdvSIMD`, `DFR0.DoubleLock`,
`MMFR0.TGran64` and `MMFR0.TGran4`, where all ones means absent. `PFR0.GIC` is
not compared because the VMM implements the GIC system registers by trapping
them. The stage-2 and EL2 fields `MMFR0.TGran*_2`, `MMFR1.VMIDBits`,
`MMFR1.VH`, `MMFR2.FWB` and `MMFR2.EVT` are not compared because the guest runs
at EL1 and never uses them.

`Arm64Caps::asid_bits` reports the ASID width each backend's host fixes for the
guest. Hypervisor.framework implements 8-bit ASIDs, so HVF reports 8 bits. ARM
KVM leaves `MMFR0.ASIDBits` out of its writable ID register mask and fails
`KVM_SET_ONE_REG` with `EINVAL` for any value other than the host's, so
`LiveKvm::new` reads the vCPU's `ID_AA64MMFR0_EL1` after `KVM_ARM_VCPU_INIT` and
reports that width.

HVF answers `SMCCC_ARCH_FEATURES(ARCH_WORKAROUND_1/2/3)` as `1/-1/0`.
ARM KVM's responses to these three queries depend on the host's Spectre-v2,
Spectre-v4, and BHB states even when optional firmware bitmaps are disabled.
After applying the guest ID-register policy, ARM KVM reads its three firmware
workaround pseudo-registers and exposes their measured levels as backend
capabilities. HVF exposes the equivalent fixed levels `2/0/1`. Hosts with other
levels can boot, but the VMM binds those levels into snapshot compatibility and
state identity so different firmware answers cannot share a replay state. The
shared `arm64_sysreg_snapshot` hardware test executes all three queries in a
guest and compares their responses with the exposed levels on both hosts. The
owned arm64 Linux guest treats workaround 2's `-2` as unaffected by Spectre-v4
but `-1` as vulnerable; its frozen CPU identity has no SSBS feature, so this
answer controls retained mitigation state.

ARM KVM handles PSCI 1.0 HVCs in-kernel. HVF implements its own single-vCPU
PSCI handler. Their guest-visible answers differ: for example,
`PSCI_FEATURES(CPU_SUSPEND)` returns `0` on KVM and `-1` on HVF, and
`AFFINITY_INFO` for CPU 0 returns `0` on KVM and `-7` on HVF. Both backends
expose an explicit PSCI profile capability. The shared
`arm64_sysreg_snapshot` hardware test checks each profile against live guest
HVC results. This profile is a replay compatibility boundary, not a claim that
the two PSCI implementations have equal behavior.

ARM KVM captures and restores the GIC CPU interface through the VGIC device
registers: priority mask, both binary point registers, `ICC_CTLR_EL1`, both
group enables, and every available Group-0 and Group-1 active-priority register.
The number of active-priority registers follows the host's `PriBits` field.
KVM cannot expose all active-priority slots when the host reports eight
priority bits, so policy setup rejects that host before guest execution.
Restore checks the immutable `ICC_CTLR_EL1` fields before changing vCPU state;
those fields also enter the runtime contract. Full vCPU capture rejects
`ICC_CTLR_EL1.CBPR=1`, because KVM does not expose the independent Group-1
binary point hidden by that mode. This also makes run-end capture and state
hashing fail closed if a guest sets CBPR. The ignored
`guest_gic_cpu_interface_survives_fresh_restore` ARM KVM test exercises guest
writes, cold restore, and this fail-closed boundary.
The VGIC record also retains distributor SPI trigger configuration. Restore
checks the immutable redistributor SGI/PPI configuration before changing vCPU
state, then writes SPI configuration before the remaining distributor state.
Policy setup programs KVM's initially edge-triggered SPIs to the common level
baseline before first guest entry and verifies the device accepted the writes.
The fixed SGI/PPI configuration is edge/level, respectively, on the measured
ARM KVM host and in the userspace model.
The ignored `guest_gic_spi_trigger_survives_fresh_restore` ARM KVM test compares
guest readback of that baseline and of a programmed edge after uninterrupted
execution and cold restore.
The single-vCPU machine supports SPI routing only to affinity zero. The
userspace GIC rejects a nonzero `GICD_IROUTER` write. KVM can execute that
write inside its in-kernel VGIC, so capture reads both 32-bit KVM device-attribute
words of every implemented SPI's 64-bit route and refuses a snapshot if either
word is nonzero. Restore clears
and verifies all SPI routes, including on a reused target that previously ran
a guest with a nonzero route. The ignored
`guest_nonzero_spi_route_rejects_capture_and_zero_route_restores` test checks
guest readback, refusal, reused-target reset, and cold restore.
KVM's VGIC can retain a pending SPI outside its vCPU delivery queue after a
guest routes the SPI away, pends it, and routes it back. The same captured VGIC
registers can then report different `ICC_HPPIR1_EL1` results. KVM capture and
state hashing therefore refuse any pending SPI or asserted SPI input line, and
restore rejects such a KVM GIC record before changing the target. The userspace
GIC retains its own pending-SPI state. The ignored
`routed_away_pending_spi_refuses_capture_and_restore` ARM KVM test compares the
two live routing histories and verifies both refusal paths.

Both ARM backends capture and restore `CONTEXTIDR_EL1`, `AMAIR_EL1`,
`PAR_EL1`, `CSSELR_EL1`, `AFSR0_EL1`, and `AFSR1_EL1` alongside the
translation, exception, and thread registers. The shared
`arm64_sysreg_snapshot` hardware test compares uninterrupted execution, capture
without restore, restore after a detour, and restore into a fresh VM. It
requires the detour to change guest readback across an exit for CONTEXTIDR, PAR,
and CSSELR. AMAIR and auxiliary fault status can be fixed or lose writes across
an exit on a particular host; those cases are reported as unobservable rather
than evidence of register restoration. Portable tests independently check all
six fields through the KVM register table, codec, and VMM identity. The aligned
guest-memory fixture also runs under Miri through the mock backend.

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

## Live HVF tests

`tests/hvf_smoke.rs` runs small hand-assembled guests on Hypervisor.framework.
It checks that a restore drops guest translations cached before it, that a
restore puts the guest counter back, that saving and restoring between steps or
branching from a snapshot does not change what the guest computes, that every
vCPU state class reads back after a restore, that restore rejects timer states
HVF cannot hold, and that `set_policy` checks every policy against the host's
ID fields and rejects a field above the host. The tests are ignored because
hosted runners have no hypervisor. The pre-push hook runs them on Apple
silicon, from `HOST_TESTS` in `scripts/ci_contract.py`. To run them directly:

```sh
cargo test -p vmm-backend --test hvf_smoke -- --ignored
```

The repository's cargo runner for `aarch64-apple-darwin`,
`scripts/macos-hvf-runner.sh`, signs each binary with `hvf.entitlements.plist`
before it runs, so these tests and a `cargo run` of the CLI need no manual
`codesign` step.

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

KVM rejects a YMM-present/SSE-absent image with non-init MXCSR when publishing
a snapshot or before restoring the vCPU. Ordinary register reads and guest
execution remain available. A host using compacted restore can retain
MXCSR=0x3f80 in its KVM UABI buffer while guest entry
initializes live MXCSR to 0x1f80. A host using standard XRSTOR may restore that
state locally, but the snapshot would not be portable to a compacted host. The
ignored `ymm_without_sse_noninit_mxcsr_restore_rejects_atomically` regression checks
that admission boundary on a live KVM vCPU. It requires AVX. Images with init
MXCSR or SSE presence remain admissible.

`natural_avx_mxcsr_survives_capture_and_restore` requires long mode. The guest
sets nonzero YMM upper lanes with zero XMM lanes, loads MXCSR=0x3f80, and stops
at completed port I/O before capture. Both continued and cold-restored STMXCSR
results must equal 0x3f80. The observed raw presence bitmap is retained rather
than asserted to exclude SSE on every host.

The rejected imported-case observation is consistent with a compacted host restore:
[Intel SDM volume 1, sections 13.8.2, 13.11 and 13.12](https://cdrdv2-public.intel.com/868137/325462-089-sdm-vol-1-2abcd-3abcd-4.pdf)
specifies that compacted restore initializes MXCSR when SSE presence is clear,
while standard XRSTOR loads it when YMM is requested even if SSE presence is
clear.
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

The x86 raw ioctl adapters infer the libc request type so the same 32-bit KVM
request encodings compile under both glibc (unsigned long) and musl (int). This
allows the production backend to be linked into the static inner driver.

Nested capture canonicalizes inactive VMCS12 exit interrupt information. Linux
VMCS12 revision `0x11e57ed0` fixes this field at byte 816 of the VMCS payload.
When its valid bit is clear, the vector/type and error-code bytes have no active
architectural meaning and can retain a physical host interrupt vector. Capture
zeros that inactive information, and zeros an error code when its delivery bit
is clear. Valid interrupt information and all other payload bytes are preserved.
The revision and field bounds are checked before mutation; an unknown layout
fails capture. Snapshots, hashes and portable exports retain the complete
canonical payload. Dirty-log reprotection still reloads the unchanged raw
current payload. Portable tests cover inactive/valid cases, preservation of the
remaining bytes, and rejection without mutation, including under Miri.
