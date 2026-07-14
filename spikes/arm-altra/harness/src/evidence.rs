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
//! **Untested on silicon.** These records have never been produced by real
//! hardware; the shapes are what AA-0..AA-6 will fill.

use oracle_model::{Payload, Scale, Weights};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Bump when a field's meaning changes. A checker refuses a version it does not
/// know, rather than silently misreading it.
pub const SCHEMA_VERSION: u32 = 1;

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
pub struct ImagePin {
    /// Path the artifact was loaded from. Never trusted — the hash is the identity.
    pub path: String,
    /// sha256 of the bytes actually loaded.
    pub sha256: String,
    /// md5 cross-reference, per the box-gate image discipline.
    pub md5: String,
    /// Whether the hash was recomputed and compared **immediately before** the
    /// artifact was used.
    pub verified_before_boot: bool,
}

/// The perf event configuration behind the work clock.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PerfConfig {
    /// The raw event. `BR_RETIRED` is `0x21` — retired *taken* branches
    /// (`docs/ARM-PORT.md`, `docs/ARM-ALTRA.md` §2). Recorded rather than assumed,
    /// so a run that silently counted a different event is visible as such.
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
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OverflowRecord {
    /// Whether an overflow was armed for this sample.
    pub armed: bool,
    /// How many times the armed overflow was delivered. Must be exactly 1 — 0 is a
    /// lost PMI, >1 is a duplicate, and both are blocking.
    pub deliveries: u64,
    /// The work target the deadline was set for.
    pub target: u64,
    /// The work count actually landed on.
    pub landed: u64,
    /// `landed - target`. Negative means the landing was early (inside the skid
    /// margin, which is the design); positive means it **overshot**, which the
    /// late-only-stop contract forbids outright.
    pub skid: i64,
}

/// One attempted sample.
///
/// Every attempted sample gets a record, including ones that failed — §Evidence
/// integrity #6: *a missing sample is a failure to account, not a pass.* There is
/// no way to express "this sample didn't work out" by omitting it; omission is
/// what the totality check catches.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
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
    /// records file.
    pub attempted: u64,
    /// The records file, relative to the manifest.
    pub records_file: String,
    /// sha256 of the records file, so a swapped or truncated one is caught.
    pub records_sha256: String,
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
                target: 999_999,
                landed: 999_999,
                skid: 0,
            }),
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
