# VM-exit-count virtual time

Virtual time is a deterministic accumulator advanced by the guest's normalized
VM-exit stream. It reads no host clock, CPU frequency, performance counter, or
instruction count.

Each architecture classifies its exits into frozen event classes. The run loop
adds the class's integer virtual-nanosecond duration after servicing the exit.
A scheduled interrupt becomes eligible at the first exit boundary whose
post-advance time reaches its deadline. When a guest is idle with a modeled
timer pending, the same clock advances directly to the deterministic deadline.

The design was brought up on macOS arm64 HVF, Linux arm64 KVM, and Linux x86 KVM.
The milestone evidence, planted negatives, normalized logs, checkpoint hashes,
and cross-host comparisons are recorded in
`docs/VM-EXIT-COUNT-VTIME-STATUS.md` and
`docs/PRESCRIPTIVE-VTIME-STATUS.md`. The current determinism argument and
support matrix live in `docs/DETERMINISM.md`.

The active implementation is `consonance/vtime`,
`consonance/vmm-core/src/virtual_time.rs`, and the two vendor dispatch trees.
Historical experiments and superseded clock designs remain available in Git
history and the status records.
