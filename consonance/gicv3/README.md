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
