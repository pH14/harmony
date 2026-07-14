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
        }
    }

    /// Parse a payload from its [`Payload::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Payload> {
        ALL_PAYLOADS.iter().copied().find(|p| p.name() == name)
    }

    /// Whether the payload has a counting window at all. [`Payload::Ident`] does not.
    #[must_use]
    pub const fn has_window(self) -> bool {
        !matches!(self, Payload::Ident)
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
}

impl Expectation {
    /// The full expected count, given measured [`Weights`] and the retry count the
    /// payload reported (0 for payloads with no reported term).
    ///
    /// Saturating throughout: this is called on untrusted evidence records by the
    /// floor checker, and library code must never panic on untrusted input.
    #[must_use]
    pub fn total(&self, w: &Weights, reported_taken: u64) -> u64 {
        self.certain_taken
            .saturating_add(reported_taken)
            .saturating_add(w.exception_entry.saturating_mul(self.exception_entries))
            .saturating_add(w.exception_return.saturating_mul(self.exception_returns))
            .saturating_add(w.svc_instruction.saturating_mul(self.svc_instructions))
            .saturating_add(w.wfi_instruction.saturating_mul(self.wfi_instructions))
            .saturating_add(w.window_offset)
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
    };

    match payload {
        Payload::Ident => {
            e.certain_taken = 0;
        }
        Payload::StraightLine => {
            e.inline_branch_seq = seq::STRAIGHT_LINE;
        }
        Payload::BranchDense => {
            let mut rng = XorShift64Star::new(seed);
            let mut data_taken: u64 = 0;
            for _ in 0..trips {
                data_taken = data_taken.saturating_add(branch_dense_trip_taken(rng.next_u64()));
            }
            e.certain_taken = back_edges.saturating_add(data_taken);
            e.inline_branch_seq = seq::BRANCH_DENSE;
        }
        Payload::Svc => {
            e.exception_entries = trips;
            e.exception_returns = trips;
            e.svc_instructions = trips;
            e.inline_branch_seq = seq::SVC;
        }
        Payload::ExceptionAbort => {
            e.exception_entries = trips;
            e.exception_returns = trips;
            e.inline_branch_seq = seq::EXCEPTION_ABORT;
        }
        Payload::WfiIdle => {
            e.exception_entries = trips;
            e.exception_returns = trips;
            e.wfi_instructions = trips;
            e.inline_branch_seq = seq::WFI_IDLE;
        }
        Payload::LlscAtomics => {
            e.has_reported_term = true;
            e.inline_branch_seq = seq::LLSC_ATOMICS;
        }
        Payload::LseAtomics => {
            e.inline_branch_seq = seq::LSE_ATOMICS;
        }
        Payload::ClockPage => {
            e.has_reported_term = true;
            e.inline_branch_seq = seq::CLOCK_PAGE;
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
        let predicted = o.expectation.total(&weights, o.reported_taken);
        let r = i128::from(o.measured) - i128::from(predicted);
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
    fn total_saturates_rather_than_overflowing() {
        // The floor checker calls this on untrusted records; a crafted weight must
        // not panic in a release-mode debug_assert or wrap into a plausible value.
        let e = expected(Payload::Svc, Scale::S1e8, DEFAULT_SEED);
        let w = Weights::measured(u64::MAX, u64::MAX, u64::MAX, u64::MAX, u64::MAX);
        assert_eq!(e.total(&w, u64::MAX), u64::MAX);
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
                measured: expectation.total(w, 0),
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
                measured: lse.total(&truth, 0) + 7,
                reported_taken: 0,
            },
        ];
        let solved = solve(&obs).expect("the five solve rows are still consistent");
        assert_eq!(solved.weights, truth);
        assert_eq!(solved.residual, 7, "the unexplained row must be visible");
        assert_eq!(solved.worst, Payload::LseAtomics);

        // And with that row explained, the residual is zero again.
        obs[5].measured = lse.total(&truth, 0);
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
