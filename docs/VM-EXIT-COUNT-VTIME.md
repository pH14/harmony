# VM-exit-count virtual time

Virtual time is a deterministic accumulator advanced by the guest's normalized
VM-exit stream. It reads no host clock, CPU frequency, performance counter, or
instruction count.

Each architecture classifies its exits into frozen event classes. The run loop
adds the class's integer virtual-nanosecond duration after servicing the exit.
A scheduled interrupt becomes eligible at the first exit boundary whose
post-advance time reaches its deadline. When a guest is idle with a modeled
timer pending, the same clock advances directly to the deterministic deadline.

Both architectures use one shared duration set. The rows are part of the CPU
contract — `docs/cpu-msr-contract.toml` on x86, `vendor/arm64/contract.rs` on
arm64 — and both vendors fold them into `contract_hash`, so a changed duration
refuses snapshots taken under the old clock. The guest kernel also rings an
execution tick (one exit per syscall entry, context switch, and idle-poll
iteration) so exit-free execution stretches still reach timer deadlines; its
duration is a contract row bounded below the guest's clockevent period.

The design was brought up on macOS arm64 HVF, Linux arm64 KVM, and Linux x86 KVM.
PR #235 records the completed milestone evidence: planted negatives, normalized
logs, checkpoint hashes, cross-host comparisons, and the exact guest-image
inputs. The final x86 execution is preserved in
[Actions run 33343244890](https://github.com/pH14/harmony/actions/runs/33343244890).
The standing determinism argument, confinement rules, and support matrix live in
[`DETERMINISM.md`](DETERMINISM.md).

The active implementation is `consonance/vtime`,
`consonance/vmm-core/src/virtual_time.rs`, and the two vendor dispatch trees.
Historical experiments and superseded clock designs remain available in Git
history and [PR #235](https://github.com/pH14/harmony/pull/235).
