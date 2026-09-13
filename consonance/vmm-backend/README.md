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
control. It then starts three fresh VMs with guest `FNINIT` before the same
MMIO boundary and integer setup followed by guest `XSAVE` before any further
x87 operation: unobserved, two repeated boundary GETs, and GET/SET/GET. These phases retain
raw and canonical guest and vCPU images, including restore-BV values, so raw
presence differences remain visible. The new phases seed a known x87 payload
with restore presence before the guest's `FNINIT`; the report records that
input separately from the guest's initialized control and tag state. The
report is evidence about the host/KVM path and does not change snapshot
semantics.

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
