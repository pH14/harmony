// SPDX-License-Identifier: AGPL-3.0-or-later
#![no_std]
//! # pit — deterministic userspace i8254 PIT (the guest's clock-event)
//!
//! The missing primitive that gives real Linux a **tick**. The box proved that
//! Linux boots here in **virtual-wire APIC mode** (`APIC: ... Switch to virtual
//! wire mode`) and therefore registers **no clock-event device** — only the TSC
//! *clocksource*. With no tick, timer interrupts never fire, `sleep`/`nanosleep`/
//! futex-timeouts never wake, and `runc`'s Go runtime deadlocks. In virtual-wire
//! mode Linux's natural tick source is the legacy **8254 PIT** (always at I/O
//! ports `0x40`–`0x43`, needs no ACPI/MADT), so modelling a deterministic vPIT
//! makes Linux register a real periodic tick. This is exactly **Deterland**'s
//! design (Wu & Ford, *Deterministically Deterring Timing Attacks in Deterland*,
//! arXiv:1504.07070 §4.1.2): model a vPIT as the guest's fine-grained notion of
//! time, **disable the LAPIC timer**, and deliver the PIT's interrupts via the
//! performance-counter overflow + single-step machinery (§5.1) — the identical
//! mechanism `vmm-core`'s `run_until` (task 47) already implements.
//!
//! This crate is the **pure-logic** half, in the mold of [`lapic`] and `vtime`: a
//! deterministic state machine that takes **V-time in** (a `u64` nanosecond count
//! the caller supplies — it never reads a clock), **port reads/writes in** (the
//! guest's `IN`/`OUT` on `0x40`–`0x43`), and produces **the next IRQ0 deadline
//! plus a pending-IRQ0 signal out**. The vPIT state is a pure function of V-time +
//! the guest's port writes, so it is deterministic by construction: two same-seed
//! runs decrement at identical V-times and raise IRQ0 at identical V-times.
//!
//! ## What stays in `vmm-core`
//!
//! The KVM-facing glue is **not** here: routing the `0x40`–`0x43` port exits into
//! this crate, sourcing `preemption_deadline()` from [`Pit::next_irq0_deadline`],
//! and injecting IRQ0 (vector `0x30`) through the 8259 ExtINT path when the guest's
//! `RFLAGS.IF` is set all stay frontier in `vmm-core`. Counter 0's IRQ0 is an
//! **edge** the `vmm-core` interrupt-controller path latches and acknowledges
//! ([`Pit::irq0_pending`] / [`Pit::ack_irq0`]).
//!
//! ## The counter model (the heart of the crate)
//!
//! Three counters (0, 1, 2) at ports `0x40`, `0x41`, `0x42`, plus the mode/command
//! register at `0x43`. **Counter 0 drives IRQ0** — the system timer; counters 1
//! and 2 are modelled for read/program fidelity but raise no interrupt (counter
//! 2's GATE, port `0x61`, is tied high here — the speaker / counter-2 calibration
//! path is unused because the contract calibrates the TSC from CPUID 0x15). The
//! countdown decrements at [`PIT_FREQ_HZ`] (1.193182 MHz, the contract frequency)
//! of V-time. A counter write stores `reload` **and** `arm_vns = now_vns` (not a
//! precomputed deadline) — that is what makes the current-count read-back exact
//! for arbitrary V-time. The firing instant is the derived
//! `deadline = arm_vns + ceil(N · 1e9 / freq)` (**ceil**, so IRQ0 never fires
//! before `N` whole ticks elapse); the current count is computed from `now_vns` on
//! read, never decremented by a background tick.
//!
//! Modes: **0** (interrupt on terminal count, one-shot), **2** (rate generator,
//! periodic — the mode Linux's `0x34` clockevent programs), **3** (square wave,
//! periodic), **4** (software strobe, one-shot). Modes 1/5 are GATE-triggered and,
//! with counter 0's gate tied high, generate no interrupt. Counter latch and the
//! read-back command (count + status), BCD/binary, and the lobyte/hibyte access
//! modes are all modelled. All arithmetic is integer-only in `u128` intermediates,
//! saturating to `u64::MAX` / `u16` — `vtime`'s house style. There is no floating
//! point and no `HashMap`/`HashSet` reaching a snapshot byte, so identical inputs
//! yield bit-identical reads, deadlines, and [`PitState`].
//!
//! [`lapic`]: https://docs.rs/lapic

mod device;
mod error;
mod state;

pub use device::{Pit, PitConfig};
pub use error::PitError;
pub use state::{
    PIT_FREQ_HZ, PIT_PORT_COMMAND, PIT_PORT_COUNTER0, PIT_PORT_COUNTER1, PIT_PORT_COUNTER2,
    PIT_STATE_VERSION, PitCounterState, PitState,
};
