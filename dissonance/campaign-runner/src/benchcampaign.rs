// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task 69 — the **signal-configured** benchmark campaign driver.
//!
//! Task 60's campaign ([`crate::campaign`]) is blind seed search — that is the
//! **baseline** configuration. This module adds the **signal** configuration: the
//! same machine + oracle, but the branch-base choice is steered by *cell
//! novelty* derived from the Phase-D log-template signal (task 67). Both
//! configurations run on identical budgets and emit a
//! [`benchmark::report::CampaignLog`] — per-branch discovered cells + per-bug find
//! branch — which `benchmark`'s offline report analyses for signal→bug
//! correlation (GO/NO-GO #2).
//!
//! The cells are the **real task-67 signal under test** (GO/NO-GO #2 ruling, user
//! 2026-07-06 — the gate must measure the actual CellFn the selectors get built on,
//! never a stand-in): a campaign-persistent [`LogSensor`] clusters the guest console
//! into template species, and [`CellFnV1`] keys the accumulating species slice into
//! bounded cells (see [`cells_of`] / [`SignalCells`]). One sensor per `(config, seed)`
//! campaign — independent across seeds, so the seeds parallelize, which is safe
//! because `CellFnV1`'s default key is a function of the distinct-species *count*
//! (species-progress + last-new-species), not of which template got which id, so two
//! seeds keying the same abstract slice still agree and the report can pool them.
//!
//! ## Why a bespoke loop (not `Explorer`)
//!
//! The log-template signal is **point-in-time over a whole run's console**, not a
//! per-fork coverage bitmap; the toy guest surfaces no sealable fork points, so
//! [`explorer::CoverageArchive`]'s fork-time admission has nothing to key on.
//! This driver therefore owns the signal directly: per branch it builds the
//! [`RunTrace`] (with the console captured through the [`Machine::console`] seam —
//! the task-69 socket-console-capture prereq), runs the `LogSensor`/`CellFnV1`
//! over it into the branch's **cell set** ([`cells_of`]), and admits an exemplar to
//! a thin novelty archive. Signal exploits novel exemplars; baseline always explores
//! from genesis. Everything is a pure function of `(campaign_seed, spec, config)`, so
//! a rerun is bit-identical (the determinism property the box campaign stress-tests
//! by comparing solo vs co-tenant state hashes).
//!
//! The toy path (portable gates) and the box `SocketMachine` path run the identical
//! signal code — the toy guest emits a proximity-graded console so the same
//! `LogSensor`/`CellFnV1` has a species ladder to key, making the portable suite a
//! faithful proxy of the box campaign.

use std::collections::BTreeSet;

use benchmark::manifest::{BugId, BugSpec, TriggerParams};
use benchmark::report::{BranchEvent, CampaignLog, Configuration, FindRecord};
use benchmark::trigger::{self, FaultKind, Perturbation, Scenario};
use environment::{BitMask, EnvSpec, FaultPolicy, HostFault};
use explorer::{
    AdapterEnv, CellFn, EvidenceCut, FeatureSet, Machine, MachineError, Moment, Prng, Record,
    Reproducer, RunTrace, Sensor, SnapId, StopConditions, StopMask, StopReason, StreamId,
};
use logtmpl::{CellFnV1, LogSensor};

/// The base V-time the benchmark toy guest is quiescent at when snapshotted —
/// mirrors `crate::planted::BASE_VTIME` and the manifest's window anchors.
pub const BASE_VTIME: u64 = 1_000;

/// One benchmark campaign's budget + search knobs. A pure function of these.
#[derive(Clone, Debug)]
pub struct BenchConfig {
    /// Seeds the campaign stream (the whole run is a pure function of this).
    pub campaign_seed: u64,
    /// Search budget: at most this many branches.
    pub max_branches: u64,
    /// N for the N/N replay verification of a find (25 on the box).
    pub replay_n: usize,
    /// Signal only: every Nth step explores fresh from genesis; the rest exploit
    /// a novel frontier exemplar. Ignored by the baseline (always explores). Set
    /// from the recorded `--explore-period` CLI flag (no ambient env); lands in the
    /// `CampaignLog` so a run is self-describing (PR#90 round-2).
    pub explore_period: u64,
    /// Bug-2 (`OrderingInterrupt`) mint's fault-offset search width past the seal.
    /// Set from the recorded `--order-range` CLI flag (no ambient env); lands in the
    /// `CampaignLog`. Threaded into [`mint_scenario_env`] as an explicit argument,
    /// never an env read on the deterministic path (PR#90 round-2).
    pub order_range: u64,
    /// The manifest-frame base V-time to **subtract** from a fault's `Moment` when
    /// minting an env, so it is keyed **relative to the sealed base** (task 69 M2).
    ///
    /// The manifest windows are absolute in the toy's frame (`BASE_VTIME + offset`).
    /// The toy [`Machine`] reads that `Moment` verbatim, so it wants `0` here (no
    /// rebase — absolute frame). The [`SocketMachine`](explorer::SocketMachine) box
    /// path, however, re-anchors a branch env's override keys by **adding the
    /// snapshot's real seal V-time** (`adapter::rebase_to_wire`); an absolute
    /// manifest `Moment` would then land at `seal + BASE + offset`, past the real
    /// vulnerable window (`seal + offset`), so the bug never fires. Subtracting
    /// [`BASE_VTIME`] here keys the fault at the bare `offset`, and the adapter's
    /// `+ seal` restores the correct absolute window on the box. Pure and
    /// per-seed-deterministic (a constant subtraction; the PRNG draw is unchanged).
    pub fault_rebase: u64,
    /// Per-branch run bound (V-time past the sealed base), or `None` to run each
    /// branch to its natural terminal. The toy runs to an instant terminal so it
    /// wants `None`; the box path sets a deadline so a **non-triggering** branch
    /// stops at the bound instead of running the guest's whole loop, and a hung
    /// guest can never wedge a multi-hour campaign (task 60's discipline). A
    /// triggering branch still crashes *before* the deadline, so the deadline never
    /// perturbs a find's `(stop, state_hash)`.
    pub deadline_delta: Option<u64>,
    /// Box path: V-time to advance on each `NotQuiescent` snapshot retry when
    /// sealing the base (the guest is mid-workload at its readiness marker, not
    /// necessarily on a snapshottable boundary — task 41 / task 60). A fine step
    /// seals the base close to the marker, maximizing the fault window. Unused on
    /// the toy path (quiescent at boot, snapshots first-try).
    pub snapshot_retry_step: u64,
    /// Box path: give up sealing the base after this many `NotQuiescent` retries
    /// (a loud failure, never a silent no-seal).
    pub snapshot_max_attempts: usize,
}

impl BenchConfig {
    /// A small portable/smoke configuration for the **toy** path (absolute frame,
    /// no fault rebase).
    pub fn smoke(campaign_seed: u64) -> Self {
        Self {
            campaign_seed,
            max_branches: 2048,
            replay_n: 25,
            explore_period: 4,
            order_range: 64,
            fault_rebase: 0,
            deadline_delta: None,
            snapshot_retry_step: 0,
            snapshot_max_attempts: 0,
        }
    }

    /// A **box** campaign configuration driving a real [`SocketMachine`]: the same
    /// search knobs, but fault moments are rebased by [`BASE_VTIME`] so the
    /// adapter's `+ seal` re-anchoring lands them in the guest's real window
    /// ([`fault_rebase`](Self::fault_rebase)), and each branch is bounded by
    /// `deadline_delta` V-time past the base so a non-triggering / hung branch can
    /// never wedge the run.
    pub fn box_campaign(
        campaign_seed: u64,
        max_branches: u64,
        replay_n: usize,
        deadline_delta: u64,
    ) -> Self {
        Self {
            campaign_seed,
            max_branches,
            replay_n,
            // Defaults; the box CLI (`run_bench_campaign_box`) overrides
            // explore_period / order_range from the recorded `--explore-period` /
            // `--order-range` flags (env fallback), so the effective values are in
            // the CampaignLog rather than read ambiently here (PR#90 round-2).
            explore_period: 4,
            order_range: 64,
            fault_rebase: BASE_VTIME,
            deadline_delta: Some(deadline_delta),
            // Task-60's box defaults: a fine 10k-V-time retry step seals close to
            // the marker; up to 200k attempts before a loud give-up.
            snapshot_retry_step: 10_000,
            snapshot_max_attempts: 200_000,
        }
    }
}

/// Mint one branch's environment for `spec`'s bug: a seeded base plus (for the
/// fault-triggered classes) a single host-fault schedule drawn from `seed` over a
/// search space that brackets the trigger. Pure in `(seed, rebase)`.
///
/// `rebase` is subtracted from each fault's window-derived `Moment` so it is keyed
/// relative to the sealed base (see [`BenchConfig::fault_rebase`]): `0` on the toy
/// path (absolute manifest frame), [`BASE_VTIME`] on the box `SocketMachine` path
/// (the adapter re-adds the real seal V-time). The subtraction is a constant, so
/// the PRNG draw sequence — and thus which schedules the search visits — is
/// identical across frames.
///
/// `order_range` is the bug-2 (`OrderingInterrupt`) fault-offset search width — an
/// EXPLICIT argument (from [`BenchConfig::order_range`], recorded in the log), never
/// an ambient env read (PR#90 round-2). Ignored by the other trigger classes.
pub fn mint_scenario_env(seed: u64, spec: &BugSpec, rebase: u64, order_range: u64) -> Reproducer {
    let mut p = Prng::new(seed);
    let mut env_spec = EnvSpec::Seeded {
        seed,
        policy: FaultPolicy::none(),
    };
    match spec.trigger {
        TriggerParams::FaultTiming { gpa, mask, window } => {
            // Search space brackets the trigger so it is findable within budget.
            let gpa_pick = one_of(&[gpa, gpa ^ 0x1000, gpa + 0x2000, 0x1000], &mut p);
            let bit = one_of(&mask_bits(mask), &mut p) % 64;
            let at = window.0.saturating_sub(rebase).saturating_sub(4) + p.next_u64() % 16;
            env_spec.perturb(
                HostFault::CorruptMemory {
                    gpa: gpa_pick,
                    mask: BitMask(1u64 << bit),
                },
                at,
            );
        }
        TriggerParams::OrderingInterrupt { vector, window } => {
            // Match the documented ~1/256 rate (10²–10³ branches): 16 candidate
            // vectors (P(right vector) = 1/16) × a 64-wide offset range over a
            // 4-wide window (P(in window) = 4/64 = 1/16) ⇒ 1/256. An earlier
            // 4-vector × 16-offset space fired at ~1/16 (too easy — round-2 P2).
            let v = vector as u64;
            let vectors: Vec<u64> = (0..16).map(|k| v ^ k).collect();
            let vec_pick = one_of(&vectors, &mut p) as u32;
            // Offset search range past the seal (`order_range`, the recorded
            // `--order-range`). Default 64; widening it lets the search reach the
            // per-iteration vulnerable windows and dials the fire rate into the
            // ~10²–10³ band (M2 bug-2 box calibration). An explicit argument, so the
            // mint is a pure function of (seed, spec, rebase, order_range) with no
            // ambient env read (PR#90 round-2).
            let range = order_range.max(1);
            let at = window.0.saturating_sub(rebase).saturating_sub(4) + p.next_u64() % range;
            env_spec.perturb(HostFault::InjectInterrupt { vector: vec_pick }, at);
        }
        // Rare-entropy fires on the seed alone — no fault schedule.
        TriggerParams::RareEntropy { .. } => {}
        // TriggerParams is #[non_exhaustive] (bugs iv/v/vi slot in later); an
        // unknown class mints a plain seeded env (never fires here).
        _ => {}
    }
    AdapterEnv {
        base_offset: 0,
        pos: 0,
        spec: env_spec,
    }
    .encode()
}

fn mask_bits(mask: u64) -> Vec<u64> {
    let trigger_bit = mask.trailing_zeros() as u64;
    vec![trigger_bit, 7, 15, 30]
}

fn one_of(xs: &[u64], p: &mut Prng) -> u64 {
    xs[(p.next_u64() % xs.len() as u64) as usize]
}

/// Decode a branch env back into the toy trigger's [`Scenario`] vocabulary: the
/// seed plus the host-fault schedule, mapped to `benchmark`'s fault kinds. A
/// malformed blob decodes to an empty (never-firing) scenario — the fail-safe.
fn scenario_of(env: &Reproducer) -> Scenario {
    let Ok(decoded) = AdapterEnv::decode(env) else {
        return Scenario::default();
    };
    // Every spec variant carries the base seed (`EnvSpec::seed()`) — including a
    // `Recorded` env minted by `SpecEnvCodec::mutate` on an exploited exemplar.
    // Reading it here (rather than zeroing non-`Seeded` specs) keeps the
    // rare-entropy bug searchable under the signal config's exploit branches.
    let seed = decoded.spec.seed();
    let faults = decoded
        .spec
        .host_faults()
        .filter_map(|(at, f)| {
            let kind = match f {
                HostFault::CorruptMemory { gpa, mask } => {
                    Some(FaultKind::CorruptMemory { gpa, mask: mask.0 })
                }
                // The benchmark manifest's vector is the 8-bit x86 identity; the
                // wire field is wider (ARCH-BOUNDARY §C), but nothing mints an
                // out-of-range vector for these scenarios.
                HostFault::InjectInterrupt { vector } => u8::try_from(vector)
                    .ok()
                    .map(|vector| FaultKind::InjectInterrupt { vector }),
                _ => None,
            };
            kind.map(|kind| Perturbation { at, kind })
        })
        .collect();
    Scenario { seed, faults }
}

/// The signal config's **exploit kernel** (task-69 M2 fix B): a small, generic,
/// BUG-AGNOSTIC perturbation of a novel parent's *existing* fault schedule. It
/// knows nothing of the planted trigger — it only jitters what the parent already
/// carries (fault timing, a nearby gpa, a neighbouring corruption bit, a nearby
/// interrupt vector), so a novel-cell parent that happens to sit near the trigger
/// gets its neighbourhood searched while one far away wanders locally. Every draw
/// is from the campaign's seeded `prng` (no ad-hoc entropy), so the whole campaign
/// stays a pure function of the seed. The env is rebuilt exactly as
/// [`mint_scenario_env`] builds an explore branch (a `Seeded` base plus `perturb`
/// faults, `base_offset` 0), so an exploit branch keys off the sealed base
/// identically to an explore branch — the two configs differ ONLY in *which* base
/// env a branch starts from, which is the variable the GO/NO-GO measures.
///
/// This replaces the earlier `EnvCodec::mutate` exploit, whose generic
/// `host_fault_from` drew a uniform-random 64-bit host fault — almost always an
/// out-of-range gpa or an out-of-scope `SkewTime`/`SetClockRate` the backend
/// rejects — so "exploitation" was a wasted (and, pre-fix-A, campaign-aborting)
/// branch rather than a local search (round-8 P0).
fn exploit_env(parent: &Reproducer, prng: &mut Prng) -> Reproducer {
    let sc = scenario_of(parent);
    if sc.faults.is_empty() {
        // A fault-less parent (a rare-entropy bug fires on the seed alone): the
        // only thing to jitter is the seed itself. A single-bit twiddle keeps the
        // exploit "near" the parent; entropy has no seed-locality, so this is
        // honest local search, not a claim that exploitation helps a seed-only bug
        // (called out in CORRELATION-REPORT.md).
        let seed = sc.seed ^ (1u64 << (prng.next_u64() % 64));
        return AdapterEnv {
            base_offset: 0,
            pos: 0,
            spec: EnvSpec::Seeded {
                seed,
                policy: FaultPolicy::none(),
            },
        }
        .encode();
    }
    // Jitter exactly one fault; carry the rest verbatim.
    let victim = (prng.next_u64() % sc.faults.len() as u64) as usize;
    let mut env_spec = EnvSpec::Seeded {
        seed: sc.seed,
        policy: FaultPolicy::none(),
    };
    for (i, p) in sc.faults.iter().enumerate() {
        let (at, kind) = if i == victim {
            perturb_fault(p.at, p.kind, prng)
        } else {
            (p.at, p.kind)
        };
        let fault = match kind {
            FaultKind::CorruptMemory { gpa, mask } => HostFault::CorruptMemory {
                gpa,
                mask: BitMask(mask),
            },
            FaultKind::InjectInterrupt { vector } => HostFault::InjectInterrupt {
                vector: vector.into(),
            },
        };
        env_spec.perturb(fault, at);
    }
    AdapterEnv {
        base_offset: 0,
        pos: 0,
        spec: env_spec,
    }
    .encode()
}

/// One small, generic jitter of a single staged fault for [`exploit_env`],
/// perturbing exactly **one** dimension and leaving the rest as the parent. This
/// one-dimension-at-a-time step is what lets exploitation *converge* on a
/// conjunctive trigger (e.g. bug 1 needs the right gpa AND bit AND timing slot
/// together): a parent that already matches two of three dimensions is fixed on
/// the third without disturbing the two it got right — whereas jittering every
/// dimension at once could never hold a match. Bug-agnostic and seeded-
/// deterministic; each step is small enough to stay in the parent's in-range
/// neighbourhood (the rare boundary escapee is caught by fix A's skip, never
/// fatal).
fn perturb_fault(at: u64, kind: FaultKind, prng: &mut Prng) -> (u64, FaultKind) {
    match kind {
        // Corrupt-memory faults have three search dimensions — timing, gpa, bit —
        // so pick one to nudge.
        FaultKind::CorruptMemory { gpa, mask } => match prng.next_u64() % 3 {
            0 => {
                // Timing jitter: a small signed step in [-32, 32] (saturating).
                let d = (prng.next_u64() % 65) as i64 - 32;
                (at.saturating_add_signed(d), kind)
            }
            1 => {
                // Nearby gpa: a small aligned signed step (a few 8-byte words / a
                // page either way).
                const STEPS: [i64; 6] = [-0x2000, -0x1000, -8, 8, 0x1000, 0x2000];
                let d = STEPS[(prng.next_u64() % STEPS.len() as u64) as usize];
                (
                    at,
                    FaultKind::CorruptMemory {
                        gpa: gpa.saturating_add_signed(d),
                        mask,
                    },
                )
            }
            _ => {
                // Bit twiddle: move the corruption to a neighbouring bit.
                let bit = mask.trailing_zeros() as i64;
                let delta = if prng.next_u64().is_multiple_of(2) {
                    -1
                } else {
                    1
                };
                let nb = (bit + delta).clamp(0, 63) as u64;
                (
                    at,
                    FaultKind::CorruptMemory {
                        gpa,
                        mask: 1u64 << nb,
                    },
                )
            }
        },
        // Interrupt faults have two dimensions — timing and vector.
        FaultKind::InjectInterrupt { vector } => match prng.next_u64() % 2 {
            0 => {
                let d = (prng.next_u64() % 65) as i64 - 32;
                (at.saturating_add_signed(d), kind)
            }
            _ => {
                // Nearby vector: flip one low bit.
                let b = (prng.next_u64() % 4) as u8;
                (
                    at,
                    FaultKind::InjectInterrupt {
                        vector: vector ^ (1 << b),
                    },
                )
            }
        },
    }
}

// ---------------------------------------------------------------------------
// The record-emitting toy machine (portable path).
// ---------------------------------------------------------------------------

/// A deterministic toy [`Machine`] for one benchmark bug that **emits a console**
/// reflecting how close a branch got to the trigger — so the log-template signal
/// has a species ladder toward the bug. Crashes iff [`trigger::fires`]; the
/// console words (not their numeric params, which the clusterer strips) form the
/// cells.
pub struct BenchToyMachine {
    spec: BugSpec,
    current: Reproducer,
    vtime: u64,
    snaps: std::collections::BTreeMap<u64, (u64, Reproducer)>,
    next_snap: u64,
    last_console: Vec<(u64, Vec<u8>)>,
}

impl BenchToyMachine {
    /// A fresh toy guest for `spec`, quiescent at [`BASE_VTIME`].
    pub fn new(spec: BugSpec) -> Self {
        Self {
            spec,
            current: mint_scenario_env(0, &spec_placeholder(), 0, 64),
            vtime: BASE_VTIME,
            snaps: std::collections::BTreeMap::new(),
            next_snap: 1,
            last_console: Vec::new(),
        }
    }

    /// The proximity phase words a scenario reaches — the species ladder. Closer
    /// to the trigger ⇒ a longer, deeper prefix of phases ⇒ more distinct
    /// template cells. Non-numeric words (the clusterer strips numbers), so each
    /// phase is its own template species.
    fn phases(&self, sc: &Scenario) -> Vec<&'static str> {
        let mut ph = vec!["supervisor boot", "supervisor warmup"];
        match self.spec.trigger {
            TriggerParams::FaultTiming { gpa, mask, window } => {
                let hit = sc.faults.iter().find_map(|p| match p.kind {
                    FaultKind::CorruptMemory { gpa: g, mask: m } => Some((g, m, p.at)),
                    _ => None,
                });
                if let Some((g, m, at)) = hit {
                    ph.push("ledger mapped");
                    if g == gpa {
                        ph.push("ledger address aligned");
                    }
                    if m == mask {
                        ph.push("guard bit aligned");
                    }
                    if window.0 <= at && at < window.1 {
                        ph.push("sensitive window entered");
                    }
                }
            }
            TriggerParams::OrderingInterrupt { vector, window } => {
                let hit = sc.faults.iter().find_map(|p| match p.kind {
                    FaultKind::InjectInterrupt { vector: v } => Some((v, p.at)),
                    _ => None,
                });
                if let Some((v, at)) = hit {
                    ph.push("handler entered");
                    if v == vector {
                        ph.push("vulnerable vector armed");
                    }
                    if window.0 <= at && at < window.1 {
                        ph.push("preempt window entered");
                    }
                }
            }
            TriggerParams::RareEntropy {
                prefix,
                prefix_bits,
            } => {
                let draw = trigger::entropy_draw(sc.seed);
                // How many leading bits match — a proximity ladder (bucketed).
                let matching = (0..prefix_bits)
                    .take_while(|&b| {
                        let sh = 63 - b;
                        (draw >> sh) & 1 == (prefix >> sh) & 1
                    })
                    .count() as u32;
                ph.push("uuid drawn");
                if matching >= prefix_bits / 2 {
                    ph.push("uuid prefix half match");
                }
                if matching >= prefix_bits {
                    ph.push("uuid prefix full match");
                }
            }
            // #[non_exhaustive] — an unknown class gets the base phases only.
            _ => {}
        }
        ph
    }
}

/// A throwaway spec only used to seed the toy's initial (pre-branch) env.
fn spec_placeholder() -> BugSpec {
    benchmark::manifest::Benchmark::wave5().bugs[0].clone()
}

impl Machine for BenchToyMachine {
    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
        let Some((vt, _)) = self.snaps.get(&snap.0) else {
            return Err(MachineError::UnknownSnapshot(snap.0));
        };
        AdapterEnv::decode(env)?;
        self.vtime = *vt;
        self.current = env.clone();
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        let Some((vt, env)) = self.snaps.get(&snap.0) else {
            return Err(MachineError::UnknownSnapshot(snap.0));
        };
        self.vtime = *vt;
        self.current = env.clone();
        Ok(())
    }

    fn run(
        &mut self,
        _until: &StopConditions,
        _resolve: Option<&explorer::Answer>,
    ) -> Result<StopReason, MachineError> {
        let sc = scenario_of(&self.current);
        let phases = self.phases(&sc);
        // Console: one line per phase reached (the species ladder) at ascending
        // moments. Deterministic in the scenario.
        self.last_console = phases
            .iter()
            .enumerate()
            .map(|(i, w)| (BASE_VTIME + i as u64, w.as_bytes().to_vec()))
            .collect();
        let terminal_vtime = self.vtime.saturating_add(64 + phases.len() as u64);
        self.vtime = terminal_vtime;
        if trigger::fires(&self.spec, &sc) {
            self.last_console
                .push((terminal_vtime, self.spec.serial_marker.as_bytes().to_vec()));
            Ok(StopReason::Crash {
                vtime: Moment(terminal_vtime),
                info: vec![self.spec.crash_kind as u8, self.spec.id.0 as u8],
            })
        } else {
            Ok(StopReason::Quiescent {
                vtime: Moment(terminal_vtime),
            })
        }
    }

    fn snapshot(&mut self) -> Result<(SnapId, EvidenceCut), MachineError> {
        let id = self.next_snap;
        self.next_snap += 1;
        self.snaps.insert(id, (self.vtime, self.current.clone()));
        // The toy stamps its cut from the same state its seal records (task
        // 127); it models no SDK capture, so the prefix is 0.
        Ok((
            SnapId(id),
            EvidenceCut {
                at: Moment(self.vtime),
                sdk_events: 0,
            },
        ))
    }

    fn drop_snap(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.snaps
            .remove(&snap.0)
            .map(|_| ())
            .ok_or(MachineError::UnknownSnapshot(snap.0))
    }

    fn hash(&mut self) -> Result<[u8; 32], MachineError> {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"campaign-runner.benchtoy.state_hash.v1");
        h.update((self.current.bytes.len() as u64).to_le_bytes());
        h.update(&self.current.bytes);
        Ok(h.finalize().into())
    }

    fn coverage(&self) -> &[u8] {
        &[]
    }

    fn recorded_env(&self) -> Result<Reproducer, MachineError> {
        Ok(self.current.clone())
    }

    fn console(&mut self) -> Result<Vec<(u64, Vec<u8>)>, MachineError> {
        Ok(self.last_console.clone())
    }
}

// ---------------------------------------------------------------------------
// The dual-config driver.
// ---------------------------------------------------------------------------

/// A novelty-frontier entry: the genesis-complete exemplar env plus its
/// ancestor-chain metadata (how long the chain is, and how many of its links were
/// novel-cell admissions), so a find that exploits this exemplar attributes the
/// whole chain's novelty to measure 2 — not just the finding branch's.
struct Exemplar {
    env: Reproducer,
    path_len: u64,
    novel_on_path: u64,
}

/// Fold a [`CellKey`](explorer::CellKey) (the encoded channel-value tuple) to the
/// opaque `u64` the [`CampaignLog`] carries — FNV-1a over the key bytes. Deterministic
/// and injective enough for the report's discovery-event stream (the report never
/// interprets a cell id; it only counts distinct ones and folds the STADS spectrum).
/// Because [`CellFnV1`]'s key is count-based (species-progress + last-new-species),
/// the same abstract slice folds to the same `u64` regardless of which seed produced
/// it — the cross-campaign comparability the report's pooled STADS wants.
fn cell_id_of(key: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in key {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Whether `hay` contains `needle` as a byte substring.
fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// The real task-67 signal under test (GO/NO-GO #2 ruling, user 2026-07-06): a
/// campaign-persistent [`LogSensor`] clusters the guest console into template
/// species, and [`CellFnV1`] keys the **accumulating** species slice into bounded
/// cells. One instance per `(config, seed)` campaign — the codebook accumulates
/// across that campaign's branches (ids stable within a seed) but is independent
/// across seeds, which is safe because `CellFnV1`'s default key is a function of
/// the distinct-species *count* (species-progress `log2_bucket(k)` + last-new-species
/// `max_id mod k`), not of which template got which id — so two seeds keying the
/// same abstract slice agree, and the seeds parallelize.
struct SignalCells {
    sensor: LogSensor,
    cellfn: CellFnV1,
}

impl SignalCells {
    /// A fresh campaign signal (empty codebook, default `CellFnV1` knobs).
    fn new() -> Self {
        Self {
            sensor: LogSensor::new(),
            cellfn: CellFnV1::new(),
        }
    }
}

/// Build the `RunTrace` for a finished branch, capturing the console into
/// `records` (the scrape channel the sensor reads).
fn trace_of<M: Machine>(
    machine: &mut M,
    stop: StopReason,
    env: Reproducer,
) -> Result<RunTrace, MachineError> {
    let records = machine
        .console()?
        .into_iter()
        .map(|(m, line)| {
            (
                Moment(m),
                Record {
                    stream: StreamId(0),
                    line,
                },
            )
        })
        .collect();
    Ok(RunTrace {
        terminal: stop,
        env,
        coverage: None,
        events: Vec::new(),
        records,
    })
}

/// The per-branch cell set — the **real** Phase-D `logtmpl` signal (`LogSensor` →
/// `CellFnV1`), the actual CellFn the selectors get built on (GO/NO-GO #2 ruling,
/// user 2026-07-06 — measure the real signal, never a stand-in). The campaign
/// [`LogSensor`] clusters this branch's console into template species (advancing
/// the codebook), and [`CellFnV1`] keys the **accumulating** species slice at each
/// arrival — the distinct bounded cells this branch's run passes through as its
/// log-template diversity grows.
///
/// The bug's terminal serial MARKER is filtered OUT of the console **before**
/// clustering (round-6 P1): the marker is *attribution*, not a behavioural cell,
/// and letting it mint a template species would make novelty correlate with bug
/// discovery **spuriously** (the signal keying its own attribution marker). The
/// full, unfiltered trace is still used by [`marker_attributed`] for attribution.
fn cells_of(spec: &BugSpec, signal: &SignalCells, trace: &RunTrace) -> Vec<u64> {
    let marker = spec.serial_marker.as_bytes();
    // Filter the attribution marker out of the console before it reaches the
    // clusterer (so it never becomes a template species).
    let filtered = RunTrace {
        terminal: trace.terminal.clone(),
        env: trace.env.clone(),
        coverage: trace.coverage.clone(),
        events: trace.events.clone(),
        records: trace
            .records
            .iter()
            .filter(|(_, r)| !contains(&r.line, marker))
            .cloned()
            .collect(),
    };
    // Advance the campaign codebook over this branch's lines, then key the
    // accumulating template slice at each species arrival — the cells the run
    // visits as its distinct-template count grows. A recurring line re-keys to
    // the same cell (already in the set); the STADS abundance stream keeps every
    // arrival, so a report can fold recurrence.
    let mut acc = FeatureSet::new();
    let mut touched = Vec::new();
    for (at, feat) in signal.sensor.observe(&filtered) {
        acc.insert(feat);
        touched.push(cell_id_of(&signal.cellfn.key(at, &acc)));
    }
    touched
}

/// A certified find's **determinism certificate**: the reproducer env and the
/// finding run's `state_hash` (which every one of the N certifying replays
/// matched). The box operator uses `state_hash` for the solo-vs-co-tenant
/// determinism stress-test (it MUST be identical whether the campaign ran alone or
/// alongside co-tenant VMs on other cores) and `env` to re-derive the reproducer.
#[derive(Clone, Debug)]
pub struct FindCert {
    /// The bug found.
    pub bug: BugId,
    /// The branch it fired at (time-to-bug).
    pub branch: u64,
    /// The genesis-replayable reproducer env.
    pub env: Reproducer,
    /// The finding run's canonical 32-byte `state_hash`.
    pub state_hash: [u8; 32],
}

/// The full outcome of one benchmark campaign: the discovery-event log the report
/// consumes, plus a determinism certificate per certified find.
#[derive(Clone, Debug)]
pub struct BenchOutcome {
    /// The discovery-event log (`report::CampaignLog`).
    pub log: CampaignLog,
    /// One certificate per certified find.
    pub certs: Vec<FindCert>,
    /// Every non-skipped branch's `(branch_index, RunTrace)`, in branch order —
    /// the **retained re-key substrate** (M2 amendment / `docs/SCORING.md` R1/R2):
    /// the offline evaluation set a future `CellFn` candidate replays through its
    /// pure fold to re-key the archive without re-running the campaign. Each branch
    /// runs from the sealed base, so its trace is genesis-rooted (no cross-fork gap).
    /// The box path serializes this to `--record`'s JSON when set; the toy path
    /// collects it (cheap) and the caller ignores it. Skipped (inadmissible) branches
    /// have no trace and are absent (they contribute no cells — see the campaign log).
    pub traces: Vec<(u64, RunTrace)>,
}

/// Seal the campaign base and return `(snapshot, base_vtime)`. On the **toy** path
/// (`deadline_delta == None`) the guest is quiescent at boot, so it snapshots
/// first-try and needs no V-time probe (which would *advance* the toy — its `run`
/// ignores the deadline). On the **box** path the guest is mid-workload at its
/// readiness marker and may not be on a snapshottable boundary, so retry past
/// `NotQuiescent` by advancing a fine `snapshot_retry_step` each time until it
/// seals — task 41 / task 60's discipline — giving up loudly after
/// `snapshot_max_attempts`. `base_vtime` is the effective V-time at the seal (the
/// deadline anchor).
fn seal_base<M: Machine>(
    machine: &mut M,
    cfg: &BenchConfig,
) -> Result<(SnapId, u64), MachineError> {
    if cfg.deadline_delta.is_none() {
        let (snap, _cut) = machine.snapshot()?;
        return Ok((snap, 0));
    }
    let mut vt = crate::probe_vtime(machine)?;
    let mut attempts = 0usize;
    let base = loop {
        attempts += 1;
        match machine.snapshot() {
            Ok((snap, _cut)) => break snap,
            Err(MachineError::NotQuiescent) => {
                if attempts >= cfg.snapshot_max_attempts {
                    return Err(MachineError::NotQuiescent);
                }
                let stop = machine.run(
                    &StopConditions {
                        deadline: Some(Moment(vt.saturating_add(cfg.snapshot_retry_step))),
                        on: StopMask::NONE,
                    },
                    None,
                )?;
                // The nudge must land on the deadline (a snapshottable boundary
                // candidate); any other stop before the base is sealed is a loud
                // failure, never a silent seal at the wrong point.
                if !matches!(stop, StopReason::Deadline { .. }) {
                    return Err(MachineError::NotQuiescent);
                }
                vt = stop.vtime().0;
            }
            Err(e) => return Err(e),
        }
    };
    Ok((base, vt))
}

/// Drive one benchmark campaign against `machine` under `config` and return its
/// discovery-event log plus per-find determinism certificates. Seals a base, then
/// per branch: pick a base env (signal exploits novel exemplars; baseline explores
/// from genesis), run, capture the console → cells, admit novel exemplars, and
/// judge — recording the first find per bug (its time-to-bug).
pub fn run_bench_campaign<M: Machine>(
    machine: &mut M,
    spec: &BugSpec,
    cfg: &BenchConfig,
    config: Configuration,
) -> Result<BenchOutcome, MachineError> {
    // The real task-67 signal for THIS campaign: a fresh LogSensor+CellFnV1 whose
    // codebook accumulates across this (config, seed)'s branches but is independent
    // of every other seed's — safe because the cell key is count-based, so the
    // seeds parallelize and still pool (see [`cells_of`] / [`SignalCells`]).
    let signal = SignalCells::new();

    // Seal the base + learn its V-time (the deadline anchor). The box guest is
    // mid-workload at its readiness marker, not necessarily on a snapshottable
    // boundary, so it may need retries past `NotQuiescent`; the toy is quiescent at
    // boot and seals first-try.
    let (base, base_vtime) = seal_base(machine, cfg)?;
    let until = StopConditions {
        deadline: cfg
            .deadline_delta
            .map(|d| Moment(base_vtime.saturating_add(d))),
        on: StopMask::NONE,
    };

    let mut prng = Prng::new(cfg.campaign_seed);
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    // The novelty frontier carries per-exemplar ancestor-chain metadata so a
    // find that exploits a novel parent counts that parent's novelty (measure 2).
    let mut frontier: Vec<Exemplar> = Vec::new();
    let mut events = Vec::new();
    // Retained re-key substrate: every non-skipped branch's (index, trace), in
    // order (SCORING R1/R2). Write-only — collected, never read mid-campaign, so a
    // golden run is bit-identical (task-65 sink invariant).
    let mut traces: Vec<(u64, RunTrace)> = Vec::new();
    let mut finds = Vec::new();
    let mut certs = Vec::new();
    let mut found = false;
    let mut step = 0u64;

    for branch in 0..cfg.max_branches {
        step += 1;
        // Pick the branch env, carrying the selected parent's path metadata so
        // the ancestor chain's novelty is attributed (not just this branch's).
        let exploit = matches!(config, Configuration::Signal)
            && !frontier.is_empty()
            && !step.is_multiple_of(cfg.explore_period);
        let (env, parent_path_len, parent_novel) = if exploit {
            let pick = (prng.next_u64() % frontier.len() as u64) as usize;
            let parent = &frontier[pick];
            let e = exploit_env(&parent.env, &mut prng);
            (e, parent.path_len, parent.novel_on_path)
        } else {
            (
                mint_scenario_env(prng.next_u64(), spec, cfg.fault_rebase, cfg.order_range),
                0,
                0,
            )
        };

        // Stage + run the branch. A backend that REJECTS this env's fault as an
        // inadmissible PROPOSAL (an out-of-range gpa, a Moment behind the restore
        // point / already taken, or an out-of-scope fault it won't service) is NOT
        // a machine failure — it is a discarded mutant, exactly as a real explorer
        // discards a rejected proposal. Skip the branch (no cells, never a find)
        // and keep the campaign running; only a genuine transport/backend death
        // (any OTHER `MachineError`) still aborts via `?`, so real failures and
        // determinism divergences are never masked (task-69 M2 fix A). Fix B's
        // small local exploit kernel keeps the parent's fault in-range, so a skip
        // here is now the rare boundary escapee rather than every insert-mutation.
        let staged = machine
            .branch(base, &env)
            .and_then(|()| machine.run(&until, None));
        let stop = match staged {
            Ok(stop) => stop,
            Err(MachineError::Inadmissible(why)) => {
                if std::env::var_os("BENCH_DIAG").is_some() {
                    eprintln!(
                        "[bench-diag] branch {branch} {config:?} seed={} SKIP inadmissible proposal: {why}",
                        cfg.campaign_seed
                    );
                }
                // A skipped branch still consumes budget (a wasted proposal costs a
                // branch, so measure-1's equal-budget comparison stays honest) but
                // contributes no cells and can never be a find.
                events.push(BranchEvent {
                    branch,
                    touched: Vec::new(),
                });
                continue;
            }
            Err(e) => return Err(e),
        };
        // The finding run's state_hash, captured before any replay disturbs the
        // machine — a certified find must replay N/N identical to THIS.
        let run_hash = machine.hash()?;
        let trace = trace_of(machine, stop, env.clone())?;
        let touched = cells_of(spec, &signal, &trace);

        // Opt-in per-branch diagnostics (`BENCH_DIAG=1`): print each branch's
        // injected schedule, terminal, marker/judge attribution, and cell count.
        // Pure observation (stderr only) — never touches campaign state or a
        // hash — so a golden run (env unset) is bit-identical. The box operator
        // uses it to calibrate a bug's trigger (does the fault fire? does the
        // marker attribute?) and to watch a long campaign's progress.
        if std::env::var_os("BENCH_DIAG").is_some() {
            let sc = scenario_of(&env);
            let faults: Vec<String> = sc
                .faults
                .iter()
                .map(|p| match p.kind {
                    FaultKind::CorruptMemory { gpa, mask } => {
                        format!(
                            "Corrupt@{} gpa={gpa:#x} bit={}",
                            p.at,
                            mask.trailing_zeros()
                        )
                    }
                    FaultKind::InjectInterrupt { vector } => {
                        format!("Interrupt@{} vec={vector:#x}", p.at)
                    }
                })
                .collect();
            let marker = marker_attributed(&trace, spec);
            let is_bug = trace.terminal.is_bug();
            let n_records = trace.records.len();
            eprintln!(
                "[bench-diag] branch {branch} {config:?} seed={} faults=[{}] stop={:?} marker={marker} is_bug={is_bug} cells={} records={n_records}",
                cfg.campaign_seed,
                faults.join(", "),
                trace.terminal,
                touched.len(),
            );
        }

        // Admit an exemplar iff it claimed a fresh cell (novelty archive).
        let mut novel = false;
        for &c in &touched {
            if seen.insert(c) {
                novel = true;
            }
        }
        // Ancestor-chain metadata: this branch extends its parent's chain (or
        // starts a fresh one on an explore step).
        let path_len = parent_path_len + 1;
        let novel_on_path = parent_novel + u64::from(novel);
        if novel {
            frontier.push(Exemplar {
                env: trace.env.clone(),
                path_len,
                novel_on_path,
            });
        }
        events.push(BranchEvent { branch, touched });

        // Record the FIRST **certified** find (its time-to-bug + full
        // ancestor-chain trajectory) but do NOT break — keep running/logging to
        // the measurement budget so measure 1 (discovery at equal budget) is
        // comparable, not driven by early termination (round-2 P1).
        //
        // A find must be a REAL find (round-3 gate-integrity P1): an incidental /
        // flaky / unrelated crash must NOT count. Certify it two ways before
        // logging: (a) the bug's distinctive serial MARKER appears in the run's
        // console (per-bug attribution — only the planted bug prints it), and
        // (b) the emitted reproducer replays the IDENTICAL `(stop, state_hash)`
        // **and** marker `cfg.replay_n` (25/25) times. This is **terminal-
        // agnostic** (M2, 2026-07-07): the marker is the bug signal, so a find is
        // certified whether the run reached the real reboot->`Crash` (a large
        // deadline — gate-2 benchmark validity) or was cut off at a `Deadline`
        // right after the marker (a small deadline — the fast correlation runs).
        // On this kernel the isa-debug-exit crash channels all fail, so the
        // `Crash{Shutdown}` terminal is ~4.8M V-time of `reboot -f`; requiring
        // it per find would make the ≥20-seed suite take weeks. The marker (at
        // ~seal+500) + 25/25 determinism is the rigorous, feasible certification;
        // gate-2 validity (a real `Crash`) is proven separately per bug with one
        // large-deadline run.
        if !found && marker_attributed(&trace, spec) {
            let certified = certify_replays(
                machine,
                base,
                &env,
                spec.serial_marker.as_bytes(),
                &until,
                &trace.terminal,
                run_hash,
                cfg.replay_n,
            )?;
            if certified {
                finds.push(FindRecord {
                    bug: spec.id,
                    branch,
                    path_len,
                    novel_on_path,
                });
                certs.push(FindCert {
                    bug: spec.id,
                    branch,
                    env: env.clone(),
                    state_hash: run_hash,
                });
                found = true;
            }
        }

        // Retain this branch's trace for offline re-key (SCORING R1/R2). At the loop
        // tail, after every read of `trace`, so it moves (no clone) and stays a
        // write-only sink.
        traces.push((branch, trace));
    }

    machine.drop_snap(base)?;
    Ok(BenchOutcome {
        log: CampaignLog {
            bug: spec.id,
            config,
            seed: cfg.campaign_seed,
            events,
            finds,
            // Self-describing: the effective search knobs this run used (PR#90
            // round-2), so a same-seed artifact never hides an ambient-env choice.
            explore_period: cfg.explore_period,
            order_range: cfg.order_range,
        },
        certs,
        traces,
    })
}

/// Whether the run's console carries the bug's distinctive serial marker — the
/// per-bug crash attribution (so an unrelated crash is not mis-credited to this
/// bug). Scans the scrape records for the marker as a byte substring.
fn marker_attributed(trace: &RunTrace, spec: &BugSpec) -> bool {
    let marker = spec.serial_marker.as_bytes();
    if marker.is_empty() {
        return false;
    }
    trace
        .records
        .iter()
        .any(|(_, r)| r.line.windows(marker.len()).any(|w| w == marker))
}

/// Certify a candidate find by **N/N replay** — the terminal-agnostic,
/// marker-based certification (M2, 2026-07-07). Replay the reproducer `n` times
/// **under the same stop conditions `until` as the finding run** and require
/// every replay to reproduce (a) the finding run's exact `(stop, state_hash)`
/// (box gate 2 — N/N identical to the FINDING, not merely to each other — the
/// round-4 determinism gate) and (b) the bug's per-bug serial `marker` in its
/// console (round-3 attribution — only the planted bug prints it).
///
/// It is decoupled from **which** terminal the run reaches: at a large deadline
/// the finding stops at the real reboot->`Crash{Shutdown}` (gate-2 benchmark
/// validity), at a small deadline it stops at a `Deadline` right after the
/// marker (the fast ≥20-seed correlation runs). Both are rigorous — the marker
/// proves the planted bug fired and `(stop, state_hash)` identity proves
/// bit-for-bit determinism. The replays use `until` (not a natural terminal) so
/// they reproduce the finding's exact stop; a natural-terminal replay would run
/// the ~4.8M-V-time reboot and diverge from a small-deadline `Deadline` finding.
/// A flaky/non-deterministic run, or one whose marker does not reproduce, fails
/// — never logged as a find. `n == 0` or an empty marker never certifies.
// The certificate genuinely needs all of (base, env, marker, until, found_stop,
// found_hash, n) to pin a replay to the FINDING run; bundling them into a struct
// would only move the arity, not reduce the coupling.
#[allow(clippy::too_many_arguments)]
fn certify_replays<M: Machine>(
    machine: &mut M,
    base: SnapId,
    env: &Reproducer,
    marker: &[u8],
    until: &StopConditions,
    found_stop: &StopReason,
    found_hash: [u8; 32],
    n: usize,
) -> Result<bool, MachineError> {
    if n == 0 || marker.is_empty() {
        return Ok(false);
    }
    for _ in 0..n {
        machine.branch(base, env)?;
        let stop = machine.run(until, None)?;
        let hash = machine.hash()?;
        let has_marker = machine
            .console()?
            .iter()
            .any(|(_, line)| contains(line, marker));
        // Every replay must reproduce the FINDING run's exact (stop, state_hash)
        // AND carry the marker — a divergent hash, a different stop, or a missing
        // marker fails certification (so a flaky find is never logged).
        if stop != *found_stop || hash != found_hash || !has_marker {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use benchmark::manifest::{Benchmark, BugId};

    /// A [`BenchConfig::smoke`] whose branch budget is cut under **Miri** (task
    /// 104, `hm-d4y`). The smoke budget is 2048 branches and the interpreter
    /// spends ~0.4 s of real time on each, so every full smoke campaign is ~14
    /// min of the Miri run.
    ///
    /// A cut is only sound where the test's assertion does not depend on the
    /// budget. It is **not** free for the tests that assert a *find*: the manifest
    /// bugs are calibrated to a ~1/256 fire rate over a 10²–10³ branch search, and
    /// the seeded searches here first hit bug 1 at branch **202** (Baseline) /
    /// **255** (Signal) and bug 3 at branch **19** / **33**. So `budget` is passed
    /// per test, sized against the find index that test actually needs, and the one
    /// test whose budget cannot be cut far enough to pay for itself is Miri-ignored
    /// rather than quietly weakened. If a future change to the mint or the PRNG
    /// moves those find indices past the budgets below, the Miri run fails loudly
    /// ("should find bug N") — raise the budget here, do not delete the assertion.
    /// Native runs always get the full 2048 and are byte-for-byte unchanged.
    fn smoke_cfg(campaign_seed: u64, miri_budget: u64) -> BenchConfig {
        let base = BenchConfig::smoke(campaign_seed);
        BenchConfig {
            max_branches: if cfg!(miri) {
                miri_budget
            } else {
                base.max_branches
            },
            ..base
        }
    }

    /// Each bug's toy machine crashes on its trigger and halts nominally, and the
    /// crash carries the per-bug id (attribution).
    #[test]
    fn each_toy_crashes_on_trigger_halts_nominal() {
        for spec in Benchmark::wave5().bugs {
            let mut m = BenchToyMachine::new(spec.clone());
            let base = m.snapshot().unwrap().0;
            let until = StopConditions {
                deadline: None,
                on: StopMask::NONE,
            };
            // The ground-truth triggering scenario, minted as an env.
            let hit = trigger::triggering_scenario(&spec);
            let env = env_of_scenario(&hit, &spec);
            m.branch(base, &env).unwrap();
            match m.run(&until, None).unwrap() {
                StopReason::Crash { info, .. } => assert_eq!(info[1], spec.id.0 as u8),
                other => panic!("{} expected crash, got {other:?}", spec.name),
            }
        }
    }

    /// The box-frame rebase (M2 prereq 2): minting a fault-carrying bug with
    /// `rebase = BASE_VTIME` keys the fault at a **bare offset** (well under
    /// `BASE_VTIME`), not the absolute manifest `Moment` (~`BASE_VTIME + offset`) —
    /// so the `SocketMachine` adapter's `+ seal` re-anchoring lands it in the
    /// guest's real vulnerable window instead of `seal + BASE + offset` (past it).
    /// The absolute (toy) frame keeps the manifest `Moment`, and the fault kind is
    /// frame-independent.
    #[test]
    fn box_frame_keys_faults_at_bare_offsets_not_absolute() {
        let bench = Benchmark::wave5();
        for id in [BugId(1), BugId(2)] {
            // The two fault-carrying classes; bug 3 (rare-entropy) mints no fault.
            let spec = bench.get(id).unwrap().clone();
            for seed in 0..64u64 {
                let abs = scenario_of(&mint_scenario_env(seed, &spec, 0, 64));
                let boxed = scenario_of(&mint_scenario_env(seed, &spec, BASE_VTIME, 64));
                assert_eq!(abs.faults.len(), 1, "{} mints one fault", spec.name);
                assert_eq!(boxed.faults.len(), 1);
                assert_eq!(
                    boxed.faults[0].kind, abs.faults[0].kind,
                    "same fault, different frame"
                );
                assert!(
                    abs.faults[0].at >= BASE_VTIME.saturating_sub(4),
                    "absolute frame keys near the manifest window (~BASE+offset)"
                );
                assert!(
                    boxed.faults[0].at < BASE_VTIME,
                    "box frame keys a bare offset (the adapter re-adds the seal V-time)"
                );
            }
        }
    }

    /// Build an env directly from a benchmark Scenario (test helper).
    fn env_of_scenario(sc: &Scenario, _spec: &BugSpec) -> Reproducer {
        let mut es = EnvSpec::Seeded {
            seed: sc.seed,
            policy: FaultPolicy::none(),
        };
        for p in &sc.faults {
            let hf = match p.kind {
                FaultKind::CorruptMemory { gpa, mask } => HostFault::CorruptMemory {
                    gpa,
                    mask: BitMask(mask),
                },
                FaultKind::InjectInterrupt { vector } => HostFault::InjectInterrupt {
                    vector: vector.into(),
                },
            };
            es.perturb(hf, p.at);
        }
        AdapterEnv {
            base_offset: 0,
            pos: 0,
            spec: es,
        }
        .encode()
    }

    /// The dual-config driver runs and is **deterministic-twice** (the box smoke
    /// property): the same (seed, config) yields the identical discovery-event
    /// log — for both configurations, for a bug it reliably finds.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "the extreme tail (task 104, hm-d4y): FOUR full 2048-branch smoke campaigns \
                  (~27 min interpreted, 80% of the pre-shrink Miri run). Pure safe code over \
                  BenchToyMachine — Vmm is never composed, so `CountingBackend::map_memory` (the \
                  crate's only unsafe) is not on the path; it stays Miri-run in \
                  mock::tests::mock_vmm_composes_maps_memory_and_ticks_per_exit. It is the one \
                  campaign test whose budget cannot be cut: it needs a find in BOTH \
                  configurations (bug 1 fires at ~1/256; first find at branch 202/255), so any \
                  affordable budget would silently turn its `should find bug 1` assertion \
                  vacuous. Its two claims stay Miri-covered at the shrunk budgets: the campaign \
                  driver's find/cert path by rare_entropy_bug_is_searchable (the same \
                  run_bench_campaign over both configurations) and events_run_to_budget_after_find \
                  (a real bug-1 find), and campaign determinism-twice by \
                  gamecampaign::tests::campaign_replays_bit_identically."
    )]
    fn dual_config_runs_and_is_deterministic_twice() {
        let bench = Benchmark::wave5();
        let bug = bench.get(BugId(1)).unwrap().clone();
        for config in [Configuration::Signal, Configuration::Baseline] {
            let cfg = BenchConfig::smoke(0xBEEF_0069);
            let mut m1 = BenchToyMachine::new(bug.clone());
            let log1 = run_bench_campaign(&mut m1, &bug, &cfg, config).unwrap().log;
            let mut m2 = BenchToyMachine::new(bug.clone());
            let log2 = run_bench_campaign(&mut m2, &bug, &cfg, config).unwrap().log;
            assert_eq!(log1, log2, "{config:?} must be deterministic-twice");
            assert!(!log1.finds.is_empty(), "{config:?} should find bug 1");
        }
    }

    /// The rare-entropy bug (bug 3, no host faults — fires on the seed alone) IS
    /// searchable: distinct branch seeds produce distinct entropy draws, so a
    /// campaign finds it. This is the portable analog of the guest's post-snapshot
    /// RDRAND draw varying per branch (the round-2 seed-source fix).
    #[test]
    fn rare_entropy_bug_is_searchable() {
        let bench = Benchmark::wave5();
        let bug = bench.get(BugId(3)).unwrap().clone();
        // Miri: 64 branches — bug 3 is first found at branch 19 (Signal) / 33
        // (Baseline), so the seed search still finds it with ~2x headroom.
        let cfg = smoke_cfg(0x0033_1D69, 64);
        for config in [Configuration::Signal, Configuration::Baseline] {
            let mut m = BenchToyMachine::new(bug.clone());
            let log = run_bench_campaign(&mut m, &bug, &cfg, config).unwrap().log;
            assert!(
                !log.finds.is_empty(),
                "{config:?} must find the rare-entropy bug by seed search"
            );
        }
    }

    /// The events stream runs to the measurement budget even after a find (round-2
    /// P1): the first find is recorded but the campaign keeps logging, so measure
    /// 1 (discovery at equal budget) is not truncated by early termination.
    #[test]
    fn events_run_to_budget_after_find() {
        let bench = Benchmark::wave5();
        let bug = bench.get(BugId(1)).unwrap().clone();
        // Miri: 384 branches — this Baseline search first finds bug 1 at branch
        // 202, so the find (and the "events keep running past it" claim the test
        // exists for) survives with ~1.9x headroom. This is the bug-1 find path's
        // Miri-run representative now that dual_config… is gated.
        let cfg = smoke_cfg(0xBEEF_0069, 384);
        let mut m = BenchToyMachine::new(bug.clone());
        let log = run_bench_campaign(&mut m, &bug, &cfg, Configuration::Baseline)
            .unwrap()
            .log;
        let find = log.finds.first().expect("bug 1 found");
        assert_eq!(
            log.events.len() as u64,
            cfg.max_branches,
            "events must run to the full budget, not stop at the find"
        );
        assert!(
            log.events.len() as u64 > find.branch + 1,
            "logging continues past the find branch"
        );
    }

    /// An incidental crash that carries **no serial marker** is NOT logged as a
    /// find — the marker-attribution gate rejects it (round-3 gate integrity).
    #[test]
    fn unmarked_crash_is_not_a_find() {
        // A machine that always crashes but emits an EMPTY console (no marker).
        struct SilentCrashMachine {
            current: Reproducer,
            snaps: std::collections::BTreeMap<u64, Reproducer>,
            next: u64,
        }
        impl Machine for SilentCrashMachine {
            fn branch(&mut self, s: SnapId, e: &Reproducer) -> Result<(), MachineError> {
                if !self.snaps.contains_key(&s.0) {
                    return Err(MachineError::UnknownSnapshot(s.0));
                }
                self.current = e.clone();
                Ok(())
            }
            fn replay(&mut self, s: SnapId) -> Result<(), MachineError> {
                self.current = self.snaps[&s.0].clone();
                Ok(())
            }
            fn run(
                &mut self,
                _u: &StopConditions,
                _r: Option<&explorer::Answer>,
            ) -> Result<StopReason, MachineError> {
                // Always a crash — but no console line is emitted, so no marker.
                Ok(StopReason::Crash {
                    vtime: Moment(BASE_VTIME + 1),
                    info: vec![0, 0],
                })
            }
            fn snapshot(&mut self) -> Result<(SnapId, EvidenceCut), MachineError> {
                let id = self.next;
                self.next += 1;
                self.snaps.insert(id, self.current.clone());
                Ok((SnapId(id), EvidenceCut::default()))
            }
            fn drop_snap(&mut self, s: SnapId) -> Result<(), MachineError> {
                self.snaps.remove(&s.0);
                Ok(())
            }
            fn hash(&mut self) -> Result<[u8; 32], MachineError> {
                Ok([7u8; 32])
            }
            fn coverage(&self) -> &[u8] {
                &[]
            }
            fn recorded_env(&self) -> Result<Reproducer, MachineError> {
                Ok(self.current.clone())
            }
            fn console(&mut self) -> Result<Vec<(u64, Vec<u8>)>, MachineError> {
                Ok(Vec::new()) // no marker ever
            }
        }
        let bench = Benchmark::wave5();
        let bug = bench.get(BugId(1)).unwrap().clone();
        let mut m = SilentCrashMachine {
            current: mint_scenario_env(0, &bug, 0, 64),
            snaps: std::collections::BTreeMap::new(),
            next: 1,
        };
        let cfg = BenchConfig {
            max_branches: 8,
            ..BenchConfig::smoke(1)
        };
        let log = run_bench_campaign(&mut m, &bug, &cfg, Configuration::Baseline)
            .unwrap()
            .log;
        assert!(
            log.finds.is_empty(),
            "an unmarked crash must not be certified as a find"
        );
    }

    /// Fix A (task-69 M2): an **inadmissible fault PROPOSAL** — the backend
    /// staged the env cleanly but refuses to apply the fault (out-of-range gpa,
    /// Moment behind/at-taken, out-of-scope fault) — is a *discarded mutant*, not
    /// a machine failure: the campaign SKIPS that branch (records it with no
    /// cells, never a find) and runs to completion. A genuine `Transport` death
    /// is the opposite — it still aborts loudly, so a real backend failure or a
    /// determinism divergence is NEVER masked by the skip.
    #[test]
    fn inadmissible_proposal_is_skipped_but_transport_still_aborts() {
        /// Seals fine (snapshot works), but every `branch` returns `err`.
        struct RejectBranchMachine {
            current: Reproducer,
            snaps: std::collections::BTreeMap<u64, Reproducer>,
            next: u64,
            err: MachineError,
        }
        impl Machine for RejectBranchMachine {
            fn branch(&mut self, _s: SnapId, _e: &Reproducer) -> Result<(), MachineError> {
                Err(self.err.clone())
            }
            fn replay(&mut self, s: SnapId) -> Result<(), MachineError> {
                self.current = self.snaps[&s.0].clone();
                Ok(())
            }
            fn run(
                &mut self,
                _u: &StopConditions,
                _r: Option<&explorer::Answer>,
            ) -> Result<StopReason, MachineError> {
                // Never reached — branch fails first.
                Ok(StopReason::Quiescent {
                    vtime: Moment(BASE_VTIME + 1),
                })
            }
            fn snapshot(&mut self) -> Result<(SnapId, EvidenceCut), MachineError> {
                let id = self.next;
                self.next += 1;
                self.snaps.insert(id, self.current.clone());
                Ok((SnapId(id), EvidenceCut::default()))
            }
            fn drop_snap(&mut self, s: SnapId) -> Result<(), MachineError> {
                self.snaps.remove(&s.0);
                Ok(())
            }
            fn hash(&mut self) -> Result<[u8; 32], MachineError> {
                Ok([0u8; 32])
            }
            fn coverage(&self) -> &[u8] {
                &[]
            }
            fn recorded_env(&self) -> Result<Reproducer, MachineError> {
                Ok(self.current.clone())
            }
        }
        let bench = Benchmark::wave5();
        let bug = bench.get(BugId(1)).unwrap().clone();
        let cfg = BenchConfig {
            max_branches: 8,
            ..BenchConfig::smoke(1)
        };
        let mk = |err| RejectBranchMachine {
            current: mint_scenario_env(0, &bug, 0, 64),
            snaps: std::collections::BTreeMap::new(),
            next: 1,
            err,
        };

        // Inadmissible on every branch: run to completion, every branch skipped.
        let mut skip = mk(MachineError::Inadmissible("gpa out of range (test)".into()));
        let log = run_bench_campaign(&mut skip, &bug, &cfg, Configuration::Baseline)
            .expect("an inadmissible proposal must never abort the campaign")
            .log;
        assert!(log.finds.is_empty(), "a skipped branch can never be a find");
        assert_eq!(
            log.events.len(),
            cfg.max_branches as usize,
            "a skipped branch still consumes budget (recorded, so measure-1 stays honest)"
        );
        assert!(
            log.events.iter().all(|e| e.touched.is_empty()),
            "a skipped branch contributes no cells"
        );

        // A real transport death still aborts — never swallowed as a skip.
        let mut die = mk(MachineError::Transport("socket torn (test)".into()));
        assert!(
            matches!(
                run_bench_campaign(&mut die, &bug, &cfg, Configuration::Baseline),
                Err(MachineError::Transport(_))
            ),
            "a genuine backend failure must abort, not be masked as an inadmissible skip"
        );
    }

    /// A firing branch cut off at a `Deadline` AFTER the marker printed (the
    /// small-deadline correlation path — the real reboot->`Crash` is ~4.8M V-time
    /// away and never reached) IS a certified find: the terminal-agnostic
    /// marker-based certification (M2, 2026-07-07). The marker proves the planted
    /// bug fired and the 25/25 identical `(Deadline, hash, marker)` proves
    /// determinism — the crash terminal is not required. Contrast
    /// `unmarked_crash_is_not_a_find` (a Crash with NO marker is NOT a find).
    #[test]
    fn marker_bearing_deadline_stop_is_a_find() {
        struct DeadlineMarkerMachine {
            marker: Vec<u8>,
            current: Reproducer,
            snaps: std::collections::BTreeMap<u64, Reproducer>,
            next: u64,
        }
        impl Machine for DeadlineMarkerMachine {
            fn branch(&mut self, s: SnapId, e: &Reproducer) -> Result<(), MachineError> {
                if !self.snaps.contains_key(&s.0) {
                    return Err(MachineError::UnknownSnapshot(s.0));
                }
                self.current = e.clone();
                Ok(())
            }
            fn replay(&mut self, s: SnapId) -> Result<(), MachineError> {
                self.current = self.snaps[&s.0].clone();
                Ok(())
            }
            fn run(
                &mut self,
                _u: &StopConditions,
                _r: Option<&explorer::Answer>,
            ) -> Result<StopReason, MachineError> {
                // The bug fired (the marker is on the console below) but the run
                // is cut off at the deadline before the slow reboot->Crash — a
                // Deadline stop, NOT a bug terminal.
                Ok(StopReason::Deadline {
                    vtime: Moment(BASE_VTIME + 100),
                })
            }
            fn snapshot(&mut self) -> Result<(SnapId, EvidenceCut), MachineError> {
                let id = self.next;
                self.next += 1;
                self.snaps.insert(id, self.current.clone());
                Ok((SnapId(id), EvidenceCut::default()))
            }
            fn drop_snap(&mut self, s: SnapId) -> Result<(), MachineError> {
                self.snaps.remove(&s.0);
                Ok(())
            }
            fn hash(&mut self) -> Result<[u8; 32], MachineError> {
                Ok([9u8; 32])
            }
            fn coverage(&self) -> &[u8] {
                &[]
            }
            fn recorded_env(&self) -> Result<Reproducer, MachineError> {
                Ok(self.current.clone())
            }
            fn console(&mut self) -> Result<Vec<(u64, Vec<u8>)>, MachineError> {
                Ok(vec![(BASE_VTIME, self.marker.clone())])
            }
        }
        let bench = Benchmark::wave5();
        let bug = bench.get(BugId(1)).unwrap().clone();
        let mut m = DeadlineMarkerMachine {
            marker: bug.serial_marker.as_bytes().to_vec(),
            current: mint_scenario_env(0, &bug, 0, 64),
            snaps: std::collections::BTreeMap::new(),
            next: 1,
        };
        let cfg = BenchConfig {
            max_branches: 1,
            ..BenchConfig::smoke(1)
        };
        let out = run_bench_campaign(&mut m, &bug, &cfg, Configuration::Baseline).unwrap();
        assert_eq!(
            out.log.finds.len(),
            1,
            "a marker-bearing Deadline stop must certify as a find (terminal-agnostic)"
        );
        assert_eq!(
            out.certs.len(),
            1,
            "the find emits a determinism certificate"
        );
    }

    /// A crash whose replays agree with EACH OTHER but differ from the FINDING
    /// run's state_hash is NOT certified (box gate 2 = N/N identical to the
    /// finding, round-4 P1). The machine emits the marker (so attribution passes)
    /// and crashes every run, but its state_hash is one value on the first run and
    /// a different — self-consistent — value on every later run.
    #[test]
    fn replays_must_match_the_finding_hash() {
        struct DriftingHashMachine {
            marker: Vec<u8>,
            runs: u64,
            current: Reproducer,
            snaps: std::collections::BTreeMap<u64, Reproducer>,
            next: u64,
        }
        impl Machine for DriftingHashMachine {
            fn branch(&mut self, s: SnapId, e: &Reproducer) -> Result<(), MachineError> {
                if !self.snaps.contains_key(&s.0) {
                    return Err(MachineError::UnknownSnapshot(s.0));
                }
                self.current = e.clone();
                Ok(())
            }
            fn replay(&mut self, s: SnapId) -> Result<(), MachineError> {
                self.current = self.snaps[&s.0].clone();
                Ok(())
            }
            fn run(
                &mut self,
                _u: &StopConditions,
                _r: Option<&explorer::Answer>,
            ) -> Result<StopReason, MachineError> {
                self.runs += 1;
                Ok(StopReason::Crash {
                    vtime: Moment(BASE_VTIME + 1),
                    info: vec![0, 1],
                })
            }
            fn snapshot(&mut self) -> Result<(SnapId, EvidenceCut), MachineError> {
                let id = self.next;
                self.next += 1;
                self.snaps.insert(id, self.current.clone());
                Ok((SnapId(id), EvidenceCut::default()))
            }
            fn drop_snap(&mut self, s: SnapId) -> Result<(), MachineError> {
                self.snaps.remove(&s.0);
                Ok(())
            }
            fn hash(&mut self) -> Result<[u8; 32], MachineError> {
                // First run → one hash; every later (replay) run → a DIFFERENT but
                // self-consistent hash. Replays agree with each other, not the find.
                Ok(if self.runs <= 1 { [1u8; 32] } else { [2u8; 32] })
            }
            fn coverage(&self) -> &[u8] {
                &[]
            }
            fn recorded_env(&self) -> Result<Reproducer, MachineError> {
                Ok(self.current.clone())
            }
            fn console(&mut self) -> Result<Vec<(u64, Vec<u8>)>, MachineError> {
                Ok(vec![(BASE_VTIME, self.marker.clone())])
            }
        }
        let bench = Benchmark::wave5();
        let bug = bench.get(BugId(1)).unwrap().clone();
        let mut m = DriftingHashMachine {
            marker: bug.serial_marker.as_bytes().to_vec(),
            runs: 0,
            current: mint_scenario_env(0, &bug, 0, 64),
            snaps: std::collections::BTreeMap::new(),
            next: 1,
        };
        // One branch only, so exactly one finding-vs-replay comparison happens.
        let cfg = BenchConfig {
            max_branches: 1,
            ..BenchConfig::smoke(1)
        };
        let log = run_bench_campaign(&mut m, &bug, &cfg, Configuration::Baseline)
            .unwrap()
            .log;
        assert!(
            log.finds.is_empty(),
            "replays that differ from the finding hash must not certify a find"
        );
    }

    /// The bug's terminal serial marker does NOT leak into the novelty cell stream
    /// (round-6 P1): a run WITH the marker line yields the same cells as one
    /// WITHOUT it — so novelty never correlates with the attribution marker.
    #[test]
    fn terminal_marker_excluded_from_cells() {
        let bench = Benchmark::wave5();
        let spec = bench.get(BugId(3)).unwrap().clone();
        let mk = |lines: &[&str]| RunTrace {
            terminal: StopReason::Quiescent {
                vtime: Moment(BASE_VTIME),
            },
            env: mint_scenario_env(0, &spec, 0, 64),
            coverage: None,
            events: Vec::new(),
            records: lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    (
                        Moment(BASE_VTIME + i as u64),
                        Record {
                            stream: StreamId(0),
                            line: l.as_bytes().to_vec(),
                        },
                    )
                })
                .collect(),
        };
        // Fresh signal per call: the marker is filtered before clustering, so the
        // "with" trace's filtered console equals the "without" one → identical cells.
        let without = cells_of(
            &spec,
            &SignalCells::new(),
            &mk(&["supervisor boot", "uuid drawn"]),
        );
        let with = cells_of(
            &spec,
            &SignalCells::new(),
            &mk(&["supervisor boot", "uuid drawn", "UUID_BUG: matched"]),
        );
        assert_eq!(
            without, with,
            "the terminal marker must not add a novelty cell"
        );
        assert!(!without.is_empty());
    }

    /// The cells are the **real CellFnV1** signal — count-based (species-progress),
    /// so (a) a run touches MORE distinct cells the more distinct log-template
    /// species it emits (the ladder toward the bug), (b) two INDEPENDENT campaigns
    /// keying runs of the same species-count agree (cross-campaign comparability
    /// with no shared codebook — what makes the seeds parallelize and still pool),
    /// and (c) numeric parameters are clustered away (Drain-style), so lines
    /// differing only in digits are one species.
    #[test]
    fn cells_track_species_count_and_pool_across_campaigns() {
        let bench = Benchmark::wave5();
        let spec = bench.get(BugId(1)).unwrap().clone();
        let run = |lines: &[&str]| -> Vec<u64> {
            let t = RunTrace {
                terminal: StopReason::Quiescent {
                    vtime: Moment(BASE_VTIME),
                },
                env: mint_scenario_env(0, &spec, 0, 64),
                coverage: None,
                events: Vec::new(),
                records: lines
                    .iter()
                    .enumerate()
                    .map(|(i, l)| {
                        (
                            Moment(BASE_VTIME + i as u64),
                            Record {
                                stream: StreamId(0),
                                line: l.as_bytes().to_vec(),
                            },
                        )
                    })
                    .collect(),
            };
            // A FRESH campaign each call (independent codebook) — the cross-campaign
            // comparability below relies on the key being codebook-order-independent.
            cells_of(&spec, &SignalCells::new(), &t)
        };
        let distinct = |lines: &[&str]| -> BTreeSet<u64> { run(lines).into_iter().collect() };
        // (a) More distinct species ⇒ more distinct cells (the species ladder).
        let one = distinct(&["ledger mapped"]);
        let three = distinct(&[
            "ledger mapped",
            "guard bit aligned",
            "sensitive window entered",
        ]);
        assert!(
            three.len() > one.len(),
            "more species ⇒ more distinct cells ({} vs {})",
            three.len(),
            one.len()
        );
        // (b) Two independent campaigns keying the same species-count sequence agree.
        assert_eq!(
            run(&["a alpha", "b beta"]),
            run(&["c gamma", "d delta"]),
            "same species-count sequence ⇒ same cells across independent campaigns"
        );
        // (c) Numeric params cluster to one species (Drain-style), so two
        // digit-only-varying lines key the same DISTINCT cell as a single one.
        assert_eq!(
            distinct(&["phase gpa=1000 at=3", "phase gpa=2000 at=9"]),
            distinct(&["phase gpa=5 at=7"]),
            "numeric params cluster to one species"
        );
    }

    /// The signal configuration discovers cells (the log-template signal is
    /// live): a run accumulates more than one distinct cell across branches.
    #[test]
    fn signal_accumulates_cells() {
        let bench = Benchmark::wave5();
        let bug = bench.get(BugId(1)).unwrap().clone();
        // Miri: 32 branches — this test's claim ("the signal config accumulates
        // more than one distinct cell") is budget-INDEPENDENT: the seed-0x1234
        // Signal campaign has already touched 5 distinct cells by branch 16.
        let cfg = smoke_cfg(0x1234, 32);
        let mut m = BenchToyMachine::new(bug.clone());
        let log = run_bench_campaign(&mut m, &bug, &cfg, Configuration::Signal)
            .unwrap()
            .log;
        let distinct: BTreeSet<u64> = log
            .events
            .iter()
            .flat_map(|e| e.touched.iter().copied())
            .collect();
        assert!(
            distinct.len() > 1,
            "signal must accumulate >1 cell, got {}",
            distinct.len()
        );
    }
}
