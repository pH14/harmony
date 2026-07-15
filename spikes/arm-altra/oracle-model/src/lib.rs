// SPDX-License-Identifier: AGPL-3.0-or-later
//! The analytical taken-branch oracle for the arm64 spike payloads.
//!
//! `docs/ARM-ALTRA.md` §Evidence integrity #5 forbids judging count-exactness by
//! comparing one PMU reading against another — that is circular. Counts are
//! judged against payloads whose **taken-branch count is known by construction**.
//! This crate is that construction: it is the single definition of every payload
//! parameter (trip counts, PRNG seed, the branch-dense predicate) and of every
//! expected count, and it is compiled into *both* the bare-metal payload (which
//! takes its parameters from here, so the asm and the model cannot drift) and the
//! host-side harness (which predicts what the counter must read).
//!
//! # The model
//!
//! V-time on ARM counts `BR_RETIRED` (raw event `0x21`) = retired **taken**
//! branches (`docs/ARM-PORT.md`, `docs/ARM-ALTRA.md` §2 — the binding statement of
//! the event's semantics; nothing here invents it). Each payload runs a hand-written
//! asm body bracketed by two MMIO console stores, so the counting window contains
//! *exactly* that body — no compiler-generated code, no UART poll loop, no boot
//! code (see `payloads/README.md` §The counting window). Within a body every branch
//! instruction is explicit, so the count decomposes as
//!
//! ```text
//! measured = certain_taken                    // exactly derived, below
//!          + reported_taken                   // branches the payload counts and reports
//!          + w_entry * exception_entries      // <- unknown weights: MEASURED on
//!          + w_eret  * exception_returns      //    silicon by stage AA-1, never
//!          + w_svc   * svc_instructions       //    assumed here
//!          + w_wfi   * wfi_instructions
//!          + offset                           // <- the per-class constant offset
//! ```
//!
//! The **weights are the unknowns the spike exists to measure.** Whether an
//! exception entry, an `ERET`, an `SVC`, or a `WFI` is counted by `BR_RETIRED` on
//! Neoverse N1 is exactly the sort of microarchitectural fact the apparatus must
//! not guess: `docs/ARM-ALTRA.md` §Execution constraints ("never silently substitute")
//! and task 109's "no invented constants" rule. So [`Weights`] has **no `Default`**
//! and no inherent values — a caller must supply measured ones, and the floor
//! checker refuses to run without them.
//!
//! # Identifiability (why this payload set can actually solve for the weights)
//!
//! The set is chosen so the unknowns are separately identifiable from measurements,
//! rather than merely constrained:
//!
//! - [`Payload::StraightLine`] and [`Payload::BranchDense`] have **zero** ambiguity
//!   terms. Their measurements yield `offset` directly — and, being two different
//!   classes, they cross-check it. A *variable* offset between them is a mismatch,
//!   not a calibration (`docs/ARM-ALTRA.md` AA-1(a)).
//! - [`Payload::ExceptionAbort`] adds `N * (w_entry + w_eret)` and nothing else, so
//!   it yields the entry+return pair.
//! - [`Payload::Svc`] adds `N * (w_svc + w_entry + w_eret)`; differencing it against
//!   `ExceptionAbort` at equal `N` isolates `w_svc`.
//! - [`Payload::WfiIdle`] adds `N * (w_wfi + w_entry + w_eret)`; the same difference
//!   isolates `w_wfi`.
//!
//! Four unknowns, five independent equations: the system is **over-determined**, and
//! the residual is itself evidence. [`solve`] performs the solve and returns the
//! residual so a nonzero one is a finding, not a rounding error.
//!
//! # Untested on silicon
//!
//! Everything here is derivation. Nothing in this crate has been checked against a
//! hardware PMU. It is *validated* two ways offline — the derivation is
//! unit-tested, and the harness's branch scanner decodes the built payload ELF and
//! asserts the emitted branch sequence is exactly the one declared in
//! [`Expectation::inline_branch_seq`] — but a validated derivation is not a
//! measurement.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

#[cfg(feature = "std")]
use serde::{Deserialize, Serialize};

/// Multiplier of the xorshift64* scrambler (Vigna, "An experimental exploration
/// of Marsaglia's xorshift generators, scrambled"). The payload asm materializes
/// this exact constant; the model must agree bit for bit.
pub const XORSHIFT_MUL: u64 = 0x2545_F491_4F6C_DD1D;

/// Default PRNG seed, used when the harness has not written a params page (the
/// TCG smoke case). Nonzero, as xorshift64* requires.
pub const DEFAULT_SEED: u64 = 0x5EED_5EED_5EED_5EED;

/// Magic in the params page, spelling "HARM" little-endian.
pub const PARAMS_MAGIC: u32 = 0x4841_524D;

/// Params-page ABI version.
pub const PARAMS_ABI: u32 = 1;

/// Guest-physical address of the harness -> guest params page.
pub const PARAMS_GPA: u64 = 0x4000_0000;

/// Guest-physical address of the work-derived clock page
/// (`docs/PARAVIRT-CLOCK.md` ABI 1).
pub const PVCLOCK_GPA: u64 = 0x4000_1000;

/// `HARMONY_PVCLOCK_ABI` (`docs/PARAVIRT-CLOCK.md` §1).
pub const PVCLOCK_ABI: u32 = 1;

/// The work-derived clock page's byte layout and its pure page builder.
///
/// The layout is written by **two** parties — the harness publishes the *managed* page
/// ([`crate::PVCLOCK_GPA`], `arm-harness`) and, under TCG where the harness never runs,
/// the payload runtime self-seeds a fallback (`payloads/runtime/src/pvclock.rs`). Both
/// must agree on the offsets to the byte, so the offsets and the packing live here,
/// once, rather than in each writer. This module is also the payloads' one piece of
/// **Miri-checkable** byte logic: the page layout is pure `[u8]` packing (no MMIO, no
/// asm), so it is interpreted here — the runtime and harness only *blit* what this
/// builds. (Their remaining `unsafe` is `asm!` and fixed-GPA volatile access, which
/// Miri fundamentally cannot execute; see `payloads/runtime/src/lib.rs`.)
pub mod pvclock {
    /// `abi_version` (u32) — equals [`super::PVCLOCK_ABI`] on a valid page.
    pub const OFF_ABI: usize = 0x00;
    /// `seq` (u32) — odd while an update is in progress, even when stable.
    pub const OFF_SEQ: usize = 0x04;
    /// `vns` (u64) — materialized V-time in nanoseconds.
    pub const OFF_VNS: usize = 0x08;
    /// `guest_clock` (u64) — the materialized virtual counter.
    pub const OFF_GUEST_CLOCK: usize = 0x10;
    /// `guest_clock_hz` (u64).
    pub const OFF_HZ: usize = 0x18;
    /// `flags` (u32) — bit 0 is [`FLAG_MATERIALIZED`].
    pub const OFF_FLAGS: usize = 0x20;
    /// The page's used length in bytes.
    pub const PAGE_LEN: usize = 0x28;

    /// `flags` bit 0: the value is finished — do not interpolate against a live
    /// counter. Always set for ABI 1.
    pub const FLAG_MATERIALIZED: u32 = 1;

    /// `flags` bit 1: the value is **work-derived and refreshed** — the harness computed
    /// V-time and the virtual counter from the guest's `BR_RETIRED` work and updates the
    /// page as work advances. This — not merely a published static page — is what AA-5
    /// certifies. A static placeholder sets [`FLAG_MATERIALIZED`] but NOT this.
    ///
    /// This bit is **defined by ABI 1** (`docs/PARAVIRT-CLOCK.md` §1 flags row); it is not
    /// a reserved bit this spike consumed. The `hm-8h8` real stamping path publishes a
    /// materialized work-derived page — exactly this bit alongside bit 0 — so a conforming
    /// hm-8h8 page and this spike's reader agree by construction. The spike's own harness
    /// publishes only a static placeholder (bit 0, not this), so AA-5 reads unfulfilled
    /// until the work-derived path lands.
    pub const FLAG_WORK_DERIVED: u32 = 1 << 1;

    /// Build the clock-page bytes for a **stable** (even-`seq`) materialized page: the
    /// ABI marker, `seq = 2`, the given V-time/counter/frequency, and the flags. When
    /// `work_derived` is set, [`FLAG_WORK_DERIVED`] rides alongside [`FLAG_MATERIALIZED`]
    /// (an AA-5-certifying page); otherwise only [`FLAG_MATERIALIZED`] (a static
    /// placeholder). Everything else is zero. Little-endian, matching the guest's reads.
    ///
    /// This is the shared, Miri-interpreted layout; a writer either blits the result or
    /// (for the live seqlock protocol) writes these fields in odd→even order using the
    /// `OFF_*` offsets above.
    #[must_use]
    pub fn materialize(vns: u64, guest_clock: u64, hz: u64, work_derived: bool) -> [u8; PAGE_LEN] {
        let mut flags = FLAG_MATERIALIZED;
        if work_derived {
            flags |= FLAG_WORK_DERIVED;
        }
        let mut p = [0u8; PAGE_LEN];
        p[OFF_ABI..OFF_ABI + 4].copy_from_slice(&super::PVCLOCK_ABI.to_le_bytes());
        p[OFF_SEQ..OFF_SEQ + 4].copy_from_slice(&2u32.to_le_bytes());
        p[OFF_VNS..OFF_VNS + 8].copy_from_slice(&vns.to_le_bytes());
        p[OFF_GUEST_CLOCK..OFF_GUEST_CLOCK + 8].copy_from_slice(&guest_clock.to_le_bytes());
        p[OFF_HZ..OFF_HZ + 8].copy_from_slice(&hz.to_le_bytes());
        p[OFF_FLAGS..OFF_FLAGS + 4].copy_from_slice(&flags.to_le_bytes());
        p
    }
}

/// PL011 UART data register — the guest's one MMIO window, and therefore the
/// harness's counter-read point. QEMU `virt` maps the PL011 here; the harness
/// models it at the same GPA so payloads are byte-identical across both.
pub const UART_BASE: u64 = 0x0900_0000;

/// Console byte that opens the counting window (ASCII STX).
pub const MARK_BEGIN: u8 = 0x02;

/// Console byte that closes the counting window (ASCII ETX).
pub const MARK_END: u8 = 0x03;

/// The oracle payloads. One per class named in `docs/ARM-ALTRA.md` §Spike
/// architecture item 2 and stages AA-1/AA-2/AA-4/AA-5.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "kebab-case"))]
pub enum Payload {
    /// Capability report from inside the guest: MIDR, LSE, ECV, PMUVer, SVE.
    /// No counting window — it is the day-one liveness and AA-0/AA-6 witness.
    Ident,
    /// Long unbranched arithmetic runs; one back-edge per trip. The lowest
    /// branch density in the set.
    StraightLine,
    /// Seven data-dependent branches per trip across four encodings
    /// (`TBZ`/`TBNZ`/`CBZ`/`B.cond`). The highest branch density in the set.
    BranchDense,
    /// `SVC #0` into a one-instruction handler. The syscall class.
    Svc,
    /// A translation fault (load from an unmapped VA) into a handler that skips
    /// the faulting instruction. The exception entry/return class, at a
    /// different exception class (EC 0x25) than [`Payload::Svc`] (EC 0x15).
    ExceptionAbort,
    /// Deterministic idle: mask, arm the virtual timer, `WFI`, unmask — so the
    /// interrupt lands at an instruction fixed by construction, not by wall clock.
    WfiIdle,
    /// `LDXR`/`STXR` increment loop — AA-4's (a) payload. The one payload whose
    /// count is *not* fully known by construction: `STXR` failure is the hazard
    /// under study, so the retries are counted in-guest and reported.
    LlscAtomics,
    /// `LDADD`/`CASAL` increment loop — AA-4's (b) payload. Same semantics as
    /// [`Payload::LlscAtomics`], no retry term: deterministic by construction.
    LseAtomics,
    /// Seqlock reads of the work-derived clock page — AA-5's payload. Reads a
    /// materialized value; performs no counter arithmetic (`docs/PARAVIRT-CLOCK.md` §0).
    ClockPage,
    /// The **AA-5 Linux guest** determinism class — a full Linux VM, not a bare-metal
    /// oracle payload. It has no counting window and no by-construction count; it exists
    /// so the AA-6 determinism matrix can *represent and require* the Linux guest the
    /// binding matrix names (`docs/ARM-ALTRA.md` §AA-6). It is deliberately **not** in
    /// [`ALL_PAYLOADS`] (nothing here builds or smokes a Linux kernel); no run produces
    /// one pre-silicon, so requiring it keeps AA-6 honestly unfulfilled until arrival day.
    LinuxGuest,
}

/// Every payload, in a stable order. The manifest generator and the smoke script
/// both iterate this, so a new payload cannot be silently left out of either.
pub const ALL_PAYLOADS: [Payload; 9] = [
    Payload::Ident,
    Payload::StraightLine,
    Payload::BranchDense,
    Payload::Svc,
    Payload::ExceptionAbort,
    Payload::WfiIdle,
    Payload::LlscAtomics,
    Payload::LseAtomics,
    Payload::ClockPage,
];

impl Payload {
    /// The payload's name — the cargo bin name, the console banner name, and the
    /// key in every manifest. One spelling, everywhere.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Payload::Ident => "ident",
            Payload::StraightLine => "straight-line",
            Payload::BranchDense => "branch-dense",
            Payload::Svc => "svc",
            Payload::ExceptionAbort => "exception-abort",
            Payload::WfiIdle => "wfi-idle",
            Payload::LlscAtomics => "llsc-atomics",
            Payload::LseAtomics => "lse-atomics",
            Payload::ClockPage => "clock-page",
            Payload::LinuxGuest => "linux-guest",
        }
    }

    /// Parse a payload from its [`Payload::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Payload> {
        ALL_PAYLOADS.iter().copied().find(|p| p.name() == name)
    }

    /// Whether the payload has a counting window at all. [`Payload::Ident`] (the
    /// capability report) and [`Payload::LinuxGuest`] (a full VM) do not.
    #[must_use]
    pub const fn has_window(self) -> bool {
        !matches!(self, Payload::Ident | Payload::LinuxGuest)
    }

    /// Whether the payload's count includes an **in-band runtime term** it reports itself
    /// (`STXR` retries for [`Payload::LlscAtomics`], seqlock retries for
    /// [`Payload::ClockPage`]) — a data-dependent count the oracle cannot derive and the
    /// guest must print. The run loop refuses a record from such a payload that never
    /// supplied the term: a defaulted 0 would let the record claim a reported count it
    /// never made. Mirrors [`Expectation::has_reported_term`].
    #[must_use]
    pub const fn has_reported_term(self) -> bool {
        matches!(self, Payload::LlscAtomics | Payload::ClockPage)
    }

    /// The EXACT ordered sequence of non-branch [`OracleOp`]s this payload's counting window
    /// must contain — order AND multiplicity, not mere presence — for its oracle count to
    /// hold. The window gate compares the decoded window's class sequence against this
    /// verbatim, so a changed `subs …, #1` decrement, an added second `SVC`, or a dropped
    /// side of an LL/SC pair fails even though the branch sequence and golden output are
    /// unchanged. EVERY counted loop carries the `SubsDecrement` its `trips - 1` backedge
    /// count depends on; `ident` has no window. (Calibrated against the built payloads and
    /// pinned by the `arm-scan windows` gate.)
    #[must_use]
    pub const fn required_window_ops(self) -> &'static [OracleOp] {
        match self {
            Payload::Ident => &[],
            Payload::Svc => &[OracleOp::Svc, OracleOp::SubsDecrement],
            Payload::WfiIdle => &[OracleOp::Wfi, OracleOp::SubsDecrement],
            Payload::LlscAtomics => &[
                // Both sides of the exclusive pair (LDXR, STXR), then the loop decrement.
                OracleOp::LlscExclusive,
                OracleOp::LlscExclusive,
                OracleOp::SubsDecrement,
            ],
            // Straight-line, branch-dense, exception-abort, lse-atomics and clock-page are
            // all counted loops whose backedge count assumes a single `subs …, #1`.
            Payload::StraightLine
            | Payload::BranchDense
            | Payload::ExceptionAbort
            | Payload::LseAtomics
            | Payload::ClockPage => &[OracleOp::SubsDecrement],
            // A guest CLASS with no bare-metal window (AA-5's Linux guest).
            Payload::LinuxGuest => &[],
        }
    }
}

/// The run scales. AA-1(a) sweeps counts "differentially across 1e6/1e7/1e8
/// scales"; [`Scale::Smoke`] is the tiny scale the TCG smoke and the
/// smoke-once-before-spend rule (`docs/ARM-ALTRA.md` §Execution constraints) use.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "kebab-case"))]
pub enum Scale {
    /// The TCG / smoke-fire scale. Small enough to boot under emulation in
    /// seconds.
    Smoke,
    /// ~1e6 trips.
    S1e6,
    /// ~1e7 trips.
    S1e7,
    /// ~1e8 trips.
    S1e8,
}

/// Every scale, in a stable order.
pub const ALL_SCALES: [Scale; 4] = [Scale::Smoke, Scale::S1e6, Scale::S1e7, Scale::S1e8];

impl Scale {
    /// The scale's name, as it appears in manifests.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Scale::Smoke => "smoke",
            Scale::S1e6 => "1e6",
            Scale::S1e7 => "1e7",
            Scale::S1e8 => "1e8",
        }
    }

    /// Parse a scale from its [`Scale::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Scale> {
        ALL_SCALES.iter().copied().find(|s| s.name() == name)
    }

    /// The index the harness writes into the params page.
    #[must_use]
    pub const fn index(self) -> u32 {
        match self {
            Scale::Smoke => 0,
            Scale::S1e6 => 1,
            Scale::S1e7 => 2,
            Scale::S1e8 => 3,
        }
    }

    /// Recover a scale from a params-page index. Anything out of range is
    /// [`Scale::Smoke`]: an unmanaged (all-zero) params page must land on the
    /// cheap scale, never on a 1e8 run.
    #[must_use]
    pub const fn from_index(index: u32) -> Scale {
        match index {
            1 => Scale::S1e6,
            2 => Scale::S1e7,
            3 => Scale::S1e8,
            _ => Scale::Smoke,
        }
    }
}

/// Trip count for a (payload, scale). This is the value the payload receives in
/// `x1`; the model and the asm therefore cannot disagree about it.
///
/// [`Payload::WfiIdle`] scales far more slowly than the rest: each trip blocks on
/// a real timer interrupt, so its cost is wall-clock-bound, not branch-bound. A
/// 1e8-trip idle run would take hours and measure nothing extra. Recorded here
/// rather than in prose so the checker sees the same numbers the harness does.
#[must_use]
pub const fn trips(payload: Payload, scale: Scale) -> u64 {
    match payload {
        // No counting window; the trip count is meaningless.
        Payload::Ident => 0,
        Payload::WfiIdle => match scale {
            Scale::Smoke => 200,
            Scale::S1e6 => 10_000,
            Scale::S1e7 => 100_000,
            Scale::S1e8 => 1_000_000,
        },
        _ => match scale {
            Scale::Smoke => 1_000,
            Scale::S1e6 => 1_000_000,
            Scale::S1e7 => 10_000_000,
            Scale::S1e8 => 100_000_000,
        },
    }
}

/// The largest work delta (BR_RETIRED past `MARK_BEGIN`) at which an armed overflow is
/// guaranteed to land INSIDE a windowed payload's counting window, at the given scale.
///
/// A deadline past the window's branch budget never fires: the guest reaches its exit
/// sentinel first and the sample records `deliveries: 0`. A fixed `1..=100_000` draw
/// therefore cannot arm a landable deadline in a small window — WFI's deliberately shortened
/// scales, or ANY payload at smoke, whose window is only ~`trips` branches — so the required
/// AA-1 cells (and the default smoke plan) cannot produce an accepted overflow run.
///
/// Every windowed payload loops with at least one retired branch per trip (the loop
/// backedge), so its window holds **at least [`trips`] branches**. A deadline in the FIRST
/// HALF of that guaranteed minimum lands no later than the window's midpoint — clear of the
/// close by at least `trips / 2` branches, which absorbs positive skid. `None` for
/// [`Payload::Ident`], which has no window.
#[must_use]
pub const fn max_landable_delta(payload: Payload, scale: Scale) -> Option<u64> {
    if !payload.has_window() {
        return None;
    }
    // `trips` is ≥ 200 for every windowed payload/scale, so the half is ≥ 100; the `max(1)`
    // is a floor against a hypothetical single-trip window, never a real one.
    let half = trips(payload, scale) / 2;
    Some(if half == 0 { 1 } else { half })
}

/// Virtual-timer interval, in counter ticks, that [`Payload::WfiIdle`] arms per
/// trip. Interval choice is not load-bearing for counts: the payload masks
/// interrupts before arming, so the interrupt is *taken* at the unmask, whatever
/// the wall-clock delay was.
pub const WFI_TIMER_TICKS: u64 = 2_000;

/// A branch instruction encoding class. The harness's scanner decodes the built
/// payload and asserts the window's branch sequence equals the model's — which is
/// what makes "known by construction" a machine-checked claim rather than a
/// comment.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "kebab-case"))]
pub enum BranchKind {
    /// `B <label>` — unconditional, always taken.
    B,
    /// `BL <label>` — unconditional, always taken.
    Bl,
    /// `B.<cond> <label>`.
    BCond,
    /// `CBZ <Rt>, <label>`.
    Cbz,
    /// `CBNZ <Rt>, <label>`.
    Cbnz,
    /// `TBZ <Rt>, #<imm>, <label>`.
    Tbz,
    /// `TBNZ <Rt>, #<imm>, <label>`.
    Tbnz,
    /// `BR <Rn>` — unconditional indirect.
    Br,
    /// `BLR <Rn>` — unconditional indirect.
    Blr,
    /// `RET {<Rn>}` — unconditional indirect.
    Ret,
    /// `ERET` — exception return. Whether `BR_RETIRED` counts it is
    /// [`Ambiguity::ExceptionReturn`], an unknown.
    Eret,
}

/// A non-branch instruction class whose presence in a payload's counting window is
/// load-bearing for that payload's oracle count — the class the count formula assumes runs.
///
/// The window gate checks the branch sequence exhaustively, but a count can also rest on
/// non-branch opcodes: `svc`'s count is `trips` SVCs, `wfi-idle`'s is `trips` WFIs, and a
/// looped window's backedge count is driven by a `SUBS`-decrement whose immediate is the
/// loop step. Removing the `SVC`, or changing `subs …, #1` to `#2`, leaves the branch
/// classes/predicates/targets — and the smoke output — unchanged while breaking the count.
/// So the model declares the non-branch ops each window must contain
/// ([`Payload::required_window_ops`]) and the gate verifies them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "kebab-case"))]
pub enum OracleOp {
    /// `SVC #0` — the synchronous exception the `svc` payload counts once per trip.
    Svc,
    /// `WFI` — the wait the `wfi-idle` payload counts once per trip.
    Wfi,
    /// An LL/SC exclusive (`LDXR`/`STXR`) — the `llsc-atomics` retry primitive.
    LlscExclusive,
    /// A `SUBS Xd, Xn, #1` — the loop-counter decrement whose backedge the model counts.
    SubsDecrement,
}

/// The classes whose per-occurrence `BR_RETIRED` contribution is **not known
/// a priori** and is measured by stage AA-1.
///
/// None of these is a branch *instruction*, so the architecturally expected weight
/// of each is 0 — but "architecturally expected" is exactly the kind of assumption
/// `docs/ARM-ALTRA.md` refuses to let a stage rest on, and rr's experience is that
/// aarch64 counter semantics are "microarch-gated and only empirically trusted"
/// (`docs/ARM-PORT.md` §evidence). So they are unknowns until silicon says
/// otherwise, and the payload set above is designed to identify each one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "kebab-case"))]
pub enum Ambiguity {
    /// Taking an exception (a change of flow that is not a branch instruction).
    ExceptionEntry,
    /// `ERET`.
    ExceptionReturn,
    /// The `SVC` instruction itself.
    SvcInstruction,
    /// The `WFI` instruction itself.
    WfiInstruction,
}

/// Every ambiguity class, in a stable order.
pub const ALL_AMBIGUITIES: [Ambiguity; 4] = [
    Ambiguity::ExceptionEntry,
    Ambiguity::ExceptionReturn,
    Ambiguity::SvcInstruction,
    Ambiguity::WfiInstruction,
];

impl Ambiguity {
    /// The class's name, as it appears in manifests.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Ambiguity::ExceptionEntry => "exception-entry",
            Ambiguity::ExceptionReturn => "exception-return",
            Ambiguity::SvcInstruction => "svc-instruction",
            Ambiguity::WfiInstruction => "wfi-instruction",
        }
    }
}

/// The measured per-occurrence `BR_RETIRED` contribution of each [`Ambiguity`]
/// class, plus the per-class constant offset of the counting window.
///
/// **There is deliberately no `Default` and no constructor that invents values.**
/// Pre-silicon, this struct cannot be built from thin air; the only way to obtain
/// one is [`Weights::measured`] (from a stage-AA-1 constants pack) or [`solve`]
/// (from retained records). A checker handed no weights must refuse to check, not
/// fall back to a guess — that is the whole point of "the apparatus must treat
/// them as unknowns (parameters), never defaults".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
pub struct Weights {
    /// `BR_RETIRED` contribution of one exception entry.
    pub exception_entry: u64,
    /// `BR_RETIRED` contribution of one `ERET`.
    pub exception_return: u64,
    /// `BR_RETIRED` contribution of one `SVC`.
    pub svc_instruction: u64,
    /// `BR_RETIRED` contribution of one `WFI`.
    pub wfi_instruction: u64,
    /// The constant offset of the counting window itself: the branches, if any,
    /// that the counter attributes to the window but that the body does not
    /// contain (the arm/read edges). x86's analogue measured `n+2`.
    ///
    /// # This field is a falsifiable prediction, and AA-1 is its test
    ///
    /// One offset, shared by every payload class — the model's stance is that the
    /// bracket overhead is a property of the *window* (the two mark stores and the
    /// counter reads around them), not of what runs inside it. That is a claim about
    /// N1, and it is the kind of claim this apparatus is forbidden from quietly
    /// assuming, so it is stated here as a prediction that the measurements can
    /// **refute**:
    ///
    /// - [`solve`] derives the offset independently from the two zero-ambiguity
    ///   classes (straight-line and branch-dense) and returns
    ///   [`SolveError::InconsistentOffset`] if they disagree. A variable offset is a
    ///   mismatch, not a calibration (`docs/ARM-ALTRA.md` §AA-1(a)).
    /// - The `SVC` class then over-determines the system, so a per-class offset the
    ///   four weights cannot absorb surfaces as a nonzero [`Solved::residual`].
    ///
    /// Both of those fail *loudly*. What they do not do is repair the model, and the
    /// AA-1 acceptance criterion speaks of "stable **per-class** count offsets" — so
    /// the escape hatch is named here rather than discovered on arrival day:
    ///
    /// **If N1 delivers stable but class-dependent offsets, this field generalizes to
    /// a per-class intercept map, solved as the intercept of count-vs-trips across
    /// the 1e6/1e7/1e8 scales** (which is exactly why AA-1(a) sweeps scales
    /// differentially rather than measuring one size). It is deliberately *not*
    /// generalized pre-silicon: a free offset per class, fitted from one scale each,
    /// would absorb every ambiguity weight into itself and make the solve
    /// unidentifiable — the over-determination that gives [`Solved::residual`] its
    /// meaning would be gone, and the model would fit anything, including a wrong
    /// answer.
    ///
    /// That is why the per-class *constants pack* is an arrival-day deliverable, not a
    /// pre-silicon one, and this is a deliberate deferral rather than a gap: emitting it
    /// requires the differential-scale-intercept solve above, whose input is the actual
    /// 1e6/1e7/1e8 counts AA-1 measures — data that does not exist until the box is in
    /// hand. Building that solve now, against synthetic data, would be building the
    /// unidentifiable free-per-class fit the paragraph above forbids. So the apparatus
    /// validates the identifiable *uniform* model pre-silicon (which its fixtures and
    /// gates exercise in full) and names the per-class generalization as the escape
    /// hatch — the same shape as the accepted skid-landing deferral: the mechanism is
    /// ready, the constants it takes are measured on silicon, never invented here.
    pub window_offset: u64,
}

impl Weights {
    /// Build a weights pack from values **measured on silicon** (stage AA-1's
    /// constants pack, `docs/ARM-ALTRA.md` §Definition of done #2). The argument
    /// names exist to make a caller that is inventing numbers read as one.
    #[must_use]
    pub const fn measured(
        exception_entry: u64,
        exception_return: u64,
        svc_instruction: u64,
        wfi_instruction: u64,
        window_offset: u64,
    ) -> Weights {
        Weights {
            exception_entry,
            exception_return,
            svc_instruction,
            wfi_instruction,
            window_offset,
        }
    }
}

/// A payload's expected taken-branch count, decomposed.
///
/// **Serialize-only, deliberately.** An `Expectation` can be *written* into a
/// manifest as human- and machine-readable evidence, but nothing may *read* one
/// back and believe it. Consumers recompute it from `(payload, scale, seed)` via
/// [`expected`] — `docs/ARM-ALTRA.md` §Evidence integrity #2 requires floors to be
/// recomputed from the raw records, never read from a summary line the harness
/// asserted about itself. Making the type impossible to deserialize is how that
/// rule is enforced by the compiler rather than by discipline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "std", derive(Serialize))]
pub struct Expectation {
    /// The payload.
    pub payload: Payload,
    /// The scale.
    pub scale: Scale,
    /// The PRNG seed the payload was given (only [`Payload::BranchDense`] uses it).
    pub seed: u64,
    /// Trips the payload was asked to run.
    pub trips: u64,
    /// Taken branches derived exactly, with no unknowns and no runtime input.
    pub certain_taken: u64,
    /// Exception entries executed in the window.
    pub exception_entries: u64,
    /// `ERET`s executed in the window.
    pub exception_returns: u64,
    /// `SVC`s executed in the window.
    pub svc_instructions: u64,
    /// `WFI`s executed in the window.
    pub wfi_instructions: u64,
    /// True when the payload's count includes a term it can only report at
    /// runtime (`STXR` retries; seqlock retries). See [`total`].
    pub has_reported_term: bool,
    /// The branch instructions the window's body contains, in address order.
    /// The harness scanner decodes the built ELF and asserts equality.
    pub inline_branch_seq: &'static [BranchKind],
    /// The expected 4-bit **condition code** of each branch, position-aligned with
    /// [`Expectation::inline_branch_seq`]: `Some(cond)` for a `B.cond` (e.g. `NE = 0x1`
    /// for a loop back-edge), `None` for every other class (they encode their test in
    /// the opcode, which the kind already distinguishes). The verifier checks this, so
    /// a `b.ne` → `b.eq` flip — same [`BranchKind::BCond`], different control flow and
    /// therefore different taken-branch count — is caught, where a class-only check
    /// would pass it.
    pub inline_branch_conds: &'static [Option<u8>],
    /// The expected **exact target** of each branch, position-aligned with
    /// [`Expectation::inline_branch_seq`]: `Some(off)` is the destination as a byte
    /// offset from the window base, `None` a register/indirect branch with no static
    /// target. The verifier resolves each immediate branch's target from the ELF and
    /// checks it equals `window_base + off`, so a backedge or `CBZ`/`TBZ` retargeted to
    /// a different in-window label — same class, same condition, different control flow
    /// and taken-branch count — is caught, where an in-window-only check would pass it.
    pub inline_branch_targets: &'static [Option<i32>],
    /// The expected **predicate operand** of each branch, position-aligned with
    /// [`Expectation::inline_branch_seq`]: `Some(op)` is the register/bit a `CBZ`/`CBNZ`/
    /// `TBZ`/`TBNZ` tests (encoded by [`scan::branch_test_operand`]), `None` for classes
    /// whose predicate is elsewhere (`B.cond`'s condition, the unconditional/indirect
    /// classes). The verifier checks it so a `TBZ` whose tested bit or register is changed
    /// — same class, same target, different taken-branch count — is caught, where a
    /// class+target check would pass it.
    pub inline_branch_operands: &'static [Option<u32>],
}

impl Expectation {
    /// The full expected count, given measured [`Weights`] and the retry count the
    /// payload reported (0 for payloads with no reported term).
    ///
    /// **Checked, not saturating** — `None` on overflow. This is called on UNTRUSTED
    /// evidence (a malformed record with huge weights or a huge reported term), and a
    /// saturated `u64::MAX` prediction would then be MATCHED by a record whose own
    /// `measured_taken` is `u64::MAX` (`work_begin = 0`, `work_end = u64::MAX`), passing
    /// count exactness on unrepresentable arithmetic. So an overflow fails closed: the
    /// checker treats `None` as a count-check failure rather than a valid oracle count.
    /// It still never panics — `checked_*` returns `None`, it does not abort.
    #[must_use]
    pub fn total(&self, w: &Weights, reported_taken: u64) -> Option<u64> {
        self.certain_taken
            .checked_add(reported_taken)?
            .checked_add(w.exception_entry.checked_mul(self.exception_entries)?)?
            .checked_add(w.exception_return.checked_mul(self.exception_returns)?)?
            .checked_add(w.svc_instruction.checked_mul(self.svc_instructions)?)?
            .checked_add(w.wfi_instruction.checked_mul(self.wfi_instructions)?)?
            .checked_add(w.window_offset)
    }
}

/// xorshift64* — the generator the [`Payload::BranchDense`] asm implements. The
/// model must reproduce it bit for bit or the predicted count is wrong.
///
/// State update then scramble: the *state* carries forward, the *product* is the
/// output whose bits the payload branches on.
#[derive(Clone, Copy, Debug)]
pub struct XorShift64Star(u64);

impl XorShift64Star {
    /// Create a generator. The seed must be nonzero; zero is remapped to
    /// [`DEFAULT_SEED`] (a zero state is a fixed point of xorshift and would make
    /// every trip identical — a silently degenerate payload, which is worse than
    /// a loud one).
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        if seed == 0 {
            Self(DEFAULT_SEED)
        } else {
            Self(seed)
        }
    }

    /// Advance the state and return the scrambled output.
    pub const fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(XORSHIFT_MUL)
    }
}

/// Taken branches contributed by one [`Payload::BranchDense`] trip's seven
/// data-dependent branches, given that trip's PRNG output.
///
/// This mirrors the asm one-for-one, in the asm's order:
/// `TBZ #0`, `TBZ #1`, `TBZ #2`, `TBZ #3` are taken when the bit is **clear**;
/// `TBNZ #4`, `TBNZ #5` are taken when the bit is **set**; `CBZ` on the low byte
/// is taken when that byte is zero.
#[must_use]
pub const fn branch_dense_trip_taken(r: u64) -> u64 {
    let mut taken = 0;
    if r & (1 << 0) == 0 {
        taken += 1;
    }
    if r & (1 << 1) == 0 {
        taken += 1;
    }
    if r & (1 << 2) == 0 {
        taken += 1;
    }
    if r & (1 << 3) == 0 {
        taken += 1;
    }
    if r & (1 << 4) != 0 {
        taken += 1;
    }
    if r & (1 << 5) != 0 {
        taken += 1;
    }
    if r & 0xff == 0 {
        taken += 1;
    }
    taken
}

/// Taken branches contributed by one [`Payload::BranchDense`] trip — and its
/// mirror image, the accumulator.
///
/// Each of the seven branches adds a distinct weight on its **not-taken** path, so
/// the accumulator is the exact complement of the taken count. That makes the
/// accumulator a machine-checkable witness for the *predicate* model: if the
/// payload's returned accumulator equals [`branch_dense_accumulator`], then every
/// one of the seven predicates evaluated the way the model says it did, on every
/// trip.
///
/// This is the strongest thing emulation can say about the oracle, and it is worth
/// being exact about what it is and is not. It validates the *predicates* — the
/// PRNG, the bit tests, the loop structure. It says nothing whatever about whether
/// the hardware counter counts those branches, which is AA-1's question and
/// silicon's alone to answer. The TCG smoke checks it for that reason: it is a real
/// gate that does not overclaim.
#[must_use]
pub const fn branch_dense_trip_acc(r: u64) -> u64 {
    let mut acc = 0;
    if r & (1 << 0) != 0 {
        acc += 1;
    }
    if r & (1 << 1) != 0 {
        acc += 2;
    }
    if r & (1 << 2) != 0 {
        acc += 3;
    }
    if r & (1 << 3) != 0 {
        acc += 4;
    }
    if r & (1 << 4) == 0 {
        acc += 5;
    }
    if r & (1 << 5) == 0 {
        acc += 6;
    }
    if r & 0xff != 0 {
        acc += 7;
    }
    acc
}

/// The accumulator [`Payload::BranchDense`] returns, predicted.
#[must_use]
pub fn branch_dense_accumulator(seed: u64, trips: u64) -> u64 {
    let mut rng = XorShift64Star::new(seed);
    let mut acc: u64 = 0;
    for _ in 0..trips {
        acc = acc.wrapping_add(branch_dense_trip_acc(rng.next_u64()));
    }
    acc
}

/// The accumulator [`Payload::StraightLine`] returns, predicted.
///
/// The body is 32 rounds of `a += b; b ^= a` per trip, starting from `(0, 1)` —
/// wrapping, integer, environment-independent. Predicted here so the TCG smoke can
/// prove the body really executed all of its trips rather than being folded away by
/// an optimizer or truncated by a bad trip count. (The body has no data-dependent
/// branches, so unlike branch-dense there is no predicate model to validate — this
/// is a liveness witness, not an oracle check.)
#[must_use]
pub fn straight_line_accumulator(trips: u64) -> u64 {
    let mut a: u64 = 0;
    let mut b: u64 = 1;
    for _ in 0..trips {
        for _ in 0..32 {
            a = a.wrapping_add(b);
            b ^= a;
        }
    }
    a
}

/// The branch sequences each payload's window body emits, in address order.
mod seq {
    use super::BranchKind::{self, *};

    pub const STRAIGHT_LINE: &[BranchKind] = &[BCond];
    pub const BRANCH_DENSE: &[BranchKind] = &[Tbz, Tbz, Tbz, Tbz, Tbnz, Tbnz, Cbz, BCond];
    pub const SVC: &[BranchKind] = &[BCond];
    pub const EXCEPTION_ABORT: &[BranchKind] = &[BCond];
    pub const WFI_IDLE: &[BranchKind] = &[BCond];
    pub const LLSC_ATOMICS: &[BranchKind] = &[Cbnz, BCond];
    pub const LSE_ATOMICS: &[BranchKind] = &[BCond];
    pub const CLOCK_PAGE: &[BranchKind] = &[Tbnz, BCond, BCond];
    pub const NONE: &[BranchKind] = &[];

    /// AArch64 condition code for `NE` (`Z == 0`) — every counted loop's back-edge
    /// (`subs; b.ne loop`). The `*_CONDS` slices are position-aligned with the
    /// `BranchKind` slices above: `Some(cond)` at each `BCond`, `None` elsewhere.
    pub const NE: u8 = 0x1;
    pub const STRAIGHT_LINE_CONDS: &[Option<u8>] = &[Some(NE)];
    pub const BRANCH_DENSE_CONDS: &[Option<u8>] =
        &[None, None, None, None, None, None, None, Some(NE)];
    pub const SVC_CONDS: &[Option<u8>] = &[Some(NE)];
    pub const EXCEPTION_ABORT_CONDS: &[Option<u8>] = &[Some(NE)];
    pub const WFI_IDLE_CONDS: &[Option<u8>] = &[Some(NE)];
    pub const LLSC_ATOMICS_CONDS: &[Option<u8>] = &[None, Some(NE)];
    pub const LSE_ATOMICS_CONDS: &[Option<u8>] = &[Some(NE)];
    pub const CLOCK_PAGE_CONDS: &[Option<u8>] = &[None, Some(NE), Some(NE)];
    pub const NONE_CONDS: &[Option<u8>] = &[];

    /// The **exact** target of each immediate window branch, as a byte offset from the
    /// window base (`target - window_base`), position-aligned with the `BranchKind`
    /// slices above. `Some(off)` pins the destination exactly; `None` is a register/
    /// indirect branch with no static target. In-window-only verification accepts a
    /// backedge or `CBZ`/`TBZ` retargeted to a *different* in-window label — which
    /// changes control flow and the taken-branch count while keeping class and
    /// condition. These pin the destination, so such a retarget fails the gate. The
    /// values are the by-construction control flow: forward exits skip ahead, the
    /// single `B.NE` backedge returns to the loop top.
    pub const STRAIGHT_LINE_TARGETS: &[Option<i32>] = &[Some(8)];
    pub const BRANCH_DENSE_TARGETS: &[Option<i32>] = &[
        Some(48),
        Some(56),
        Some(64),
        Some(72),
        Some(80),
        Some(88),
        Some(100),
        Some(24),
    ];
    pub const SVC_TARGETS: &[Option<i32>] = &[Some(0)];
    pub const EXCEPTION_ABORT_TARGETS: &[Option<i32>] = &[Some(4)];
    pub const WFI_IDLE_TARGETS: &[Option<i32>] = &[Some(0)];
    pub const LLSC_ATOMICS_TARGETS: &[Option<i32>] = &[Some(4), Some(4)];
    pub const LSE_ATOMICS_TARGETS: &[Option<i32>] = &[Some(4)];
    pub const CLOCK_PAGE_TARGETS: &[Option<i32>] = &[Some(20), Some(20), Some(20)];
    pub const NONE_TARGETS: &[Option<i32>] = &[];

    /// The register/bit predicate operand of each branch (`scan::branch_test_operand`
    /// encoding), position-aligned with the `BranchKind` slices: `Some(op)` at each
    /// `CBZ`/`CBNZ`/`TBZ`/`TBNZ`, `None` elsewhere. Extracted by construction from the
    /// built ELF, then frozen — a later edit that retargets a bit-test to a different
    /// register or bit changes the taken count and diverges from these.
    pub const STRAIGHT_LINE_OPERANDS: &[Option<u32>] = &[None];
    pub const BRANCH_DENSE_OPERANDS: &[Option<u32>] = &[
        Some(0x004),
        Some(0x024),
        Some(0x044),
        Some(0x064),
        Some(0x084),
        Some(0x0a4),
        Some(0x02d),
        None,
    ];
    pub const SVC_OPERANDS: &[Option<u32>] = &[None];
    pub const EXCEPTION_ABORT_OPERANDS: &[Option<u32>] = &[None];
    pub const WFI_IDLE_OPERANDS: &[Option<u32>] = &[None];
    pub const LLSC_ATOMICS_OPERANDS: &[Option<u32>] = &[Some(0x005), None];
    pub const LSE_ATOMICS_OPERANDS: &[Option<u32>] = &[None];
    pub const CLOCK_PAGE_OPERANDS: &[Option<u32>] = &[Some(0x008), None, None];
    pub const NONE_OPERANDS: &[Option<u32>] = &[];
}

/// The expected count for a (payload, scale, seed).
///
/// # The derivations
///
/// Every body is a single loop of `trips` iterations whose back-edge is a
/// `B.NE`, taken on all but the last trip: **`trips - 1`** taken branches. What
/// each payload adds on top:
///
/// - **straight-line** — nothing. 64 ALU instructions per trip, no branch.
///   `certain = trips - 1`.
/// - **branch-dense** — seven data-dependent branches per trip, summed over the
///   trip's PRNG output by [`branch_dense_trip_taken`].
///   `certain = (trips - 1) + Σ branch_dense_trip_taken(r_i)`.
/// - **svc** — `SVC #0` per trip. The handler is a bare `ERET` placed inline in
///   the payload's own vector slot, so the exception path contributes **no branch
///   instruction at all**: only one exception entry, one `ERET`, one `SVC`, each
///   of unknown weight. `certain = trips - 1`.
/// - **exception-abort** — a load from an unmapped VA per trip. The handler
///   (three ALU instructions to skip the faulting load, then `ERET`) is likewise
///   branch-free. `certain = trips - 1`, plus one entry and one `ERET` per trip —
///   the same pair as **svc** but with no `SVC` term, which is what isolates
///   `w_svc`.
/// - **wfi-idle** — one `WFI` and one timer interrupt per trip; the IRQ handler
///   (acknowledge, disable the timer, EOI, `ERET`) is branch-free.
///   `certain = trips - 1`.
/// - **llsc-atomics** — the `CBNZ` retry is taken once per `STXR` failure, and
///   failures are the hazard under study, so their count is *not* known by
///   construction: the payload counts them branch-free (`ADD` of the `STXR`
///   status register) and reports the total. `certain = trips - 1`, plus the
///   reported retries.
/// - **lse-atomics** — the same increment with `LDADD`/`CASAL`: no retry, no
///   ambiguity. `certain = trips - 1`. The a/b pair is AA-4's whole argument.
/// - **clock-page** — a seqlock read per trip. Under the spike protocol the page
///   is quiescent *by construction* inside the window (the harness can only
///   refresh it at a guest exit, and the window contains none), so neither retry
///   branch is ever taken. The payload counts any retry anyway, branch-free
///   (`CINC`), and reports it: a nonzero count falsifies the quiescence argument
///   rather than silently perturbing the oracle. `certain = trips - 1`.
#[must_use]
pub fn expected(payload: Payload, scale: Scale, seed: u64) -> Expectation {
    let trips = trips(payload, scale);
    // Every body's loop back-edge is taken on all but the final trip. A zero-trip
    // payload (Ident) has no window and no back-edge.
    let back_edges = trips.saturating_sub(1);

    let mut e = Expectation {
        payload,
        scale,
        seed,
        trips,
        certain_taken: back_edges,
        exception_entries: 0,
        exception_returns: 0,
        svc_instructions: 0,
        wfi_instructions: 0,
        has_reported_term: false,
        inline_branch_seq: seq::NONE,
        inline_branch_conds: seq::NONE_CONDS,
        inline_branch_targets: seq::NONE_TARGETS,
        inline_branch_operands: seq::NONE_OPERANDS,
    };

    match payload {
        // No counting window: the capability report, and the AA-5 Linux guest class (a
        // full VM whose determinism the AA-6 matrix requires, not a bare-metal payload
        // with a by-construction count).
        Payload::Ident | Payload::LinuxGuest => {
            e.certain_taken = 0;
        }
        Payload::StraightLine => {
            e.inline_branch_seq = seq::STRAIGHT_LINE;
            e.inline_branch_conds = seq::STRAIGHT_LINE_CONDS;
            e.inline_branch_targets = seq::STRAIGHT_LINE_TARGETS;
            e.inline_branch_operands = seq::STRAIGHT_LINE_OPERANDS;
        }
        Payload::BranchDense => {
            let mut rng = XorShift64Star::new(seed);
            let mut data_taken: u64 = 0;
            for _ in 0..trips {
                data_taken = data_taken.saturating_add(branch_dense_trip_taken(rng.next_u64()));
            }
            e.certain_taken = back_edges.saturating_add(data_taken);
            e.inline_branch_seq = seq::BRANCH_DENSE;
            e.inline_branch_conds = seq::BRANCH_DENSE_CONDS;
            e.inline_branch_targets = seq::BRANCH_DENSE_TARGETS;
            e.inline_branch_operands = seq::BRANCH_DENSE_OPERANDS;
        }
        Payload::Svc => {
            e.exception_entries = trips;
            e.exception_returns = trips;
            e.svc_instructions = trips;
            e.inline_branch_seq = seq::SVC;
            e.inline_branch_conds = seq::SVC_CONDS;
            e.inline_branch_targets = seq::SVC_TARGETS;
            e.inline_branch_operands = seq::SVC_OPERANDS;
        }
        Payload::ExceptionAbort => {
            e.exception_entries = trips;
            e.exception_returns = trips;
            e.inline_branch_seq = seq::EXCEPTION_ABORT;
            e.inline_branch_conds = seq::EXCEPTION_ABORT_CONDS;
            e.inline_branch_targets = seq::EXCEPTION_ABORT_TARGETS;
            e.inline_branch_operands = seq::EXCEPTION_ABORT_OPERANDS;
        }
        Payload::WfiIdle => {
            e.exception_entries = trips;
            e.exception_returns = trips;
            e.wfi_instructions = trips;
            e.inline_branch_seq = seq::WFI_IDLE;
            e.inline_branch_conds = seq::WFI_IDLE_CONDS;
            e.inline_branch_targets = seq::WFI_IDLE_TARGETS;
            e.inline_branch_operands = seq::WFI_IDLE_OPERANDS;
        }
        Payload::LlscAtomics => {
            e.has_reported_term = true;
            e.inline_branch_seq = seq::LLSC_ATOMICS;
            e.inline_branch_conds = seq::LLSC_ATOMICS_CONDS;
            e.inline_branch_targets = seq::LLSC_ATOMICS_TARGETS;
            e.inline_branch_operands = seq::LLSC_ATOMICS_OPERANDS;
        }
        Payload::LseAtomics => {
            e.inline_branch_seq = seq::LSE_ATOMICS;
            e.inline_branch_conds = seq::LSE_ATOMICS_CONDS;
            e.inline_branch_targets = seq::LSE_ATOMICS_TARGETS;
            e.inline_branch_operands = seq::LSE_ATOMICS_OPERANDS;
        }
        Payload::ClockPage => {
            e.has_reported_term = true;
            e.inline_branch_seq = seq::CLOCK_PAGE;
            e.inline_branch_conds = seq::CLOCK_PAGE_CONDS;
            e.inline_branch_targets = seq::CLOCK_PAGE_TARGETS;
            e.inline_branch_operands = seq::CLOCK_PAGE_OPERANDS;
        }
    }

    e
}

/// One (payload, measured-count) pair fed to [`solve`].
#[derive(Clone, Copy, Debug)]
pub struct Observation {
    /// Which payload and scale produced the count.
    pub expectation: Expectation,
    /// The `BR_RETIRED` delta the harness read across the window.
    pub measured: u64,
    /// The retry count the payload reported (0 when it has no reported term).
    pub reported_taken: u64,
}

/// Why a weight solve could not be performed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SolveError {
    /// A payload class the solve needs was absent from the observations.
    MissingClass(Payload),
    /// The two zero-ambiguity classes (straight-line, branch-dense) disagreed
    /// about the window offset. Per AA-1(a) a *variable* offset is a mismatch,
    /// not a calibration: this is a finding, not something to average away.
    InconsistentOffset {
        /// Offset implied by [`Payload::StraightLine`].
        straight_line: i128,
        /// Offset implied by [`Payload::BranchDense`].
        branch_dense: i128,
    },
    /// A derived weight was negative — the model does not describe this silicon.
    NegativeWeight {
        /// The class whose weight came out negative.
        class: Ambiguity,
        /// The value derived.
        value: i128,
    },
    /// A derived weight was **not an integer**: the measured count differs from the
    /// model by an amount its trip count does not divide.
    ///
    /// This is the sharp end of the model. `BR_RETIRED` is a count of *events*, so a
    /// per-occurrence weight is by definition a whole number; a remainder means the
    /// measurement is not explained by "n occurrences of a fixed-cost class", and
    /// the difference is exactly the unexplained count mismatch that
    /// `docs/ARM-ALTRA.md` treats as **blocking**. Truncating it — which integer
    /// division does silently — would bury the finding inside a plausible weight.
    NonIntegralWeight {
        /// The class whose weight did not come out whole.
        class: Ambiguity,
        /// The numerator: measured minus everything the model already explains.
        numerator: i128,
        /// The trip count that failed to divide it.
        trips: i128,
        /// What was left over.
        remainder: i128,
    },
    /// Trip counts differed where the solve needs them equal.
    UnequalTrips,
}

/// The result of a weight solve, including the residual that makes the
/// over-determination worth having.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Solved {
    /// The derived weights.
    pub weights: Weights,
    /// The **worst residual over every supplied observation**: `measured - predicted`
    /// for whichever row the solved weights explain least well (0 when they explain
    /// all of them).
    ///
    /// This used to be recomputed from the `SVC` row alone — the very row `w_svc` was
    /// derived from — so it was zero by construction and could report a clean solve
    /// whose weights did not reproduce the other measurements. It is now checked
    /// against *all* of them, including any extra classes or scales the caller
    /// supplies beyond the five the solve needs, which is where the model's
    /// over-determination actually lives. Nonzero means the model is wrong about this
    /// silicon: evidence, not noise.
    pub residual: i128,
    /// The payload whose row produced [`Solved::residual`].
    pub worst: Payload,
}

/// Solve for the [`Weights`] from measurements of the payload set.
///
/// Requires observations of straight-line, branch-dense, exception-abort, svc and
/// wfi-idle **at equal trip counts** for the last three (the differencing argument
/// in the crate docs). Returns the derived weights and the residual.
///
/// Signed arithmetic throughout: a negative intermediate is a real result (the
/// model is wrong), and it must surface as [`SolveError`], not wrap into a
/// plausible u64.
pub fn solve(observations: &[Observation]) -> Result<Solved, SolveError> {
    let find = |p: Payload| -> Result<&Observation, SolveError> {
        observations
            .iter()
            .find(|o| o.expectation.payload == p)
            .ok_or(SolveError::MissingClass(p))
    };

    let sl = find(Payload::StraightLine)?;
    let bd = find(Payload::BranchDense)?;
    let ex = find(Payload::ExceptionAbort)?;
    let sv = find(Payload::Svc)?;
    let wf = find(Payload::WfiIdle)?;

    // The two zero-ambiguity classes each yield the window offset directly.
    let off_sl = i128::from(sl.measured) - i128::from(sl.expectation.certain_taken);
    let off_bd = i128::from(bd.measured) - i128::from(bd.expectation.certain_taken);
    if off_sl != off_bd {
        return Err(SolveError::InconsistentOffset {
            straight_line: off_sl,
            branch_dense: off_bd,
        });
    }
    let offset = off_sl;

    // exception-abort: measured = certain + n*(w_entry + w_eret) + offset.
    let n_ex = i128::from(ex.expectation.trips);
    if n_ex == 0 {
        return Err(SolveError::UnequalTrips);
    }
    let pair = i128::from(ex.measured) - i128::from(ex.expectation.certain_taken) - offset;
    let pair_per = exact_div(pair, n_ex, Ambiguity::ExceptionEntry)?;
    if pair_per < 0 {
        return Err(SolveError::NegativeWeight {
            class: Ambiguity::ExceptionEntry,
            value: pair_per,
        });
    }

    // svc - exception (equal trips) isolates w_svc; likewise wfi.
    if sv.expectation.trips != ex.expectation.trips {
        return Err(SolveError::UnequalTrips);
    }
    let w_svc = exact_div(
        i128::from(sv.measured) - i128::from(sv.expectation.certain_taken) - offset,
        n_ex,
        Ambiguity::SvcInstruction,
    )? - pair_per;
    if w_svc < 0 {
        return Err(SolveError::NegativeWeight {
            class: Ambiguity::SvcInstruction,
            value: w_svc,
        });
    }

    let n_wf = i128::from(wf.expectation.trips);
    if n_wf == 0 {
        return Err(SolveError::UnequalTrips);
    }
    let w_wfi = exact_div(
        i128::from(wf.measured) - i128::from(wf.expectation.certain_taken) - offset,
        n_wf,
        Ambiguity::WfiInstruction,
    )? - pair_per;
    if w_wfi < 0 {
        return Err(SolveError::NegativeWeight {
            class: Ambiguity::WfiInstruction,
            value: w_wfi,
        });
    }

    // The entry/return split is not separately identifiable from this set — only
    // their sum is. Attribute the pair to the entry and leave ERET at zero; the
    // sum is what every prediction uses, and claiming a split we cannot see would
    // be exactly the invented constant this apparatus refuses to produce. AA-2
    // (single-step) can separate them by stepping an exception boundary.
    let weights = Weights::measured(
        u64::try_from(pair_per).unwrap_or(u64::MAX),
        0,
        u64::try_from(w_svc).unwrap_or(u64::MAX),
        u64::try_from(w_wfi).unwrap_or(u64::MAX),
        u64::try_from(offset.max(0)).unwrap_or(u64::MAX),
    );

    // Check the solved weights against EVERY observation the caller supplied — not
    // just the `SVC` row they were partly derived from, which reproduces itself by
    // construction and so could report a clean solve over weights that explain
    // nothing else. Extra classes and extra scales beyond the five the solve needs
    // are where the over-determination actually lives, and this is what reads it.
    let mut residual: i128 = 0;
    let mut worst = sv.expectation.payload;
    for o in observations {
        // An overflowing prediction (checked `total` returns `None`) means these candidate
        // weights cannot fit this observation at all — treat it as an unbounded residual so
        // it is never mistaken for a clean solve.
        let predicted = o
            .expectation
            .total(&weights, o.reported_taken)
            .map_or(i128::MAX, i128::from);
        let r = i128::from(o.measured) - predicted;
        if r.unsigned_abs() > residual.unsigned_abs() {
            residual = r;
            worst = o.expectation.payload;
        }
    }

    Ok(Solved {
        weights,
        residual,
        worst,
    })
}

/// Divide exactly, or refuse.
///
/// A per-occurrence `BR_RETIRED` weight is a count of events, so it is a whole
/// number by definition. A remainder means the measurement is not explained by "n
/// occurrences of a fixed-cost class" — the unexplained count mismatch the program
/// treats as blocking. Integer division would truncate it into a plausible weight
/// and lose the finding, which is precisely what this function exists to prevent.
fn exact_div(numerator: i128, trips: i128, class: Ambiguity) -> Result<i128, SolveError> {
    if trips == 0 {
        return Err(SolveError::UnequalTrips);
    }
    let remainder = numerator % trips;
    if remainder != 0 {
        return Err(SolveError::NonIntegralWeight {
            class,
            numerator,
            trips,
            remainder,
        });
    }
    Ok(numerator / trips)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_names_round_trip() {
        for p in ALL_PAYLOADS {
            assert_eq!(Payload::from_name(p.name()), Some(p));
        }
        assert_eq!(Payload::from_name("nope"), None);
    }

    #[test]
    fn pvclock_page_layout_packs_at_the_declared_offsets() {
        // The one payload-side byte layout Miri can interpret: pure `[u8]` packing, no
        // MMIO. A wrong offset or endianness here would corrupt the guest's clock-page
        // read the same in both the harness (managed) and runtime (self-seeded) writers,
        // since both build from this. Reading each field back pins the layout.
        use pvclock::*;
        let p = materialize(
            0x1122_3344_5566_7788,
            0x00de_00ad_00be_00ef,
            1_000_000_000,
            false,
        );
        let u32_at = |o: usize| u32::from_le_bytes(p[o..o + 4].try_into().unwrap());
        let u64_at = |o: usize| u64::from_le_bytes(p[o..o + 8].try_into().unwrap());
        assert_eq!(u32_at(OFF_ABI), PVCLOCK_ABI, "ABI marker");
        assert_eq!(u32_at(OFF_SEQ), 2, "seq is stable/even");
        assert_eq!(u64_at(OFF_VNS), 0x1122_3344_5566_7788, "V-time");
        assert_eq!(u64_at(OFF_GUEST_CLOCK), 0x00de_00ad_00be_00ef, "counter");
        assert_eq!(u64_at(OFF_HZ), 1_000_000_000, "frequency");
        // A static placeholder is materialized but NOT work-derived.
        assert_eq!(
            u32_at(OFF_FLAGS),
            FLAG_MATERIALIZED,
            "materialized, not work-derived"
        );

        // A work-derived page rides FLAG_WORK_DERIVED alongside it — the AA-5-certifying
        // page the harness does not yet publish.
        let w = materialize(0, 0, 1_000_000_000, true);
        let wflags = u32::from_le_bytes(w[OFF_FLAGS..OFF_FLAGS + 4].try_into().unwrap());
        assert_eq!(
            wflags,
            FLAG_MATERIALIZED | FLAG_WORK_DERIVED,
            "work-derived flag"
        );
    }

    #[test]
    fn branch_metadata_is_position_aligned_with_the_sequence() {
        // The verifier indexes conds/targets by branch position, so a slice that fell
        // out of alignment with `inline_branch_seq` would check the wrong branch (or
        // silently skip one). Every payload's three slices must be the same length.
        for p in ALL_PAYLOADS {
            let e = expected(p, Scale::Smoke, DEFAULT_SEED);
            assert_eq!(
                e.inline_branch_seq.len(),
                e.inline_branch_conds.len(),
                "{}: conds not aligned with seq",
                p.name()
            );
            assert_eq!(
                e.inline_branch_seq.len(),
                e.inline_branch_targets.len(),
                "{}: targets not aligned with seq",
                p.name()
            );
            assert_eq!(
                e.inline_branch_seq.len(),
                e.inline_branch_operands.len(),
                "{}: operands not aligned with seq",
                p.name()
            );
        }
    }

    #[test]
    fn scale_names_and_indices_round_trip() {
        for s in ALL_SCALES {
            assert_eq!(Scale::from_name(s.name()), Some(s));
            assert_eq!(Scale::from_index(s.index()), s);
        }
    }

    #[test]
    fn unmanaged_params_page_lands_on_the_smoke_scale() {
        // An all-zero params page (the TCG case, and the case where a harness bug
        // forgets to write it) must never select a 1e8 run.
        assert_eq!(Scale::from_index(0), Scale::Smoke);
        assert_eq!(Scale::from_index(999), Scale::Smoke);
    }

    #[test]
    fn straight_line_is_one_back_edge_per_trip() {
        let e = expected(Payload::StraightLine, Scale::Smoke, DEFAULT_SEED);
        assert_eq!(e.trips, 1_000);
        assert_eq!(e.certain_taken, 999);
        assert_eq!(e.exception_entries, 0);
        assert_eq!(e.inline_branch_seq, seq::STRAIGHT_LINE);
    }

    #[test]
    fn branch_dense_matches_a_hand_rolled_replay() {
        let e = expected(Payload::BranchDense, Scale::Smoke, DEFAULT_SEED);
        // Recompute independently of `expected`, straight from the generator.
        let mut rng = XorShift64Star::new(DEFAULT_SEED);
        let mut taken = 0u64;
        for _ in 0..1_000u64 {
            let r = rng.next_u64();
            for bit in 0..4 {
                if r & (1 << bit) == 0 {
                    taken += 1;
                }
            }
            for bit in 4..6 {
                if r & (1 << bit) != 0 {
                    taken += 1;
                }
            }
            if r & 0xff == 0 {
                taken += 1;
            }
        }
        assert_eq!(e.certain_taken, 999 + taken);
        // Sanity: with ~7 branches per trip and roughly half taken, the count must
        // land in a plausible band — a model that silently degenerated to zero or
        // to "all taken" fails here.
        assert!(taken > 1_000 && taken < 6_000, "taken = {taken}");
    }

    #[test]
    fn branch_dense_taken_and_accumulator_partition_the_seven_branches() {
        // Every branch contributes to exactly one of the two: taken (the count) or
        // the accumulator (the not-taken path's `add`). If that ever stopped being
        // true, the accumulator would no longer witness the predicate model, and
        // the TCG smoke's oracle check would be checking nothing.
        for r in [0u64, u64::MAX, 0xff, 0x100, DEFAULT_SEED, XORSHIFT_MUL] {
            let taken = branch_dense_trip_taken(r);
            let acc_terms = [
                (r & 1 != 0) as u64,
                (r & 2 != 0) as u64,
                (r & 4 != 0) as u64,
                (r & 8 != 0) as u64,
                (r & 16 == 0) as u64,
                (r & 32 == 0) as u64,
                (r & 0xff != 0) as u64,
            ];
            let contributing: u64 = acc_terms.iter().sum();
            assert_eq!(taken + contributing, 7, "r = {r:#x}");
        }
    }

    #[test]
    fn accumulators_are_deterministic_and_live() {
        // Not zero (the body ran), and a pure function of its inputs.
        let a = branch_dense_accumulator(DEFAULT_SEED, 1_000);
        assert_eq!(a, branch_dense_accumulator(DEFAULT_SEED, 1_000));
        assert_ne!(a, 0);
        assert_ne!(a, branch_dense_accumulator(DEFAULT_SEED, 999));

        let s = straight_line_accumulator(1_000);
        assert_eq!(s, straight_line_accumulator(1_000));
        assert_ne!(s, 0);
        assert_ne!(s, straight_line_accumulator(999));
    }

    #[test]
    fn branch_dense_is_seed_sensitive() {
        let a = expected(Payload::BranchDense, Scale::Smoke, DEFAULT_SEED);
        let b = expected(Payload::BranchDense, Scale::Smoke, 0x1234_5678_9abc_def0);
        assert_ne!(a.certain_taken, b.certain_taken);
    }

    #[test]
    fn zero_seed_is_remapped_not_degenerate() {
        // A zero state is a fixed point of xorshift: every trip would draw 0 and
        // the payload would be silently degenerate. Assert it is remapped.
        let mut rng = XorShift64Star::new(0);
        let first = rng.next_u64();
        let second = rng.next_u64();
        assert_ne!(first, 0);
        assert_ne!(first, second);
    }

    #[test]
    fn svc_and_exception_differ_only_by_the_svc_term() {
        let s = expected(Payload::Svc, Scale::Smoke, DEFAULT_SEED);
        let x = expected(Payload::ExceptionAbort, Scale::Smoke, DEFAULT_SEED);
        assert_eq!(s.certain_taken, x.certain_taken);
        assert_eq!(s.exception_entries, x.exception_entries);
        assert_eq!(s.exception_returns, x.exception_returns);
        assert_eq!(s.svc_instructions, 1_000);
        assert_eq!(x.svc_instructions, 0);
    }

    #[test]
    fn ident_has_no_window() {
        let e = expected(Payload::Ident, Scale::Smoke, DEFAULT_SEED);
        assert!(!Payload::Ident.has_window());
        assert_eq!(e.certain_taken, 0);
        assert_eq!(e.trips, 0);
        assert!(e.inline_branch_seq.is_empty());
    }

    #[test]
    fn a_landable_delta_never_exceeds_the_guaranteed_window() {
        // The cap is half the trip count (a sound lower bound on window branches), so a
        // deadline drawn under it lands no later than the window midpoint. The windowless
        // ident payload has no cap.
        assert_eq!(max_landable_delta(Payload::Ident, Scale::Smoke), None);
        for &scale in &[Scale::Smoke, Scale::S1e6, Scale::S1e7, Scale::S1e8] {
            for &payload in ALL_PAYLOADS.iter().filter(|p| p.has_window()) {
                let cap = max_landable_delta(payload, scale).expect("windowed payload has a cap");
                assert!(
                    cap >= 1,
                    "{payload:?}/{scale:?} cap must admit at least delta 1"
                );
                assert!(
                    cap <= trips(payload, scale),
                    "{payload:?}/{scale:?} cap {cap} exceeds its trip count — not a sound window bound"
                );
            }
        }
        // WFI at smoke is the tightest: 200 trips → cap 100, far below the 100_000 default.
        assert_eq!(
            max_landable_delta(Payload::WfiIdle, Scale::Smoke),
            Some(100)
        );
    }

    #[test]
    fn total_fails_closed_on_overflow_rather_than_saturating() {
        // The floor checker calls this on untrusted records; a crafted weight must neither
        // panic nor SATURATE to u64::MAX (which a record whose measured_taken is u64::MAX
        // would then match). It returns None — the checker reads that as malformed evidence.
        let e = expected(Payload::Svc, Scale::S1e8, DEFAULT_SEED);
        let w = Weights::measured(u64::MAX, u64::MAX, u64::MAX, u64::MAX, u64::MAX);
        assert_eq!(e.total(&w, u64::MAX), None);
        // A representable case still computes.
        let small = Weights::measured(1, 1, 1, 1, 0);
        assert!(e.total(&small, 0).is_some());
    }

    /// Build a synthetic, self-consistent measurement set for a chosen ground
    /// truth, then check the solver recovers it. This is the test that keeps the
    /// identifiability argument in the crate docs honest.
    fn synthesize(w: &Weights, scale: Scale) -> [Observation; 5] {
        // No `Vec`: the crate is no_std by default, and the tests run in that
        // shape on purpose — it is the shape the payloads compile in.
        [
            Payload::StraightLine,
            Payload::BranchDense,
            Payload::ExceptionAbort,
            Payload::Svc,
            Payload::WfiIdle,
        ]
        .map(|p| {
            let expectation = expected(p, scale, DEFAULT_SEED);
            Observation {
                expectation,
                measured: expectation
                    .total(w, 0)
                    .expect("synthetic weights do not overflow"),
                reported_taken: 0,
            }
        })
    }

    #[test]
    fn solve_recovers_the_ground_truth_weights() {
        // The architecturally expected answer (nothing but branch instructions
        // counts) — but reached by solving, never assumed.
        let truth = Weights::measured(0, 0, 0, 0, 2);
        let solved = solve(&synthesize(&truth, Scale::S1e6)).expect("solvable");
        assert_eq!(solved.weights, truth);
        assert_eq!(solved.residual, 0);
    }

    #[test]
    fn a_wfi_measurement_its_trips_do_not_divide_is_refused_not_truncated() {
        // The failure this closes: `w_wfi = (measured - certain - offset) / trips`
        // TRUNCATED a remainder, and the residual was recomputed from the SVC row —
        // the very row `w_svc` came from — so it read zero. A WFI measurement the
        // model could not explain therefore produced a clean solve with plausible
        // weights. `BR_RETIRED` counts events: a per-occurrence weight is a whole
        // number, so a remainder IS the unexplained mismatch the program calls
        // blocking, and it must surface as one.
        let truth = Weights::measured(0, 0, 0, 0, 2);
        let mut obs = synthesize(&truth, Scale::S1e6);
        for o in &mut obs {
            if o.expectation.payload == Payload::WfiIdle {
                // One extra taken branch across the whole run: not divisible by the
                // trip count, so no integral per-WFI weight explains it.
                o.measured += 1;
            }
        }
        match solve(&obs) {
            Err(SolveError::NonIntegralWeight {
                class: Ambiguity::WfiInstruction,
                remainder,
                ..
            }) => assert_eq!(remainder, 1),
            other => panic!("a non-integral WFI weight must be refused, got {other:?}"),
        }
    }

    #[test]
    fn a_non_integral_svc_weight_is_refused_too() {
        let truth = Weights::measured(0, 0, 0, 0, 2);
        let mut obs = synthesize(&truth, Scale::S1e6);
        for o in &mut obs {
            if o.expectation.payload == Payload::Svc {
                o.measured += 3;
            }
        }
        assert!(matches!(
            solve(&obs),
            Err(SolveError::NonIntegralWeight {
                class: Ambiguity::SvcInstruction,
                ..
            })
        ));
    }

    #[test]
    fn the_residual_reads_every_observation_not_just_the_row_it_came_from() {
        // An extra class the solve does not consume (lse-atomics) that the solved
        // weights do NOT reproduce. The old residual — recomputed from SVC alone —
        // was zero by construction and could not see this; the whole point of an
        // over-determined system is that it can.
        let truth = Weights::measured(0, 0, 0, 0, 2);
        let five = synthesize(&truth, Scale::S1e6);
        let lse = expected(Payload::LseAtomics, Scale::S1e6, DEFAULT_SEED);
        let mut obs = [
            five[0],
            five[1],
            five[2],
            five[3],
            five[4],
            Observation {
                expectation: lse,
                // 7 taken branches the model cannot account for.
                measured: lse.total(&truth, 0).expect("representable") + 7,
                reported_taken: 0,
            },
        ];
        let solved = solve(&obs).expect("the five solve rows are still consistent");
        assert_eq!(solved.weights, truth);
        assert_eq!(solved.residual, 7, "the unexplained row must be visible");
        assert_eq!(solved.worst, Payload::LseAtomics);

        // And with that row explained, the residual is zero again.
        obs[5].measured = lse.total(&truth, 0).expect("representable");
        assert_eq!(solve(&obs).expect("solvable").residual, 0);
    }

    #[test]
    fn solve_recovers_a_counterfactual_silicon() {
        // A silicon where exception entry *does* count and SVC counts as a branch.
        // The apparatus must be able to express and recover this, which is the
        // reason the weights are unknowns rather than zeros.
        let truth = Weights::measured(1, 0, 1, 0, 5);
        let solved = solve(&synthesize(&truth, Scale::S1e6)).expect("solvable");
        assert_eq!(solved.weights, truth);
        assert_eq!(solved.residual, 0);
    }

    #[test]
    fn solve_rejects_an_inconsistent_window_offset() {
        // AA-1(a): a *variable* per-class offset is a mismatch, not a calibration.
        let truth = Weights::measured(0, 0, 0, 0, 2);
        let mut obs = synthesize(&truth, Scale::S1e6);
        obs[1].measured += 1; // branch-dense now implies a different offset
        assert!(matches!(
            solve(&obs),
            Err(SolveError::InconsistentOffset { .. })
        ));
    }

    #[test]
    fn solve_reports_a_missing_class_rather_than_guessing() {
        let truth = Weights::measured(0, 0, 0, 0, 0);
        let obs = synthesize(&truth, Scale::S1e6);
        let without_svc = [obs[0], obs[1], obs[2], obs[4]];
        assert_eq!(
            solve(&without_svc),
            Err(SolveError::MissingClass(Payload::Svc))
        );
    }

    #[test]
    fn solve_rejects_a_negative_weight() {
        let truth = Weights::measured(0, 0, 0, 0, 2);
        let mut obs = synthesize(&truth, Scale::S1e6);
        // Make the exception class read *below* its certain count by exactly one
        // per trip: the implied weight is a whole number, and it is -1. No
        // non-negative weight explains it, and that is a different finding from a
        // measurement no *integral* weight explains (which is NonIntegralWeight —
        // the two must not be confused, because they say different things about the
        // silicon).
        let ex = obs
            .iter_mut()
            .find(|o| o.expectation.payload == Payload::ExceptionAbort)
            .expect("present");
        ex.measured -= ex.expectation.trips;
        assert!(matches!(
            solve(&obs),
            Err(SolveError::NegativeWeight { .. })
        ));
    }
}
