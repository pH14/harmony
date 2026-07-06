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
//! On the M1 toy path the cells are **content-keyed log-template species** (see
//! [`cells_of`] — Drain-style clustering keyed by content, so ids are stable
//! across independent logs). The M2 SocketMachine path swaps in the real task-67
//! `logtmpl` `LogSensor`/`CellFnV1` over the guest's captured console, with a
//! persisted campaign codebook for the same cross-log id stability.
//!
//! ## Why a bespoke loop (not `Explorer`)
//!
//! The log-template signal is **point-in-time over a whole run's console**, not a
//! per-fork coverage bitmap; the toy guest surfaces no sealable fork points, so
//! [`explorer::CoverageArchive`]'s fork-time admission has nothing to key on.
//! This driver therefore owns the cell function directly: per branch it builds the
//! [`RunTrace`] (with the console captured through the new [`Machine::console`]
//! seam — the task-69 engine fix), content-keys the console lines into the
//! branch's **cell set** ([`cells_of`]), and admits an exemplar to a thin novelty
//! archive. Signal exploits novel exemplars; baseline always explores from
//! genesis. Everything is a pure function of `(campaign_seed, spec, config)`, so a
//! rerun is bit-identical (the box determinism smoke test).
//!
//! **Milestone boundary (task 69):** this is the *mechanism* + a determinism
//! smoke test. The full ≥20-seed campaign that produces the actual GO/NO-GO
//! ruling is milestone 2. Nothing here decides the gate.

use std::collections::BTreeSet;

use benchmark::manifest::{BugSpec, TriggerParams};
use benchmark::report::{BranchEvent, CampaignLog, Configuration, FindRecord};
use benchmark::trigger::{self, FaultKind, Perturbation, Scenario};
use environment::{BitMask, EnvSpec, FaultPolicy, HostFault};
use explorer::{
    AdapterEnv, EnvCodec, Environment, Machine, MachineError, Moment, Oracle, Prng, Record,
    RunTrace, SnapId, StopConditions, StopMask, StopReason, StreamId, TerminalOracle, VTime,
};

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
    /// a novel frontier exemplar. Ignored by the baseline (always explores).
    pub explore_period: u64,
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
            fault_rebase: 0,
        }
    }

    /// A **box** campaign configuration driving a real [`SocketMachine`]: the same
    /// search knobs, but fault moments are rebased by [`BASE_VTIME`] so the
    /// adapter's `+ seal` re-anchoring lands them in the guest's real window
    /// ([`fault_rebase`](Self::fault_rebase)).
    pub fn box_campaign(campaign_seed: u64, max_branches: u64, replay_n: usize) -> Self {
        Self {
            campaign_seed,
            max_branches,
            replay_n,
            explore_period: 4,
            fault_rebase: BASE_VTIME,
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
pub fn mint_scenario_env(seed: u64, spec: &BugSpec, rebase: u64) -> Environment {
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
            let vec_pick = one_of(&vectors, &mut p) as u8;
            let at = window.0.saturating_sub(rebase).saturating_sub(4) + p.next_u64() % 64;
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
fn scenario_of(env: &Environment) -> Scenario {
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
                HostFault::InjectInterrupt { vector } => {
                    Some(FaultKind::InjectInterrupt { vector })
                }
                _ => None,
            };
            kind.map(|kind| Perturbation { at, kind })
        })
        .collect();
    Scenario { seed, faults }
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
    current: Environment,
    vtime: u64,
    snaps: std::collections::BTreeMap<u64, (u64, Environment)>,
    next_snap: u64,
    last_console: Vec<(u64, Vec<u8>)>,
}

impl BenchToyMachine {
    /// A fresh toy guest for `spec`, quiescent at [`BASE_VTIME`].
    pub fn new(spec: BugSpec) -> Self {
        Self {
            spec,
            current: mint_scenario_env(0, &spec_placeholder(), 0),
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
    fn branch(&mut self, snap: SnapId, env: &Environment) -> Result<(), MachineError> {
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
                vtime: VTime(terminal_vtime),
                info: vec![self.spec.crash_kind as u8, self.spec.id.0 as u8],
            })
        } else {
            Ok(StopReason::Quiescent {
                vtime: VTime(terminal_vtime),
            })
        }
    }

    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        let id = self.next_snap;
        self.next_snap += 1;
        self.snaps.insert(id, (self.vtime, self.current.clone()));
        Ok(SnapId(id))
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
        h.update(b"conductor.benchtoy.state_hash.v1");
        h.update((self.current.bytes.len() as u64).to_le_bytes());
        h.update(&self.current.bytes);
        Ok(h.finalize().into())
    }

    fn coverage(&self) -> &[u8] {
        &[]
    }

    fn recorded_env(&self) -> Result<Environment, MachineError> {
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
    env: Environment,
    path_len: u64,
    novel_on_path: u64,
}

/// A **content-keyed** log-template species id, STABLE across logs (round-6 P2).
/// Each console line is clustered Drain-style — variable tokens (ASCII digits)
/// are collapsed to `#` so numeric parameters don't fragment a template — then
/// content-hashed (FNV-1a). The same template therefore gets the same id in every
/// campaign, so the report can pool species across independent logs (a
/// per-campaign codebook counter could not — it assigns ids first-come, so the
/// same template drifts to different ids across logs).
fn content_cell_id(line: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in line {
        let tok = if b.is_ascii_digit() { b'#' } else { b };
        h ^= tok as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Whether `hay` contains `needle` as a byte substring.
fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Build the `RunTrace` for a finished branch, capturing the console into
/// `records` (the scrape channel the sensor reads).
fn trace_of<M: Machine>(
    machine: &mut M,
    stop: StopReason,
    env: Environment,
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

/// The per-branch cell set — content-keyed log-template species (with recurrence,
/// the STADS abundance stream). The bug's terminal serial MARKER is filtered OUT
/// (round-6 P1): the marker is *attribution*, not a behavioural cell, and letting
/// it into the novelty stream would make novelty correlate with bug discovery
/// **spuriously** (the signal measuring its own attribution marker). The full,
/// unfiltered trace is still used by [`marker_attributed`] for attribution.
///
/// This is the toy path's stable stand-in for the Phase-D `logtmpl` signal
/// (`LogSensor` → `CellFnV1`): identical clustering (strip params, cluster by
/// template) but keyed by content so ids are stable across independent logs. The
/// M2 SocketMachine path runs the real `LogSensor`/`CellFnV1` with a **persisted
/// campaign codebook** (`codebook_bytes`), which gives the same cross-log id
/// stability on the box.
fn cells_of(spec: &BugSpec, trace: &RunTrace) -> Vec<u64> {
    let marker = spec.serial_marker.as_bytes();
    trace
        .records
        .iter()
        .filter(|(_, r)| !contains(&r.line, marker))
        .map(|(_, r)| content_cell_id(&r.line))
        .collect()
}

/// Drive one benchmark campaign against `machine` under `config` and return its
/// discovery-event log. Seals a base, then per branch: pick a base env (signal
/// exploits novel exemplars; baseline explores from genesis), run, capture the
/// console → cells, admit novel exemplars, and judge — recording the first find
/// per bug (its time-to-bug).
pub fn run_bench_campaign<M: Machine>(
    machine: &mut M,
    codec: &dyn EnvCodec,
    spec: &BugSpec,
    cfg: &BenchConfig,
    config: Configuration,
) -> Result<CampaignLog, MachineError> {
    let oracle = TerminalOracle::new();

    // Seal the base (the toy is quiescent at boot).
    let base = machine.snapshot()?;
    let until = StopConditions {
        deadline: None,
        on: StopMask::NONE,
    };

    let mut prng = Prng::new(cfg.campaign_seed);
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    // The novelty frontier carries per-exemplar ancestor-chain metadata so a
    // find that exploits a novel parent counts that parent's novelty (measure 2).
    let mut frontier: Vec<Exemplar> = Vec::new();
    let mut events = Vec::new();
    let mut finds = Vec::new();
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
            let e = codec.mutate(&parent.env, prng.next_u64());
            (e, parent.path_len, parent.novel_on_path)
        } else {
            (
                mint_scenario_env(prng.next_u64(), spec, cfg.fault_rebase),
                0,
                0,
            )
        };

        machine.branch(base, &env)?;
        let stop = machine.run(&until, None)?;
        // The finding run's state_hash, captured before any replay disturbs the
        // machine — a certified find must replay N/N identical to THIS.
        let run_hash = machine.hash()?;
        let trace = trace_of(machine, stop, env.clone())?;
        let touched = cells_of(spec, &trace);

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
        // console (per-bug attribution), and (b) the emitted reproducer replays
        // the IDENTICAL crash `cfg.replay_n` (25/25) times.
        if !found && oracle.judge(&trace).is_some() && marker_attributed(&trace, spec) {
            let certified =
                certify_replays(machine, base, &env, &trace.terminal, run_hash, cfg.replay_n)?;
            if certified {
                finds.push(FindRecord {
                    bug: spec.id,
                    branch,
                    path_len,
                    novel_on_path,
                });
                found = true;
            }
        }
    }

    machine.drop_snap(base)?;
    Ok(CampaignLog {
        bug: spec.id,
        config,
        seed: cfg.campaign_seed,
        events,
        finds,
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

/// Certify a candidate find by **N/N replay**: replay the reproducer `n` times
/// and require every run to reproduce the identical bug-bearing
/// `(stop, state_hash)` **of the finding run** (box gate 2 — N/N identical to the
/// FINDING, not merely to each other). A flaky/non-deterministic crash whose
/// replays are self-consistent but differ from the finding run fails this, so it
/// is never logged as a find. `n == 0` never certifies.
fn certify_replays<M: Machine>(
    machine: &mut M,
    base: SnapId,
    env: &Environment,
    found_stop: &StopReason,
    found_hash: [u8; 32],
    n: usize,
) -> Result<bool, MachineError> {
    if n == 0 || !found_stop.is_bug() {
        return Ok(false);
    }
    let replays = verify_replays(machine, base, env, n)?;
    // Every replay must reproduce the FINDING run's exact (stop, state_hash).
    Ok(replays.len() == n
        && replays
            .iter()
            .all(|(stop, h)| stop == found_stop && stop.is_bug() && *h == found_hash))
}

/// Replay a found reproducer `n` times, returning the `(stop, state_hash)` seen —
/// the N/N verification (box gate 2). All must be identical.
pub fn verify_replays<M: Machine>(
    machine: &mut M,
    base: SnapId,
    env: &Environment,
    n: usize,
) -> Result<Vec<(StopReason, [u8; 32])>, MachineError> {
    let until = StopConditions {
        deadline: None,
        on: StopMask::NONE,
    };
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        machine.branch(base, env)?;
        let stop = machine.run(&until, None)?;
        let hash = machine.hash()?;
        out.push((stop, hash));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use benchmark::manifest::{Benchmark, BugId};
    use explorer::SpecEnvCodec;

    fn codec() -> SpecEnvCodec {
        SpecEnvCodec
    }

    /// Each bug's toy machine crashes on its trigger and halts nominally, and the
    /// crash carries the per-bug id (attribution).
    #[test]
    fn each_toy_crashes_on_trigger_halts_nominal() {
        for spec in Benchmark::wave5().bugs {
            let mut m = BenchToyMachine::new(spec.clone());
            let base = m.snapshot().unwrap();
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
                let abs = scenario_of(&mint_scenario_env(seed, &spec, 0));
                let boxed = scenario_of(&mint_scenario_env(seed, &spec, BASE_VTIME));
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
    fn env_of_scenario(sc: &Scenario, _spec: &BugSpec) -> Environment {
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
                FaultKind::InjectInterrupt { vector } => HostFault::InjectInterrupt { vector },
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
    fn dual_config_runs_and_is_deterministic_twice() {
        let bench = Benchmark::wave5();
        let bug = bench.get(BugId(1)).unwrap().clone();
        for config in [Configuration::Signal, Configuration::Baseline] {
            let cfg = BenchConfig::smoke(0xBEEF_0069);
            let mut m1 = BenchToyMachine::new(bug.clone());
            let log1 = run_bench_campaign(&mut m1, &codec(), &bug, &cfg, config).unwrap();
            let mut m2 = BenchToyMachine::new(bug.clone());
            let log2 = run_bench_campaign(&mut m2, &codec(), &bug, &cfg, config).unwrap();
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
        let cfg = BenchConfig::smoke(0x0033_1D69);
        for config in [Configuration::Signal, Configuration::Baseline] {
            let mut m = BenchToyMachine::new(bug.clone());
            let log = run_bench_campaign(&mut m, &codec(), &bug, &cfg, config).unwrap();
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
        let cfg = BenchConfig::smoke(0xBEEF_0069);
        let mut m = BenchToyMachine::new(bug.clone());
        let log =
            run_bench_campaign(&mut m, &codec(), &bug, &cfg, Configuration::Baseline).unwrap();
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
            current: Environment,
            snaps: std::collections::BTreeMap<u64, Environment>,
            next: u64,
        }
        impl Machine for SilentCrashMachine {
            fn branch(&mut self, s: SnapId, e: &Environment) -> Result<(), MachineError> {
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
                    vtime: VTime(BASE_VTIME + 1),
                    info: vec![0, 0],
                })
            }
            fn snapshot(&mut self) -> Result<SnapId, MachineError> {
                let id = self.next;
                self.next += 1;
                self.snaps.insert(id, self.current.clone());
                Ok(SnapId(id))
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
            fn recorded_env(&self) -> Result<Environment, MachineError> {
                Ok(self.current.clone())
            }
            fn console(&mut self) -> Result<Vec<(u64, Vec<u8>)>, MachineError> {
                Ok(Vec::new()) // no marker ever
            }
        }
        let bench = Benchmark::wave5();
        let bug = bench.get(BugId(1)).unwrap().clone();
        let mut m = SilentCrashMachine {
            current: mint_scenario_env(0, &bug, 0),
            snaps: std::collections::BTreeMap::new(),
            next: 1,
        };
        let cfg = BenchConfig {
            max_branches: 8,
            ..BenchConfig::smoke(1)
        };
        let log =
            run_bench_campaign(&mut m, &codec(), &bug, &cfg, Configuration::Baseline).unwrap();
        assert!(
            log.finds.is_empty(),
            "an unmarked crash must not be certified as a find"
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
            current: Environment,
            snaps: std::collections::BTreeMap<u64, Environment>,
            next: u64,
        }
        impl Machine for DriftingHashMachine {
            fn branch(&mut self, s: SnapId, e: &Environment) -> Result<(), MachineError> {
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
                    vtime: VTime(BASE_VTIME + 1),
                    info: vec![0, 1],
                })
            }
            fn snapshot(&mut self) -> Result<SnapId, MachineError> {
                let id = self.next;
                self.next += 1;
                self.snaps.insert(id, self.current.clone());
                Ok(SnapId(id))
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
            fn recorded_env(&self) -> Result<Environment, MachineError> {
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
            current: mint_scenario_env(0, &bug, 0),
            snaps: std::collections::BTreeMap::new(),
            next: 1,
        };
        // One branch only, so exactly one finding-vs-replay comparison happens.
        let cfg = BenchConfig {
            max_branches: 1,
            ..BenchConfig::smoke(1)
        };
        let log =
            run_bench_campaign(&mut m, &codec(), &bug, &cfg, Configuration::Baseline).unwrap();
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
                vtime: VTime(BASE_VTIME),
            },
            env: mint_scenario_env(0, &spec, 0),
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
        let without = cells_of(&spec, &mk(&["supervisor boot", "uuid drawn"]));
        // The same run plus the terminal marker line.
        let with = cells_of(
            &spec,
            &mk(&["supervisor boot", "uuid drawn", "UUID_BUG: matched"]),
        );
        assert_eq!(
            without, with,
            "the terminal marker must not add a novelty cell"
        );
        assert!(!without.is_empty());
    }

    /// Cell ids are **content-keyed and stable across logs** (round-6 P2): the same
    /// console line yields the same cell id in two unrelated traces, and numeric
    /// parameters are clustered away (Drain-style).
    #[test]
    fn cell_ids_are_content_stable_across_logs() {
        let bench = Benchmark::wave5();
        let spec = bench.get(BugId(1)).unwrap().clone();
        let line = |s: &str| RunTrace {
            terminal: StopReason::Quiescent {
                vtime: VTime(BASE_VTIME),
            },
            env: mint_scenario_env(0, &spec, 0),
            coverage: None,
            events: Vec::new(),
            records: vec![(
                Moment(BASE_VTIME),
                Record {
                    stream: StreamId(0),
                    line: s.as_bytes().to_vec(),
                },
            )],
        };
        // Same template in two different "logs" → identical id.
        assert_eq!(
            cells_of(&spec, &line("ledger mapped")),
            cells_of(&spec, &line("ledger mapped"))
        );
        // Numeric params are clustered away: same template, different numbers.
        assert_eq!(
            cells_of(&spec, &line("phase gpa=1000 at=3")),
            cells_of(&spec, &line("phase gpa=2000 at=9")),
        );
        // Distinct templates → distinct ids.
        assert_ne!(
            cells_of(&spec, &line("ledger mapped")),
            cells_of(&spec, &line("guard bit aligned"))
        );
    }

    /// The signal configuration discovers cells (the log-template signal is
    /// live): a run accumulates more than one distinct cell across branches.
    #[test]
    fn signal_accumulates_cells() {
        let bench = Benchmark::wave5();
        let bug = bench.get(BugId(1)).unwrap().clone();
        let cfg = BenchConfig::smoke(0x1234);
        let mut m = BenchToyMachine::new(bug.clone());
        let log = run_bench_campaign(&mut m, &codec(), &bug, &cfg, Configuration::Signal).unwrap();
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
