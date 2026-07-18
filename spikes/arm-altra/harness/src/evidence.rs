// SPDX-License-Identifier: AGPL-3.0-or-later
//! The canonical evidence formats.
//!
//! Everything a stage retains is written by the harness, in stable JSON with
//! sorted keys, never handwritten from terminal output (`docs/ARM-ALTRA.md`
//! §Evidence integrity). The shapes here are the field list that document's §Spike
//! architecture requires of every run, and the floor checkers
//! (`schemas/floor-check`) recompute every acceptance floor from them.
//!
//! # The split, and why it exists
//!
//! A run-set is two files:
//!
//! - **`run-set.json`** — the manifest: environment, mechanism attestation, image
//!   pins, perf configuration, pinning, the experimental condition, the measured
//!   weights, and the *attempted* sample count.
//! - **`records.jsonl`** — one [`RunRecord`] per line, one line per attempted
//!   sample.
//!
//! The manifest states what was *attempted*; the records say what *happened*. A
//! checker that trusted the manifest's own summary of the records would be
//! checking the harness's opinion of itself — the exact pathology
//! §Evidence integrity #2 forbids ("recomputed from the raw per-sample data, not
//! read from a summary line the harness itself asserted"). So the manifest
//! deliberately carries **no** result totals at all. There is no "mismatches: 0"
//! field to believe. The only numbers a checker may use are the ones it derives
//! from `records.jsonl`, whose sha256 the manifest pins so a swapped record file
//! cannot go unnoticed.
//!
//! # The loader enforces what the schema promises
//!
//! The canonical schemas under `schemas/` declare `additionalProperties: false`.
//! Plain serde would happily accept a manifest with extra keys — and, worse, a
//! *misspelled* optional field would silently deserialize to `None`, turning
//! "weights I measured" into "weights I refuse to check" with no complaint. Every
//! shape here is therefore `deny_unknown_fields`, so the Rust loader and the JSON
//! schema agree about what a valid run-set is.
//!
//! The predecessor v3 shape was exercised with retained N1 evidence. Schema v4 is the offline
//! totality/semantics hardening for the next run and has not yet been emitted on the box;
//! historical v3 results remain retained under the checker version that produced their
//! transcripts.

use oracle_model::{Payload, Scale, Weights};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The measured and integrator-ruled ARM work-clock binding (AA1-F1).
///
/// This is deliberately ARM-specific. The x86 work clock remains retired conditional branches.
pub const ARM64_WORK_CLOCK_BINDING: &str = "arm64 BR_RETIRED raw 0x21 = all architecturally executed branch instructions (taken or not; AA1-F1)";

/// Bump when a field's meaning changes. A checker refuses a version it does not
/// know, rather than silently misreading it.
///
/// **v2** added [`OverflowRecord::advisory_exits`] and [`OverflowRecord::landed_digest`].
/// Both exist because of the patch's own arm64 arch-difference: the PMU overflow is
/// an ordinary maskable IRQ there, so an armed vCPU takes `KVM_EXIT_PREEMPT` on *any*
/// host IRQ, and the exit is **advisory** — the harness must re-read the counter and
/// decide whether the deadline was actually reached. Those advisory exits are a real,
/// per-sample fact about the mechanism, so they are recorded rather than silently
/// absorbed; and the state that AA-3's replay identity is *about* is the state at the
/// landing, not at the payload's eventual exit.
///
/// **v3** added [`RunRecord::step`] — the structured single-step measurement AA-2's
/// floor validates, so a bare `exit_reason: debug` can no longer certify the stage.
///
/// **v4** adds [`StepRecord::planned_sample_id`], [`StepRecord::step_index`], and
/// [`StepTransition::NotTakenBranch`]. These are required wire fields/classes: the stable planned
/// id makes step totality non-forgeable after record ids are densely renumbered, while the step
/// position and explicit not-taken class bind replay identity and AA1-F1's all-branch semantics.
pub const SCHEMA_VERSION: u32 = 4;

/// Which stage produced a run-set.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    /// Day-one bring-up + capability truth table.
    Aa0,
    /// The work clock: count exactness, PMI reliability, skid.
    Aa1,
    /// Single-step exactness.
    Aa2,
    /// Deterministic force-exit at PMI + exact landing.
    Aa3,
    /// The LL/SC vs LSE ruling.
    Aa4,
    /// The paravirt work-derived clock.
    Aa5,
    /// Contract enforcement + device model + the mini determinism gate.
    Aa6,
}

/// Which mechanism a run *claims* to have exercised — and the evidence for it.
///
/// `docs/ARM-ALTRA.md` §Evidence integrity #4: a silent fallback path must be
/// **structurally unable** to masquerade as the mechanism under test. The PR-98
/// review found an existential-stage harness quietly exercising the stock backend
/// and reporting green. So the claim and its proof travel together, per run, and
/// the checker rejects a run whose per-record exit reasons do not match the
/// mechanism the manifest claims.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mechanism {
    /// `true` when the host kernel carries the 0004-analogue patch
    /// (`host/patches/`). `false` means stock KVM — which is a legitimate thing to
    /// run (AA-1's pre-patch signal-kick), but it may never be reported as if it
    /// were the patched path.
    pub kvm_patched: bool,
    /// sha256 of the running host kernel image. arm64 KVM is built in
    /// (`CONFIG_KVM=y`), not a module, so there is no `kvm.ko` to hash — the
    /// kernel *is* the module identity, and swapping it is a reboot.
    pub host_kernel_sha256: String,
    /// The exit reason every landing in this run-set must carry. For the patched
    /// path this is [`ExitReason::Preempt`]; for the stock pre-patch path it is
    /// [`ExitReason::SignalKick`]. The checker enforces it per record, so a run
    /// that silently fell back cannot pass.
    pub expected_exit_reason: ExitReason,
    /// Whether the patch's marker was observed in the running kernel (a positive
    /// probe, e.g. the capability advertising itself), not merely assumed from the
    /// build.
    pub patch_marker_observed: bool,
}

/// How a `KVM_RUN` ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExitReason {
    /// `KVM_EXIT_PREEMPT` — the patched in-kernel force-exit (the 0004-analogue).
    Preempt,
    /// A host-side signal kicked the vCPU out of `KVM_RUN`. This is AA-1(c)'s
    /// pre-patch mechanism and AA-3's forbidden fallback.
    SignalKick,
    /// `KVM_EXIT_MMIO` — a console access, including the two window marks.
    Mmio,
    /// `KVM_EXIT_DEBUG` — a single-step landed (AA-2).
    Debug,
    /// Anything else. Always a finding.
    Other,
}

/// A boot artifact, pinned by content and verified immediately before use.
///
/// §Evidence integrity #3: *recording a hash without verifying it is the
/// anti-pattern this rule exists to kill.* Hence [`ImagePin::verified_before_boot`]
/// — the checker rejects any run-set with an unverified pin, so a hash that was
/// merely written down cannot pass for one that was checked.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImagePin {
    /// Path the artifact was loaded from. Never trusted — the hash is the identity.
    pub path: String,
    /// sha256 of the bytes actually loaded. This is the artifact's identity.
    pub sha256: String,
    /// md5 cross-reference, per the box-gate image discipline — **optional**.
    ///
    /// It is a belt-and-suspenders cross-reference, not the identity (sha256 is). No
    /// md5 implementation is on the dependency whitelist, so the harness emits `None`
    /// rather than a placeholder: an empty string would violate the schema's
    /// `^[0-9a-f]{32}$` pattern, making even a good run-set's evidence
    /// schema-invalid. When present it must be 32 lowercase hex; `None` is the honest
    /// "not computed".
    pub md5: Option<String>,
    /// Whether the hash was recomputed and compared **immediately before** the
    /// artifact was used.
    pub verified_before_boot: bool,
}

/// The perf event configuration behind the work clock.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerfConfig {
    /// The raw event. On N1, `BR_RETIRED` is `0x21` and counts all architecturally executed
    /// branch instructions, taken or not (AA1-F1; `docs/ARM-PORT.md`,
    /// `docs/ARM-ALTRA.md` §2). Recorded rather than assumed, so a run that silently counted
    /// a different event is visible as such.
    pub raw_event: u64,
    /// `exclude_host` — the count must be guest-only.
    pub exclude_host: bool,
    /// `exclude_guest`.
    pub exclude_guest: bool,
    /// `exclude_hv`.
    pub exclude_hv: bool,
    /// Whether the event was opened pinned (never multiplexed). A multiplexed
    /// counter scales its count, which would silently corrupt every measurement.
    pub pinned: bool,
    /// The sampling period armed, when the run arms overflow. `None` in counting
    /// mode.
    pub sample_period: Option<u64>,
}

/// Core pinning and frequency posture.
///
/// Pinning is a **correctness** condition on this lineage, not hygiene: the
/// N1/V1 arm64 kernel can miss PMU overflow interrupts on core migration (rr
/// issue #3607), and a missed overflow means `run_until` never breaks out of
/// `KVM_RUN` (`docs/ARM-ALTRA.md` §2). The one sanctioned unpinned run is AA-1's
/// bounded migration probe, which is why [`Pinning::pinned`] can legitimately be
/// `false` — and why the checker demands it be `true` everywhere else.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pinning {
    /// Whether the vCPU thread and its perf context were hard-pinned.
    pub pinned: bool,
    /// The core they were pinned to.
    pub core: Option<u32>,
    /// The cpufreq governor. V-time counts are frequency-independent; this matters
    /// only for wall-clock numbers, and is recorded so that is checkable.
    pub governor: String,
    /// Set only for AA-1's deliberate, bounded migration probe.
    pub migration_probe: bool,
}

/// The machine, as found.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    /// `MIDR_EL1`.
    pub midr: u64,
    /// The SoC part, as reported by the platform.
    pub soc: String,
    /// Firmware versions, free-form key/value. `BTreeMap` (never `HashMap`): key
    /// order reaches the serialized bytes, and the bytes are hashed.
    pub firmware: BTreeMap<String, String>,
    /// `uname -r` of the running host kernel.
    pub host_kernel: String,
    /// `vhe` or `nvhe`.
    pub kvm_mode: String,
}

/// The overflow bookkeeping for one sample.
///
/// §Evidence integrity #6: exactly-once must be shown **per record**, not inferred
/// from totals. [`OverflowRecord::deliveries`] is that per-record multiplicity: a
/// checker sums nothing to establish it, it looks at every record and demands
/// exactly 1.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverflowRecord {
    /// Whether an overflow was armed for this sample.
    pub armed: bool,
    /// How many times the armed overflow was delivered. Must be exactly 1 — 0 is a
    /// lost PMI, >1 is a duplicate, and both are blocking.
    ///
    /// A *delivery* is an exit at which the work counter had actually reached the
    /// target. It is emphatically not "an exit happened": see
    /// [`OverflowRecord::advisory_exits`].
    pub deliveries: u64,
    /// Exits that left `KVM_RUN` while armed but **before the counter reached the
    /// target** — and were therefore not the overflow.
    ///
    /// On arm64 the PMU overflow is an ordinary maskable IRQ, so the patch's armed
    /// vCPU exits on *any* host IRQ (the host timer tick included); the patch's own
    /// commit message says to treat every `KVM_EXIT_PREEMPT` as **advisory** and
    /// re-arm if the target has not been reached. A harness that counted those exits
    /// as deliveries would record an early timer tick as an exactly-once PMI, at a
    /// count that is not the target, and the real overflow would never be seen. So
    /// the harness re-reads the counter, re-arms, re-enters — and records how often
    /// that happened, because "the mechanism fires on unrelated IRQs" is a measured
    /// property of this silicon (AA-3's residual), not noise to hide.
    pub advisory_exits: u64,
    /// The work target the deadline was set for.
    pub target: u64,
    /// The work count actually landed on.
    pub landed: u64,
    /// `landed - target`. Negative means the landing was early (inside the skid
    /// margin, which is the design); positive means it **overshot**, which the
    /// late-only-stop contract forbids outright.
    pub skid: i64,
    /// Digest of the guest state **at the landing** — sampled when the mechanism
    /// exit was taken, before the guest was resumed.
    ///
    /// This, not [`RunRecord::state_digest`], is what AA-3's replay-identity claim is
    /// about: two runs of the same seed must be in the same state *at the same
    /// Moment*. A digest taken after the guest resumed and ran to its exit sentinel
    /// can converge — two different landed states can produce the same final state —
    /// so it cannot establish landing identity. Empty when nothing was delivered.
    pub landed_digest: String,
}

/// What kind of transition a single step made — recorded from the **stepped opcode /
/// trap syndrome**, not inferred from PC arithmetic.
///
/// This matters because `pc_after != pc_before + 4` alone cannot tell a retired taken
/// branch from an *exception* transition (an `SVC`, an abort, an injected IRQ, an
/// `ERET`), and `BR_RETIRED` counts the former but not the latter. AA-2 measures the
/// per-class `BR_RETIRED` behaviour; the class is evidence the harness records at step
/// time (from the instruction it stepped and the exit it took), and the checker validates
/// the delta against it — forcing `delta == 1` only where the architecture guarantees a
/// retired branch, and letting AA-2 *measure* it where it does not.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepTransition {
    /// Fell through to `pc + 4`: a **non-branch** instruction. `BR_RETIRED` must not move.
    /// A not-taken *conditional branch* also lands at `pc + 4`, but it is [`Self::NotTakenBranch`]
    /// — not this — because the branch instruction itself retired.
    Sequential,
    /// An architectural **taken** branch (`B`/`B.cond`/`CBZ`/`TBZ`/`BL`/`BR`/`RET`/…).
    /// `BR_RETIRED` must increment by exactly 1, and the PC lands on the branch target.
    TakenBranch,
    /// A branch instruction that was **not taken** — a conditional (`B.cond`/`CBZ`/`TBZ`/…)
    /// whose predicate failed, so the PC fell through to `pc + 4`. It lands at `pc + 4` like a
    /// [`Self::Sequential`] step, but the branch *instruction retired*: on N1 `BR_RETIRED`
    /// counts branch INSTRUCTIONS, taken AND not-taken (finding AA1-F1), so the delta is exactly
    /// one. Distinguishing it from `Sequential` (a non-branch, delta 0) is what keeps a not-taken
    /// branch from failing the sequential-step rule (`delta == 0`), and a non-branch fall-through
    /// from being graded as though a branch retired.
    NotTakenBranch,
    /// Synchronous **exception entry** — `SVC`, or a data/instruction abort. Not a
    /// retired branch instruction; the `BR_RETIRED` delta is AA-2's to measure.
    ExceptionEntry,
    /// **`ERET`** — exception return. Its `BR_RETIRED` weight is unknown by construction
    /// (the oracle model's ambiguity term); AA-2 measures it.
    ExceptionReturn,
    /// **`WFI`** — waited and was resumed by an interrupt. Measured.
    Wfi,
    /// An **injected interrupt** boundary (asynchronous IRQ, AA-6's mechanism). Measured.
    Injection,
    /// Stepping an **LL/SC exclusive** (`LDXR`/`STXR`) sequence — the monitor-clearing /
    /// livelock behaviour AA-2 must characterize for AA-4's LSE-only contract. Required
    /// coverage: an AA-2 run that never stepped an exclusive has not measured it.
    LlscExclusive,
}

/// One single-step measurement (AA-2).
///
/// AA-2 exists to *characterize single-stepping*: does one step retire exactly one
/// instruction, and what is each class's `BR_RETIRED` weight. The evidence is not the
/// enum label `exit_reason: debug` — that a rehashed run-set can flip in one byte — but
/// these measured numbers plus the [`StepTransition`] class recorded from the stepped
/// opcode. The AA-2 floor validates them: a step must advance the PC and retire exactly
/// one instruction, and its `br_retired_delta` must agree with its transition class.
///
/// This shape was exercised on N1 by AA-2. Schema v4 adds the stable planned-sample identity
/// needed to make totality independently certifiable from retained step records.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepRecord {
    /// Stable id of the plan entry that emitted this step.
    ///
    /// Every step from one planned run carries the same id, copied from the pre-execution plan.
    /// Unlike [`RunRecord::sample_id`], it is never densely renumbered after multiple stepped runs
    /// are concatenated. Step totality requires the distinct ids to be exactly `0..planned`, so
    /// duplicating one run's step-zero record cannot conceal a different dropped plan entry.
    pub planned_sample_id: u64,
    /// This step's **position within its stepped run**, 0-based. Set by [`crate::run::step_run`]
    /// at emission and never renumbered (the caller reassigns the record-level
    /// [`RunRecord::sample_id`] across every planned run, but this stays the within-run index).
    /// It is the key AA-2's replay identity groups on: step N of one rep is compared to step N of
    /// another, so a loop that revisits one `pc_before` across iterations — each a different step
    /// with a different `step_digest` — is not read as a false divergence, and a rep with fewer
    /// steps surfaces as a missing position (a real divergence).
    pub step_index: u64,
    /// The PC before the step.
    pub pc_before: u64,
    /// The PC after the step — must differ from `pc_before`.
    pub pc_after: u64,
    /// Instructions retired by the step. AA-2's single-step semantics require exactly 1.
    pub insn_retired: u64,
    /// `BR_RETIRED` delta across the step. Validated against [`StepRecord::transition`]:
    /// 0 for [`StepTransition::Sequential`], 1 for [`StepTransition::TakenBranch`] and
    /// [`StepTransition::NotTakenBranch`] (on N1 a not-taken branch still retires the branch
    /// instruction, AA1-F1), and a measured 0-or-1 for the exception/WFI/injection classes AA-2
    /// characterizes.
    pub br_retired_delta: u64,
    /// What kind of transition the step made, from the stepped opcode and the exit taken.
    pub transition: StepTransition,
    /// Digest of the guest state **at the step Moment** — sampled at the single step, not
    /// at the exit sentinel. AA-2's acceptance is replay-identical stepped states across
    /// repeated inputs, and the final `state_digest` can converge (two divergent step
    /// states running on to the same exit), so replay identity compares THIS.
    pub step_digest: String,
}

/// One attempted sample.
///
/// Every attempted sample gets a record, including ones that failed — §Evidence
/// integrity #6: *a missing sample is a failure to account, not a pass.* There is
/// no way to express "this sample didn't work out" by omitting it; omission is
/// what the totality check catches.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    /// Dense index into the attempted samples, `0..attempted`. The totality check
    /// is exactly: these are present, once each, with no gaps.
    pub sample_id: u64,
    /// The payload.
    pub payload: Payload,
    /// The scale.
    pub scale: Scale,
    /// The seed the params page carried.
    pub seed: u64,
    /// The trip count the payload was given.
    pub trips: u64,
    /// The experimental condition (`pinned-solo`, `co-tenant-load`, …).
    pub condition: String,
    /// `BR_RETIRED` sampled at the window's opening mark.
    pub work_begin: u64,
    /// `BR_RETIRED` sampled at the window's closing mark.
    pub work_end: u64,
    /// `work_end - work_begin` — the measured taken-branch count of the window.
    /// The checker recomputes this from the two endpoints rather than believing it.
    pub measured_taken: u64,
    /// The retry count the payload reported in-band (`STXR` retries, seqlock
    /// retries). Zero for payloads with no reported term.
    pub reported_taken: u64,
    /// How this sample's `KVM_RUN` ended. Checked against
    /// [`Mechanism::expected_exit_reason`].
    pub exit_reason: ExitReason,
    /// Overflow bookkeeping, when the sample armed one.
    pub overflow: Option<OverflowRecord>,
    /// Single-step measurement, when this sample was a stepped one (AA-2). `None` for
    /// every non-stepped run. The AA-2 floor requires this structured evidence — a
    /// bare `exit_reason: debug` proves nothing — and validates that the step retired
    /// exactly one instruction and advanced the PC.
    pub step: Option<StepRecord>,
    /// Digest of the landed guest state, for replay-identity checks.
    pub state_digest: String,
    /// The `PARAMS mode=` the guest itself printed. The checker demands `managed`:
    /// a harness that forgot to publish the params page would otherwise run the
    /// smoke scale while the manifest claimed 1e8.
    pub params_mode: String,
    /// The `CLOCKPAGE mode=` the guest printed, for AA-5 runs.
    pub clockpage_mode: Option<String>,
    /// The payload's own exit status. Nonzero means the payload's in-guest
    /// self-checks failed, and the sample is a failure however good its counts look.
    pub payload_status: i32,
}

/// The run-set manifest.
///
/// Carries **no result totals** — see the module docs. It says what was attempted
/// and under what conditions; the records say what happened.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSet {
    /// [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The stage.
    pub stage: Stage,
    /// Identifier of this run-set. Golden evidence is immutable; a rerun creates a
    /// new run-set rather than overwriting one.
    pub run_set_id: String,
    /// The machine.
    pub environment: Environment,
    /// What mechanism this run claims, and the proof.
    pub mechanism: Mechanism,
    /// Every boot artifact, pinned and verified.
    pub images: Vec<ImagePin>,
    /// The work counter's configuration.
    pub perf: PerfConfig,
    /// Core pinning and frequency posture.
    pub pinning: Pinning,
    /// The experimental condition this run-set sweeps.
    pub condition: String,
    /// The measured [`Weights`] used to predict counts.
    ///
    /// `None` before stage AA-1 has produced them — and a checker handed `None`
    /// **refuses to check counts** rather than falling back to a guess. Task 109:
    /// "skid margins, densities, and count offsets are spike deliverables — the
    /// apparatus must treat them as unknowns (parameters), never defaults."
    pub weights: Option<Weights>,
    /// The N1 skid margin, measured by AA-1. `None` for the same reason as
    /// [`RunSet::weights`]; a checker cannot bound skid without it and must say so.
    pub skid_margin: Option<u64>,
    /// How many samples were **attempted**. Every one of them must appear in the
    /// records file. In single-step mode one attempted sample emits MANY step
    /// records, so `attempted` is the STEP count there, not the plan size — see
    /// [`RunSet::planned`].
    pub attempted: u64,
    /// How many **planned samples** the run intended to measure (the plan length).
    /// Equals `attempted` for ordinary runs; in single-step mode it is DISTINCT
    /// from `attempted` (which is the step count), and the totality checker uses it
    /// to verify every planned sample is represented by its stable
    /// [`StepRecord::planned_sample_id`] — dense step renumbering or duplicated step-zero rows
    /// must not let a dropped planned sample pass as a complete `0..attempted`.
    /// Required in schema v4; older schemas are intentionally not accepted by the
    /// v4 checker.
    pub planned: u64,
    /// The records file, relative to the manifest.
    pub records_file: String,
    /// sha256 of the records file, so a swapped or truncated one is caught.
    pub records_sha256: String,
}

/// Everything a run-set manifest needs that cannot be derived from the records.
///
/// The split is the point: what the harness *can* derive it derives (the records'
/// sha256, the perf block from the armed `perf_event_attr`, the pinned core), and
/// what it cannot know it must be *given* — the machine's identity, the measured
/// weights, the measured skid margin. There is no field here the harness fills with
/// a plausible default, because a plausible default is how an unmeasured constant
/// becomes a published one.
#[derive(Clone, Debug)]
pub struct RunSetContext {
    /// The stage this run-set belongs to.
    pub stage: Stage,
    /// Identifier for this run-set.
    pub run_set_id: String,
    /// The machine, from AA-0's capture.
    pub environment: Environment,
    /// The mechanism claimed, and its proof.
    pub mechanism: Mechanism,
    /// The boot artifacts, pinned and verified.
    pub images: Vec<ImagePin>,
    /// The work counter's configuration, as armed.
    pub perf: PerfConfig,
    /// Core pinning and frequency posture.
    pub pinning: Pinning,
    /// The experimental condition swept.
    pub condition: String,
    /// The measured weights (AA-1). `None` until they exist.
    pub weights: Option<Weights>,
    /// The measured skid margin (AA-1). `None` until it exists.
    pub skid_margin: Option<u64>,
    /// How many samples were **attempted** — for an ordinary run this is the plan's
    /// length; in single-step mode the caller sets it to the STEP count (one plan
    /// sample emits many step records).
    pub attempted: u64,
    /// How many **planned samples** the run intended — the plan length, always,
    /// regardless of mode. Equals `attempted` for ordinary runs and is DISTINCT from
    /// it in single-step mode, where the totality checker needs it to verify every
    /// planned sample is represented (see [`RunSet::planned`]).
    pub planned: u64,
}

/// Assemble a run-set: serialize the records, pin their real sha256, emit the
/// manifest.
///
/// Two properties, both load-bearing:
///
/// - **The records' hash is of the bytes actually written.** A manifest cannot pin a
///   hash of records that were never emitted.
/// - **`attempted` comes from the plan, not from `records.len()`.** A run that died
///   after 300 of 1,000 samples writes a manifest saying 1,000 attempted and 300
///   records — evidence that indicts itself, which the totality check then catches.
///   Deriving `attempted` from the records would let a truncated run look complete,
///   and *that* is how a missing sample becomes a pass.
///
/// # Errors
/// The underlying `serde_json` error if a record or the manifest cannot be
/// serialized.
pub fn assemble_run_set(
    ctx: RunSetContext,
    records: &[RunRecord],
) -> Result<(String, String), serde_json::Error> {
    let mut jsonl = String::new();
    for r in records {
        jsonl.push_str(&serde_json::to_string(r)?);
        jsonl.push('\n');
    }

    let mut hasher = Sha256::new();
    hasher.update(jsonl.as_bytes());
    let records_sha256 = hex_lower(&hasher.finalize());

    let run_set = RunSet {
        schema_version: SCHEMA_VERSION,
        stage: ctx.stage,
        run_set_id: ctx.run_set_id,
        environment: ctx.environment,
        mechanism: ctx.mechanism,
        images: ctx.images,
        perf: ctx.perf,
        pinning: ctx.pinning,
        condition: ctx.condition,
        weights: ctx.weights,
        skid_margin: ctx.skid_margin,
        attempted: ctx.attempted,
        planned: ctx.planned,
        records_file: "records.jsonl".to_string(),
        records_sha256,
    };
    Ok((to_stable_json(&run_set)?, jsonl))
}

/// Lowercase-hex-encode a byte slice.
///
/// No `hex` crate is on the dependency whitelist, and hashes reach evidence from
/// three places (the state digest, the records pin, the fixture generator) — so the
/// encoding lives once, here, rather than three times.
#[must_use]
pub fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // Writing to a String is infallible; the Result exists only to satisfy the
        // `Write` trait.
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Serialize to stable JSON: sorted keys, deterministic bytes.
///
/// `serde_json`'s object serializer preserves the order the type declares its
/// fields in, which is stable across runs — but a `BTreeMap` is used wherever the
/// *keys* are data rather than field names, so no map iteration order can reach
/// the output. (Rule 4 of `tasks/00-CONVENTIONS.md`, and the reason this project
/// exists at all.)
///
/// # Errors
/// Returns the underlying `serde_json` error if the value cannot be serialized.
pub fn to_stable_json<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_record() -> RunRecord {
        RunRecord {
            sample_id: 0,
            payload: Payload::Svc,
            scale: Scale::S1e6,
            seed: 1,
            trips: 1_000_000,
            condition: "pinned-solo".into(),
            work_begin: 100,
            work_end: 1_000_099,
            measured_taken: 999_999,
            reported_taken: 0,
            exit_reason: ExitReason::Preempt,
            overflow: Some(OverflowRecord {
                armed: true,
                deliveries: 1,
                advisory_exits: 0,
                target: 999_999,
                landed: 999_999,
                skid: 0,
                landed_digest: "sha256:aa".into(),
            }),
            step: None,
            state_digest: "sha256:00".into(),
            params_mode: "managed".into(),
            clockpage_mode: None,
            payload_status: 0,
        }
    }

    #[test]
    fn records_round_trip_through_stable_json() {
        let r = a_record();
        let json = to_stable_json(&r).expect("serializable");
        let back: RunRecord = serde_json::from_str(&json).expect("deserializable");
        assert_eq!(r, back);
    }

    #[test]
    fn serialization_is_byte_stable_across_repeats() {
        // Evidence is hashed; if the bytes moved between two serializations of the
        // same value, every pin downstream would be noise.
        let r = a_record();
        assert_eq!(
            to_stable_json(&r).expect("serializable"),
            to_stable_json(&r).expect("serializable")
        );
    }

    #[test]
    fn an_unknown_field_is_refused_not_ignored() {
        // The schemas say `additionalProperties: false`; the loader must agree. The
        // dangerous half of this is not the extra key — it is the MISSPELLED one: a
        // manifest carrying `skid_margins` would otherwise deserialize with
        // `skid_margin: None`, and the checker would dutifully report "no measured
        // margin" for evidence that had one.
        let json = to_stable_json(&a_record()).expect("serializable");
        let with_extra = json.replace(
            "\"sample_id\": 0,",
            "\"sample_id\": 0,\n  \"surprise\": true,",
        );
        assert!(
            serde_json::from_str::<RunRecord>(&with_extra).is_err(),
            "an unknown field must be refused, not silently ignored"
        );
    }

    #[test]
    fn hex_is_lowercase_and_zero_padded() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xff]), "000fff");
        assert_eq!(hex_lower(&[]), "");
    }

    #[test]
    fn firmware_map_order_is_key_order_not_insertion_order() {
        // The environment is hashed into the evidence. A HashMap here would make
        // the bytes depend on iteration order — the determinism bug this whole
        // project exists to eliminate. BTreeMap makes that impossible; this test
        // is what notices if someone "helpfully" changes the type.
        let mut a = BTreeMap::new();
        a.insert("z".to_string(), "1".to_string());
        a.insert("a".to_string(), "2".to_string());
        let mut b = BTreeMap::new();
        b.insert("a".to_string(), "2".to_string());
        b.insert("z".to_string(), "1".to_string());
        assert_eq!(
            serde_json::to_string(&a).expect("serializable"),
            serde_json::to_string(&b).expect("serializable")
        );
    }
}
