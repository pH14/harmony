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
exits remain pending until the matching completion method is called; resuming
with an unserviced completion is an error. The x86 KVM backend finishes its
scalar PIO callbacks with an immediate-exit entry before a serviced operation
returns. This executes no following guest instruction and exposes the completed
CPU state to `save`; snapshot capture itself performs no entry. An unserviced
read still requires its host response. Exit counters and capability flags are
exposed for the VMM's reports. Virtual-time policy, device models, snapshot
formats, and entropy live above this crate.

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
