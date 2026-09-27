<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# gicv3

`gicv3` is a pure, `no_std` userspace model of a single-vCPU GICv3 distributor,
redistributor, and EL1 virtual timer. Methods take the current V-time as an
argument; the model never reads a clock or depends on an architecture backend.

The model exposes frame-relative MMIO reads and writes, register files for the
implemented INTIDs, Group-1 arbitration, pending-to-active acceptance, EOI,
priority masking, and the virtual timer's compare value. Arbitration selects
the highest-priority deliverable interrupt and breaks ties by lowest INTID.
Timer deadlines are converted between counter ticks and V-time with integer
arithmetic.

`GicState` is the complete fixed-size snapshot record. Restore validates the
configuration and clears state beyond the configured SPI limit. The firing
deadline is derived from the current V-time and timer registers rather than
stored in the state record.

The model covers SGIs, PPIs, and configured SPIs for one security state and one
redistributor. Group 0/FIQ, LPIs, interrupt routing, SGI generation through
ICC registers, and real-guest delivery are outside this crate. `vmm-core`
wires it into the arm64 HVF composition; stock arm64 KVM uses its in-kernel
GIC.

## Arbitration cost

Active-interrupt acknowledgement walks only set bits inside the configured
interrupt range. It retains priority-first selection and lowest-INTID tie
breaking without inspecting every inactive interrupt slot. Queries read immutable
controller state and allocate no memory.

The unit qualification module keeps the previous scans as controls in the same
executable. Deterministic and randomized comparisons cover every supported
controller size, priority masking, pending and level inputs, active interrupts,
acceptance, EOI, and restored snapshots.

Pending arbitration selects the best enabled Group-1 candidate below PMR before
checking active priorities. An active interrupt that blocks that candidate also
blocks every worse candidate, so the check can stop at its first blocker. With
no candidate, or with PMR zero, no active-priority scan is needed. Candidate
iteration stays in increasing INTID order, accepts only a strictly better
priority, and stops at priority zero; lowest-INTID ties are therefore preserved.
Four-word bitmap groups skip empty ranges together, without retaining a cache.
`input_deliverable` keeps its existing independent single-input check.

Run the native qualification separately from other CPU-heavy work:

```sh
cargo test --release -p gicv3 --lib qualify_ -- --ignored --nocapture --test-threads=1
```

It alternates control/proposed order across nine pairs for 14 workloads and
three controller sizes (32, 96, and 992 interrupts). The workloads include no
pending work, a single timer, nesting, dense pending/active sets, priority ties,
masked priorities, level-only inputs, and winners or active interrupts in the
last bitmap word. A delivery-cycle check also compares every returned interrupt
and the complete final snapshot, with each arm starting from a fresh controller.
Wall-clock measurements are confined to these ignored tests and never influence
the modeled state. The Device State CI job runs both qualifications in release mode to check the
controls and endpoints; its shared-runner timings are informational. There are
no timing assertions in CI.
