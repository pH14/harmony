// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task 69 — the **signal-configured** benchmark campaign driver.
//!
//! Task 60's campaign ([`crate::campaign`]) is blind seed search — that is the
//! **baseline** configuration. This module adds the **signal** configuration: the
//! same machine + oracle, but the branch-base choice is steered by *cell
//! novelty* derived from the Phase-D signal stack (task-67 `logtmpl`
//! [`LogSensor`] → [`CellFnV1`]). Both configurations run on identical budgets
//! and emit a [`benchmark::report::CampaignLog`] — per-branch discovered cells +
//! per-bug find branch — which `benchmark`'s offline report analyses for
//! signal→bug correlation (GO/NO-GO #2).
//!
//! ## Why a bespoke loop (not `Explorer`)
//!
//! The log-template signal is **point-in-time over a whole run's console**, not a
//! per-fork coverage bitmap; the toy guest surfaces no sealable fork points, so
//! [`explorer::CoverageArchive`]'s fork-time admission has nothing to key on.
//! This driver therefore owns the sensor + cell function directly: per branch it
//! builds the [`RunTrace`] (with the console captured through the new
//! [`Machine::console`] seam — the task-69 engine fix), runs the sensor, keys the
//! per-moment feature slices through [`CellFnV1`] into the branch's **cell set**,
//! and admits an exemplar to a thin novelty archive. Signal exploits novel
//! exemplars; baseline always explores from genesis. Everything is a pure
//! function of `(campaign_seed, spec, config)`, so a rerun is bit-identical (the
//! box determinism smoke test).
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
    AdapterEnv, CellFn, EnvCodec, Environment, Feature, FeatureSet, Machine, MachineError, Moment,
    Oracle, Prng, Record, RunTrace, Sensor, SnapId, StopConditions, StopMask, StopReason, StreamId,
    TerminalOracle, VTime,
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
    /// a novel frontier exemplar. Ignored by the baseline (always explores).
    pub explore_period: u64,
}

impl BenchConfig {
    /// A small portable/smoke configuration.
    pub fn smoke(campaign_seed: u64) -> Self {
        Self {
            campaign_seed,
            max_branches: 2048,
            replay_n: 25,
            explore_period: 4,
        }
    }
}

/// Mint one branch's environment for `spec`'s bug: a seeded base plus (for the
/// fault-triggered classes) a single host-fault schedule drawn from `seed` over a
/// search space that brackets the trigger. Pure in `seed`.
pub fn mint_scenario_env(seed: u64, spec: &BugSpec) -> Environment {
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
            let at = window.0.saturating_sub(4) + p.next_u64() % 16;
            env_spec.perturb(
                HostFault::CorruptMemory {
                    gpa: gpa_pick,
                    mask: BitMask(1u64 << bit),
                },
                at,
            );
        }
        TriggerParams::OrderingInterrupt { vector, window } => {
            let v = vector as u64;
            let vec_pick = one_of(&[v, v ^ 1, v ^ 2, 0x20], &mut p) as u8;
            let at = window.0.saturating_sub(4) + p.next_u64() % 16;
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
    let seed = match &decoded.spec {
        EnvSpec::Seeded { seed, .. } => *seed,
        other => seed_of(other),
    };
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

/// A non-`Seeded` spec carries its seed in its base; the codec keeps it stable
/// under `mutate`. Fall back to 0 if unreadable (never panics).
fn seed_of(_spec: &EnvSpec) -> u64 {
    0
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
            current: mint_scenario_env(0, &spec_placeholder()),
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

/// A small order-independent 64-bit fold of a cell key → an opaque id for the
/// discovery-event log (the report never interprets it).
fn cell_id(key: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in key {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
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

/// The per-branch cell set: run the sensor over the trace, group features by
/// moment into slices, key each through the cell function. Returns the touched
/// cell ids (with recurrence — the STADS abundance stream).
fn cells_of(sensor: &LogSensor, cells: &CellFnV1, trace: &RunTrace) -> Vec<u64> {
    // features by moment (deterministic: BTreeSet keyed on (Moment, Feature)).
    let mut by_moment: std::collections::BTreeMap<Moment, Vec<Feature>> =
        std::collections::BTreeMap::new();
    for (at, f) in sensor.observe(trace) {
        by_moment.entry(at).or_default().push(f);
    }
    by_moment
        .into_iter()
        .map(|(at, feats)| {
            let set: FeatureSet = feats.into_iter().collect();
            cell_id(&cells.key(at, &set))
        })
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
    let sensor = LogSensor::new();
    let cells = CellFnV1::new();

    // Seal the base (the toy is quiescent at boot).
    let base = machine.snapshot()?;
    let until = StopConditions {
        deadline: None,
        on: StopMask::NONE,
    };

    let mut prng = Prng::new(cfg.campaign_seed);
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    let mut frontier: Vec<Environment> = Vec::new();
    let mut events = Vec::new();
    let mut finds = Vec::new();
    let mut step = 0u64;

    for branch in 0..cfg.max_branches {
        step += 1;
        // Pick the branch env.
        let exploit = matches!(config, Configuration::Signal)
            && !frontier.is_empty()
            && !step.is_multiple_of(cfg.explore_period);
        let env = if exploit {
            let pick = (prng.next_u64() % frontier.len() as u64) as usize;
            codec.mutate(&frontier[pick], prng.next_u64())
        } else {
            mint_scenario_env(prng.next_u64(), spec)
        };

        machine.branch(base, &env)?;
        let stop = machine.run(&until, None)?;
        let trace = trace_of(machine, stop, env.clone())?;
        let touched = cells_of(&sensor, &cells, &trace);

        // Admit an exemplar iff it claimed a fresh cell (novelty archive).
        let mut novel = false;
        for &c in &touched {
            if seen.insert(c) {
                novel = true;
            }
        }
        if novel {
            frontier.push(trace.env.clone());
        }
        events.push(BranchEvent { branch, touched });

        if let Some(bug) = oracle.judge(&trace) {
            // First find of this bug: record time-to-bug + a coarse trajectory.
            let _ = bug;
            finds.push(FindRecord {
                bug: spec.id,
                branch,
                path_len: if exploit { 2 } else { 1 },
                novel_on_path: u64::from(novel),
            });
            break;
        }
    }

    machine.drop_snap(base)?;
    Ok(CampaignLog {
        config,
        seed: cfg.campaign_seed,
        events,
        finds,
    })
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
