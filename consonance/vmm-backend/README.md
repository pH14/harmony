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

XSAVE captures retain component-presence bits even when a component contains
init values: clearing a bit can change the next guest XSAVE after cold restore.
Only ignored payload and host metadata are normalized. Captures that retain
previously discarded bits intentionally have different state hashes and cache
identities. Legacy snapshots remain readable, but bits already cleared by an
older writer cannot be recovered.

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
write sequence and error handling; the Nova restore oracle checks 200 restored
continuations across a branching snapshot tree on KVM.

This diagnostic branch invalidates cached translations at each ordinary entry
through a private VM ioctl in its pinned Linux 6.17 kernel. The ioctl refuses
hardware paging (NPT/EPT or the TDP MMU) before changing any state. The kernel holds the
memslot lock and calls `kvm_arch_flush_shadow_all`; it does not replace registers
or memory slots. Capture, completion-only calls, and internal retries do not
invoke it. An error or unexpected ABI reply poisons the backend. Stock kernels
lack this ioctl. This is an unqualified experiment, not a supported backend mode.
