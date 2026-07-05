// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **task-68 chain protocol**: drive the explorer's [`Materializer`] (the
//! lazy-materialization engine + spanning-ancestor retention pool) against a
//! live [`Machine`] over the task-58 socket, and check the three task-68 box
//! gates as a pure function of the observed report.
//!
//! The protocol is workload-blind (the same code runs against the scripted
//! mock guest and the box's Postgres image; only the composition root knows
//! which) and builds the chain exactly the way the Archive addresses states
//! under the task-63 **GO (grid-restricted)** ruling: every hop is
//! `branch → run(deadline) → seal`, keyed by the **landed synchronized
//! boundary** (never the requested interior `Moment`), with every suffix a
//! real [`Machine::recorded_env`] and every fold the production
//! [`EnvCodec::compose`]:
//!
//! 1. Seal the **base** (the campaign genesis) at the current point.
//! 2. Build an `n ≥ 3` hop chain of seals below it, registering each hop as a
//!    frontier exemplar + lineage record.
//! 3. **Gate (a) — measured depth:** evict the deepest exemplar's own seal
//!    and materialize it: the replay must be parent-rooted (only the last
//!    suffix), and its depth ratio against a full from-scratch re-execution
//!    is the number to quote against the task-63 §4 baseline.
//! 4. **Gate (b) — eviction round-trip:** evict the retained ancestor,
//!    re-materialize (deeper, compose-folded replay) → the `state_hash` must
//!    be **bit-identical**; then evict everything and re-materialize from the
//!    base (the graceful worst case) → still bit-identical.
//! 5. **Gate (c) — the composed reproducer:** run a tail below the deep
//!    exemplar (a "bug" leg — under the seed-driven v1 vocabulary its stop is
//!    a deadline, the contract being replay identity), fold its delta down
//!    the chain via `compose` exactly as `Explorer::report` mints a
//!    `Bug.env`, and `branch(base, composed)` → identical stop + hash.
//!
//! [`verify_materialize`] returns every violated gate; a round-trip hash
//! mismatch carries the **sequential-entropy-splice diagnostic**. Since task
//! 78 the env format stores every hop's **reseed marker** and the server
//! re-executes each collapsed hop's reseed at its recorded Moment, so folds
//! are bit-identical **even with draws inside collapsed intervals** — the
//! loopback suite pins the positive property portably on a draw-carrying mock
//! script, and [`MaterializeReport::tail_draws`] measures (never assumes)
//! whether a live window actually drew.

use explorer::{
    EnvCodec, Environment, ExemplarRef, Frontier, FrontierEntry, Machine, MachineError,
    Materialization, Materializer, Moment, Reward, SnapId, StopConditions, StopMask, StopReason,
    VTime, VirtualExemplar,
};

use crate::{fmt_stop, hex, probe_vtime};

/// The task-63 §4 measured baseline this task's gate (a) must beat, in parts
/// per million of a full from-scratch re-execution: `SEAL-RATE-REPORT.md` §6
/// measured suffix/genesis = 7 159 536 / 462 999 204 = **1.5463 %** (15 463
/// ppm) on the real Postgres workload.
pub const TASK63_BASELINE_PPM: u64 = 15_463;

/// Configuration for one [`run_materialize`].
#[derive(Clone, Debug)]
pub struct MaterializeConfig {
    /// The chain seed (every hop branches with `codec.seeded(seed)` — chains
    /// are same-seed by the compose contract).
    pub seed: u64,
    /// Chain seals below the base. Must be ≥ 3 (gate (b) needs a retained
    /// ancestor *above* the evicted parent so the fold is a real
    /// intermediate-collapsing replay, not just the genesis worst case).
    pub hops: usize,
    /// Requested V-time per hop; the landed synchronized boundary keys the
    /// exemplar (grid-restricted — overshoot is expected and reported).
    pub hop_delta: u64,
    /// The reproducer leg's requested run past the deepest seal.
    pub tail_delta: u64,
    /// Snapshot retry: on `NotQuiescent`, advance this much V-time and retry…
    pub snapshot_retry_step: u64,
    /// …at most this many times before giving up loudly.
    pub snapshot_max_attempts: usize,
}

impl Default for MaterializeConfig {
    fn default() -> Self {
        MaterializeConfig {
            seed: 0x0028_C0FF_EE5E_EDC0,
            hops: 3,
            hop_delta: 250,
            tail_delta: 250,
            snapshot_retry_step: 100,
            snapshot_max_attempts: 64,
        }
    }
}

/// One chain hop as built: where it was aimed, where the boundary landed, and
/// how many seal attempts it took.
#[derive(Clone, Copy, Debug)]
pub struct HopRow {
    /// The requested deadline (`prior at + hop_delta`).
    pub requested: u64,
    /// The landed (and sealed) synchronized boundary — the exemplar's `at`.
    pub at: u64,
    /// Seal attempts (1 = first try).
    pub attempts: usize,
}

/// What one [`run_materialize`] observed.
#[derive(Clone, Debug)]
pub struct MaterializeReport {
    /// The V-time the base (campaign genesis) was sealed at.
    pub genesis_at: u64,
    /// Seal attempts for the base.
    pub genesis_attempts: usize,
    /// The chain, shallowest first.
    pub hops: Vec<HopRow>,
    /// Gate (a): the deep exemplar materialized from its direct parent.
    pub hot: Materialization,
    /// `state_hash` of the hot materialization.
    pub hot_hash: [u8; 32],
    /// Gate (b): re-materialized after the direct parent's eviction (the
    /// compose-folded, deeper replay).
    pub folded: Materialization,
    /// `state_hash` of the folded re-materialization.
    pub folded_hash: [u8; 32],
    /// The graceful worst case: everything evicted, replayed from the base.
    pub worst: Materialization,
    /// `state_hash` of the worst-case re-materialization.
    pub worst_hash: [u8; 32],
    /// Gate (c): the "bug" leg's stop (run below the deep exemplar).
    pub leg_stop: StopReason,
    /// `state_hash` at the bug leg's stop.
    pub leg_hash: [u8; 32],
    /// The compose-folded, genesis-complete reproducer (no `SnapId` in it).
    pub bug_env: Environment,
    /// The replay leg's stop (`branch(base, bug_env)` run to the same
    /// absolute deadline).
    pub replay_stop: StopReason,
    /// `state_hash` at the replay leg's stop.
    pub replay_hash: [u8; 32],
    /// **Draw probes (task 78), per collapsed hop window:** `hop_draws[j]` is
    /// `true` iff hop `j`'s window (parent seal → landed boundary) actually
    /// draws entropy — measured with the same trailing-reseed probe as
    /// [`tail_draws`](MaterializeReport::tail_draws) (below). These are the
    /// windows a compose-fold collapses, so the reseed-aware bit-identity
    /// gates ((b)/(c)) exercise entropy exactly when one of these is `true`.
    pub hop_draws: Vec<bool>,
    /// **Draw probe (task 78):** `true` iff the tail window actually draws
    /// entropy — measured, not assumed. The probe leg re-runs the tail window
    /// under the SAME branch seed plus a trailing reseed marker back to that
    /// seed at the landing boundary (using the very task-78 machinery under
    /// test): a no-op iff no draw moved the stream, so its hash differs from
    /// the tail leg's iff the window drew. The reseed-aware fold gates
    /// ((b)/(c) bit-identity) are only *entropy-exercising* when this is
    /// `true`; a draw-free workload window leaves them vacuous on the entropy
    /// axis (the task-68 situation).
    pub tail_draws: bool,
}

/// Seal the machine's current point, nudging past non-sealable boundaries the
/// same way the task-58 sweep does: on `NotQuiescent` (an RNG mid-exit
/// completion or a non-synchronized point), run `retry_step` further — landing
/// on the next synchronized boundary — and try again. Returns the seal, the
/// V-time it landed at, and the attempt count.
fn seal_here<M: Machine>(
    machine: &mut M,
    mut vt: u64,
    retry_step: u64,
    max_attempts: usize,
) -> Result<(SnapId, u64, usize), MachineError> {
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        match machine.snapshot() {
            Ok(snap) => return Ok((snap, vt, attempts)),
            Err(MachineError::NotQuiescent) if attempts < max_attempts => {
                let stop = machine.run(
                    &StopConditions {
                        deadline: Some(VTime(vt.saturating_add(retry_step))),
                        on: StopMask::NONE,
                    },
                    None,
                )?;
                if !matches!(stop, StopReason::Deadline { .. }) {
                    // The guest ended before a sealable boundary was found.
                    return Err(MachineError::NotQuiescent);
                }
                vt = stop.vtime().0;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Run one hop leg: `run` to `deadline` under [`StopMask::NONE`], requiring a
/// `Deadline` stop (the workload must not end mid-chain).
fn run_to<M: Machine>(machine: &mut M, deadline: u64, what: &str) -> Result<u64, MachineError> {
    let stop = machine.run(
        &StopConditions {
            deadline: Some(VTime(deadline)),
            on: StopMask::NONE,
        },
        None,
    )?;
    match stop {
        StopReason::Deadline { vtime } => Ok(vtime.0),
        other => Err(MachineError::Transport(format!(
            "{what}: expected a Deadline stop at {deadline}, the guest ended first: {}",
            fmt_stop(&other)
        ))),
    }
}

/// The chain protocol (module doc). Drives any [`Machine`]; in this crate that
/// is always the socket adapter, so every verb crosses the wire and every
/// suffix/fold uses the production codec + real `recorded_env`.
///
/// # Panics
/// If `cfg.hops < 3` — the gates need a chain deep enough that evicting the
/// direct parent still leaves a retained non-genesis ancestor.
pub fn run_materialize<M: Machine>(
    machine: &mut M,
    codec: &dyn EnvCodec,
    cfg: &MaterializeConfig,
) -> Result<MaterializeReport, MachineError> {
    assert!(
        cfg.hops >= 3,
        "the chain gates need hops >= 3 (got {}) — gate (b)'s fold must collapse a real \
         intermediate below a retained non-genesis ancestor",
        cfg.hops
    );

    // 1. The base: probe the current V-time and seal the campaign genesis.
    let v0 = probe_vtime(machine)?;
    let (genesis, genesis_at, genesis_attempts) = seal_here(
        machine,
        v0,
        cfg.snapshot_retry_step,
        cfg.snapshot_max_attempts,
    )?;
    let mut mat = Materializer::new(genesis, Moment(genesis_at));
    let mut frontier = Frontier::new();

    // 2. The chain: branch → run(deadline) → seal per hop, keyed by the
    //    landed boundary (grid-restricted), each suffix a real recorded_env.
    let mut hops: Vec<HopRow> = Vec::with_capacity(cfg.hops);
    let mut refs: Vec<ExemplarRef> = Vec::with_capacity(cfg.hops);
    let mut seals: Vec<SnapId> = Vec::with_capacity(cfg.hops);
    let mut cur = genesis;
    let mut cur_at = genesis_at;
    let mut entry_env: Option<Environment> = None;
    for i in 0..cfg.hops {
        machine.branch(cur, &codec.seeded(cfg.seed))?;
        let requested = cur_at.saturating_add(cfg.hop_delta);
        let landed = run_to(machine, requested, &format!("chain hop {i}"))?;
        let (seal, at, attempts) = seal_here(
            machine,
            landed,
            cfg.snapshot_retry_step,
            cfg.snapshot_max_attempts,
        )?;
        let suffix = machine.recorded_env()?;
        let env = match &entry_env {
            None => suffix.clone(),
            Some(base) => codec.compose(base, &suffix),
        };
        let r = frontier.insert(FrontierEntry {
            exemplar: VirtualExemplar {
                parent: cur,
                seed: cfg.seed,
                suffix: suffix.clone(),
                at: Moment(at),
            },
            env: env.clone(),
            reward: Reward { new_cells: 1 },
        });
        frontier.claim((i as u64).to_le_bytes().to_vec(), r);
        let displaced = mat.register(r, seal, cur, suffix, Moment(at));
        debug_assert!(displaced.is_none(), "fresh refs never carry a seal");
        hops.push(HopRow {
            requested,
            at,
            attempts,
        });
        refs.push(r);
        seals.push(seal);
        entry_env = Some(env);
        cur = seal;
        cur_at = at;
    }
    let deep = refs[cfg.hops - 1];
    let deep_env = entry_env.expect("hops >= 3");

    // 2b. Per-hop draw probes (task 78), while every hop's parent seal is
    //     still live: re-run each hop window plainly and with a trailing
    //     reseed marker back to the same seed at the landed boundary — the
    //     hashes differ iff a draw inside the window moved the stream (the
    //     same self-normalizing probe as the tail's, step 6b).
    let mut hop_draws: Vec<bool> = Vec::with_capacity(cfg.hops);
    {
        let mut parent = genesis;
        let mut parent_at = genesis_at;
        for (i, h) in hops.iter().enumerate() {
            machine.branch(parent, &codec.seeded(cfg.seed))?;
            let landed = run_to(machine, h.at, &format!("hop {i} plain probe"))?;
            let h_plain = machine.hash()?;
            machine.branch(parent, &reseed_probe_env(cfg.seed, parent_at, h.at)?)?;
            run_to(machine, h.at, &format!("hop {i} marker probe"))?;
            hop_draws.push(machine.hash()? != h_plain);
            debug_assert_eq!(landed, h.at, "the hop leg is deterministic");
            parent = seals[i];
            parent_at = h.at;
        }
    }

    // 3. Gate (a): evict the deep exemplar's own (eager) seal and materialize
    //    — the hot, parent-rooted, suffix-only replay.
    mat.evict_seal(machine, deep)?;
    let (_, hot) = mat.materialize(machine, codec, &frontier, deep)?;
    let hot = hot.expect("the deep seal was evicted, so a real replay ran");
    let hot_hash = machine.hash()?;

    // 4. Gate (b): evict the fresh seal AND the retained ancestor (the direct
    //    parent), re-materialize — the compose-folded, deeper replay.
    mat.evict_seal(machine, deep)?;
    mat.evict_seal(machine, refs[cfg.hops - 2])?;
    let (_, folded) = mat.materialize(machine, codec, &frontier, deep)?;
    let folded = folded.expect("the deep seal was evicted again");
    let folded_hash = machine.hash()?;

    // 5. The graceful worst case: evict everything, re-materialize from the
    //    base via the memoized genesis-complete env.
    mat.evict_all(machine)?;
    let (deep_seal, worst) = mat.materialize(machine, codec, &frontier, deep)?;
    let worst = worst.expect("every seal was evicted");
    let worst_hash = machine.hash()?;

    // 6. Gate (c): the "bug" leg below the (≥ 2-deep) chain, and its
    //    compose-folded reproducer replayed from the base. Both legs run to
    //    the same absolute deadline, so a deterministic substrate stops both
    //    at the same boundary.
    machine.branch(deep_seal, &codec.seeded(cfg.seed))?;
    let tail_deadline = cur_at.saturating_add(cfg.tail_delta);
    let leg_stop = machine.run(
        &StopConditions {
            deadline: Some(VTime(tail_deadline)),
            on: StopMask::NONE,
        },
        None,
    )?;
    let leg_hash = machine.hash()?;
    let delta = machine.recorded_env()?;
    // Exactly how `Explorer::report` mints a Bug.env: the branch-local delta
    // composed onto the entry's genesis-complete env. No SnapId anywhere.
    let bug_env = codec.compose(&deep_env, &delta);
    machine.branch(genesis, &bug_env)?;
    let replay_stop = machine.run(
        &StopConditions {
            deadline: Some(VTime(tail_deadline)),
            on: StopMask::NONE,
        },
        None,
    )?;
    let replay_hash = machine.hash()?;

    // 6b. The draw probe (task 78): the tail leg again under the SAME branch
    //     seed, plus a trailing reseed marker back to that seed at the landing
    //     boundary. The guest executes identically (same seed ⇒ same drawn
    //     values), so the probe hash differs from `leg_hash` exactly when the
    //     trailing reseed was NOT a no-op — i.e. when a draw inside the window
    //     moved the stream off its reseed point.
    //
    //     **Deadline-stopped tails only (PR #62 round-4 blocking fix).** For a
    //     Quiescent/Crash tail the probe's `deadline = landing` stops BEFORE
    //     consuming the terminal exit, so the hashes would differ from the
    //     skipped terminal state, not from any draw — a false positive. A
    //     terminal tail reports `tail_draws = false` (draws-unknown; the
    //     per-hop probes are unaffected — their legs are Deadline stops by
    //     construction).
    let tail_draws = if matches!(leg_stop, StopReason::Deadline { .. }) {
        let landing = leg_stop.vtime().0;
        let deep_at = cur_at;
        machine.branch(deep_seal, &reseed_probe_env(cfg.seed, deep_at, landing)?)?;
        let probe_stop = machine.run(
            &StopConditions {
                deadline: Some(VTime(landing)),
                on: StopMask::NONE,
            },
            None,
        )?;
        debug_assert_eq!(
            probe_stop.vtime().0,
            landing,
            "timing is draw-value-independent"
        );
        machine.hash()? != leg_hash
    } else {
        false
    };

    // 7. Cleanup: release every seal and the base (corpus GC over the wire).
    mat.evict_all(machine)?;
    machine.drop_snap(genesis)?;

    Ok(MaterializeReport {
        genesis_at,
        genesis_attempts,
        hops,
        hot,
        hot_hash,
        folded,
        folded_hash,
        worst,
        worst_hash,
        leg_stop,
        leg_hash,
        bug_env,
        replay_stop,
        replay_hash,
        hop_draws,
        tail_draws,
    })
}

/// The draw-probe env (task 78): branch-reseed to `seed` at the window origin
/// plus a trailing reseed back to `seed` at the landed boundary — a no-op iff
/// no draw inside `[origin, landed]` moved the stream, so the probe leg's hash
/// equals the plain leg's exactly when the window is draw-free.
///
/// `landed` and `origin` arrive from the Machine/transport boundary, so the
/// relative-key subtraction **fails closed** (PR #62 round-1 blocking fix): a
/// stop vtime below the branch origin is a broken monotonicity invariant —
/// a loud [`MachineError::Transport`], never a debug panic or a release wrap
/// that would encode a marker near `u64::MAX`.
fn reseed_probe_env(seed: u64, origin: u64, landed: u64) -> Result<Environment, MachineError> {
    let rel = landed.checked_sub(origin).ok_or_else(|| {
        MachineError::Transport(format!(
            "draw probe: landed vtime {landed} is BELOW the branch origin {origin} — the \
             machine reported a non-monotone stop; refusing to mint a wrapped reseed marker"
        ))
    })?;
    let mut spec = environment::EnvSpec::Seeded {
        seed,
        policy: environment::FaultPolicy::none(),
    };
    spec.record_reseed(0, seed);
    spec.record_reseed(rel, seed);
    Ok(explorer::AdapterEnv {
        base_offset: origin,
        pos: origin,
        spec,
    }
    .encode())
}

/// Integer parts-per-million of `num / den` (`den == 0` reports 0).
fn ppm(num: u64, den: u64) -> u64 {
    num.saturating_mul(1_000_000).checked_div(den).unwrap_or(0)
}

/// The depth ratio gate (a) quotes: the issued replay depth against a **full
/// from-scratch re-execution** to the same absolute V-time — the same
/// definition as the task-63 §4 baseline (its from-genesis leg boots from
/// V-time 0), which is the cost the archive's virtual exemplars avoid.
pub fn depth_ratio_ppm(m: &Materialization) -> u64 {
    ppm(m.depth(), m.at.0)
}

/// The sequential-entropy-splice diagnostic appended to a round-trip hash
/// mismatch. Task 78 made the fold reseed-aware (the env stores each hop's
/// reseed marker and the server re-executes it at its recorded Moment), so a
/// mismatch now points at a defect in that chain, not a documented limit.
const SPLICE_DIAGNOSTIC: &str = "SUSPECT the sequential-entropy splice machinery (task 78): every \
     hop's branch reseed is recorded as a reseed marker, compose splices markers positionally, \
     and the server re-executes each collapsed hop's reseed at its recorded Moment — a mismatch \
     means a marker was lost, mis-spliced, mis-anchored, or applied at the wrong count.";

/// The task-68 gates over a [`MaterializeReport`]:
///
/// 1. **Grid keying** — every hop landed at/after its requested deadline and
///    was sealed exactly where it landed.
/// 2. **Gate (a), hot path** — the deep exemplar materialized from its
///    **direct parent** (suffix-only: depth = its own hop, zero folds, never
///    genesis); with `baseline_ppm` set (the box), its depth ratio beats it.
/// 3. **Gate (b), eviction round-trip** — the folded re-materialization is a
///    real deeper replay (parent-of-parent base, one fold) and its hash is
///    **bit-identical**; the all-evicted worst case replays from the base
///    (`from_genesis`, full pool depth) still bit-identically. Depths degrade
///    monotonically (hot < folded < worst).
/// 4. **Gate (c), composed reproducer** — the tail leg below the ≥ 2-deep
///    chain and its compose-folded `bug_env` replay from the base with an
///    identical stop **and** hash.
///
/// Returns every violated gate (empty = all pass).
pub fn verify_materialize(r: &MaterializeReport, baseline_ppm: Option<u64>) -> Vec<String> {
    let mut failures = Vec::new();
    let n = r.hops.len();

    // 0. Chain depth. The gates below index the parent (n−2) and grandparent
    //    (n−3) hops, and gate (b)'s fold is only meaningful with a retained
    //    non-genesis ancestor above the evicted parent — so a short chain is
    //    a verification FAILURE, never a panic (this is a public, total
    //    function over an arbitrary report; conventions rule 4).
    if n < 3 {
        failures.push(format!(
            "report carries only {n} hop(s) — the chain gates need >= 3 (gate (b)'s fold must \
             collapse a real intermediate below a retained non-genesis ancestor); nothing to \
             verify"
        ));
        return failures;
    }

    // 1. Grid keying.
    for (i, h) in r.hops.iter().enumerate() {
        if h.at < h.requested {
            failures.push(format!(
                "hop {i}: landed at {} BEFORE the requested deadline {} — not a boundary at/after \
                 the target",
                h.at, h.requested
            ));
        }
    }

    // 2. Gate (a): parent-rooted, suffix-only.
    let parent_at = r.hops[n - 2].at;
    let deep_at = r.hops[n - 1].at;
    if r.hot.from_genesis {
        failures.push(
            "gate (a): the hot materialization replayed GENESIS — a defect, not a slow \
             path (the direct parent was retained)"
                .into(),
        );
    }
    if r.hot.base_at.0 != parent_at || r.hot.at.0 != deep_at {
        failures.push(format!(
            "gate (a): hot replay spanned {}..{} — expected the direct parent's suffix \
             {parent_at}..{deep_at}",
            r.hot.base_at.0, r.hot.at.0
        ));
    }
    if r.hot.folded != 0 {
        failures.push(format!(
            "gate (a): the hot path folded {} suffixes — the direct parent needs none",
            r.hot.folded
        ));
    }
    if let Some(baseline) = baseline_ppm {
        let measured = depth_ratio_ppm(&r.hot);
        if measured >= baseline {
            failures.push(format!(
                "gate (a): measured depth ratio {measured} ppm does not beat the task-63 §4 \
                 baseline {baseline} ppm (suffix {} of a {}-deep full re-execution)",
                r.hot.depth(),
                r.hot.at.0
            ));
        }
    }

    // 3. Gate (b): the folded re-materialization + the worst case.
    let grandparent_at = r.hops[n - 3].at;
    if r.folded.from_genesis {
        failures.push(
            "gate (b): the folded re-materialization replayed GENESIS — the \
             grandparent was still retained"
                .into(),
        );
    }
    if r.folded.base_at.0 != grandparent_at || r.folded.folded != 1 {
        failures.push(format!(
            "gate (b): the fold based at {} with {} folds — expected the grandparent \
             {grandparent_at} collapsing exactly the evicted parent (1 fold)",
            r.folded.base_at.0, r.folded.folded
        ));
    }
    if r.folded_hash != r.hot_hash {
        failures.push(format!(
            "gate (b): folded re-materialization hash {} != hot hash {} — NOT bit-identical. {}",
            hex(&r.folded_hash),
            hex(&r.hot_hash),
            SPLICE_DIAGNOSTIC
        ));
    }
    if !r.worst.from_genesis || r.worst.base_at.0 != r.genesis_at {
        failures.push(format!(
            "worst case: expected a from-genesis replay based at {}, got base {} \
             (from_genesis={})",
            r.genesis_at, r.worst.base_at.0, r.worst.from_genesis
        ));
    }
    if r.worst_hash != r.hot_hash {
        failures.push(format!(
            "gate (b) worst case: from-genesis re-materialization hash {} != hot hash {} — NOT \
             bit-identical. {}",
            hex(&r.worst_hash),
            hex(&r.hot_hash),
            SPLICE_DIAGNOSTIC
        ));
    }
    if !(r.hot.depth() < r.folded.depth() && r.folded.depth() < r.worst.depth()) {
        failures.push(format!(
            "degradation is not monotone: hot {} < folded {} < worst {} violated",
            r.hot.depth(),
            r.folded.depth(),
            r.worst.depth()
        ));
    }

    // 4. Gate (c): the composed reproducer.
    if r.replay_stop != r.leg_stop {
        failures.push(format!(
            "gate (c): replay stop {} != leg stop {} — the composed reproducer does not \
             reproduce the run",
            fmt_stop(&r.replay_stop),
            fmt_stop(&r.leg_stop)
        ));
    }
    if r.replay_hash != r.leg_hash {
        failures.push(format!(
            "gate (c): replay hash {} != leg hash {} — the composed reproducer does not replay \
             bit-identically. {}",
            hex(&r.replay_hash),
            hex(&r.leg_hash),
            SPLICE_DIAGNOSTIC
        ));
    }

    failures
}

/// Render the chain/materialization table (the artifact the box gate records).
pub fn render_materialize_table(r: &MaterializeReport) -> String {
    let mut out = String::new();
    let mut push = |line: String| {
        out.push_str(&line);
        out.push('\n');
    };
    push(format!(
        "base: sealed at V-time {} ({} attempt{})",
        r.genesis_at,
        r.genesis_attempts,
        if r.genesis_attempts == 1 { "" } else { "s" }
    ));
    push(format!(
        "{:<6} {:>14} {:>14} {:>10} {:>9}",
        "hop", "requested", "landed(at)", "overshoot", "attempts"
    ));
    for (i, h) in r.hops.iter().enumerate() {
        push(format!(
            "{:<6} {:>14} {:>14} {:>10} {:>9}",
            i,
            h.requested,
            h.at,
            h.at.saturating_sub(h.requested),
            h.attempts
        ));
    }
    let leg = |name: &str, m: &Materialization, hash: &[u8; 32]| {
        format!(
            "{name:<7} base_at {:>14} → at {:>14}  depth {:>12}  ratio {:>6} ppm  folds {}  \
             from_genesis {:<5}  state_hash {}",
            m.base_at.0,
            m.at.0,
            m.depth(),
            depth_ratio_ppm(m),
            m.folded,
            m.from_genesis,
            hex(hash)
        )
    };
    push(leg("hot", &r.hot, &r.hot_hash));
    push(leg("folded", &r.folded, &r.folded_hash));
    push(leg("worst", &r.worst, &r.worst_hash));
    push(format!(
        "round-trip: folded {} hot, worst {} hot",
        if r.folded_hash == r.hot_hash {
            "=="
        } else {
            "!="
        },
        if r.worst_hash == r.hot_hash {
            "=="
        } else {
            "!="
        }
    ));
    push(format!(
        "reproducer: leg    {:<24} state_hash {}",
        fmt_stop(&r.leg_stop),
        hex(&r.leg_hash)
    ));
    push(format!(
        "reproducer: replay {:<24} state_hash {} ({} leg; bug_env {} bytes, genesis-complete)",
        fmt_stop(&r.replay_stop),
        hex(&r.replay_hash),
        if r.replay_hash == r.leg_hash && r.replay_stop == r.leg_stop {
            "=="
        } else {
            "!="
        },
        r.bug_env.bytes.len()
    ));
    push(format!(
        "baseline: task-63 §4 = {TASK63_BASELINE_PPM} ppm (1.5463%); measured hot = {} ppm",
        depth_ratio_ppm(&r.hot)
    ));
    push(format!(
        "draw probes (task 78): hops {:?}; tail window {} entropy (trailing-reseed probe)",
        r.hop_draws,
        if r.tail_draws {
            "DRAWS"
        } else {
            "does NOT draw"
        }
    ));
    out
}

#[cfg(test)]
mod tests {
    use explorer::{Environment, Materialization, Moment, SnapId, StopReason, VTime};

    use super::*;

    /// A syntactically-complete report with `hops` chain rows (contents
    /// synthetic — only the shape matters to the short-chain guard).
    fn report(hops: usize) -> MaterializeReport {
        let m = Materialization {
            base: SnapId(1),
            base_at: Moment(0),
            at: Moment(10),
            folded: 0,
            from_genesis: false,
        };
        MaterializeReport {
            genesis_at: 0,
            genesis_attempts: 1,
            hops: (0..hops)
                .map(|i| HopRow {
                    requested: i as u64 * 10,
                    at: i as u64 * 10,
                    attempts: 1,
                })
                .collect(),
            hot: m,
            hot_hash: [0; 32],
            folded: m,
            folded_hash: [0; 32],
            worst: m,
            worst_hash: [0; 32],
            leg_stop: StopReason::Deadline { vtime: VTime(20) },
            leg_hash: [0; 32],
            bug_env: Environment {
                blob_version: 1,
                bytes: Vec::new(),
            },
            replay_stop: StopReason::Deadline { vtime: VTime(20) },
            replay_hash: [0; 32],
            hop_draws: Vec::new(),
            tail_draws: false,
        }
    }

    /// The draw probe's relative-key subtraction fails closed (PR #62 round-1
    /// blocking fix): a landed vtime below the branch origin — a non-monotone
    /// stop from the transport — is a loud `MachineError`, never a wrapped
    /// marker near `u64::MAX`.
    #[test]
    fn reseed_probe_env_rejects_a_landed_vtime_below_the_origin() {
        let err = reseed_probe_env(7, 100, 50).expect_err("landed < origin must fail closed");
        assert!(
            matches!(err, explorer::MachineError::Transport(_)),
            "fails as a transport-invariant error, got {err:?}"
        );
        // The boundary case is fine: an empty window keys both markers at 0
        // (last write wins — one marker), still a valid probe env.
        let env = reseed_probe_env(7, 100, 100).expect("landed == origin is a valid empty window");
        let decoded = explorer::AdapterEnv::decode(&env).expect("adapter blob");
        assert_eq!(decoded.spec.reseeds().len(), 1);
    }

    /// `verify_materialize` is a total, public function over an arbitrary
    /// report (conventions rule 4): a chain shorter than the 3 hops the gates
    /// index is a verification FAILURE, never a panic (PR #58 round-2 fix —
    /// the parent/grandparent rows were read unguarded).
    #[test]
    fn verify_rejects_a_short_chain_without_panicking() {
        for n in 0..3 {
            let failures = verify_materialize(&report(n), Some(TASK63_BASELINE_PPM));
            assert_eq!(
                failures.len(),
                1,
                "a {n}-hop report fails the depth check alone (and reaches no indexing)"
            );
            assert!(
                failures[0].contains("need >= 3"),
                "a {n}-hop report must name the depth failure, got {failures:?}"
            );
        }
        // A 3-hop report passes the depth guard and reaches the real gates
        // (whose synthetic-value failures are the proof it got there).
        let failures = verify_materialize(&report(3), None);
        assert!(!failures.iter().any(|f| f.contains("need >= 3")));
        assert!(
            !failures.is_empty(),
            "the synthetic report reaches (and fails) the substantive gates"
        );
    }

    /// A fully-valid 3-hop report: every gate in [`verify_materialize`] passes
    /// (so `failures` is empty). V-times are large enough that the hot suffix
    /// ratio (1%) beats the task-63 §4 baseline. Reused to exercise the gate
    /// pass-branches and the table renderer.
    fn valid_report() -> MaterializeReport {
        let mat = |base_at: u64, at: u64, folded: u64, from_genesis: bool| Materialization {
            base: SnapId(1),
            base_at: Moment(base_at),
            at: Moment(at),
            folded,
            from_genesis,
        };
        MaterializeReport {
            genesis_at: 0,
            genesis_attempts: 2,
            // grandparent 98e6, parent 99e6, deep 100e6.
            hops: [98_000_000u64, 99_000_000, 100_000_000]
                .iter()
                .map(|&at| HopRow {
                    requested: at,
                    at,
                    attempts: 1,
                })
                .collect(),
            // hot: direct-parent suffix 99e6..100e6 (depth 1e6, ratio 10_000 ppm).
            hot: mat(99_000_000, 100_000_000, 0, false),
            hot_hash: [7; 32],
            // folded: grandparent-rooted, exactly one fold (depth 2e6).
            folded: mat(98_000_000, 100_000_000, 1, false),
            folded_hash: [7; 32],
            // worst: from-genesis (depth 100e6).
            worst: mat(0, 100_000_000, 0, true),
            worst_hash: [7; 32],
            leg_stop: StopReason::Deadline { vtime: VTime(100) },
            leg_hash: [9; 32],
            bug_env: Environment {
                blob_version: 1,
                bytes: vec![1, 2, 3],
            },
            replay_stop: StopReason::Deadline { vtime: VTime(100) },
            replay_hash: [9; 32],
            hop_draws: vec![true],
            tail_draws: true,
        }
    }

    /// A valid report clears every gate (including the baseline ratio gate) and
    /// renders a table naming the round-trip equalities. Covers the pass-branch
    /// of each gate and the whole renderer.
    #[test]
    fn valid_report_passes_all_gates_and_renders() {
        let r = valid_report();
        assert!(
            verify_materialize(&r, Some(TASK63_BASELINE_PPM)).is_empty(),
            "a fully-valid report clears every gate: {:?}",
            verify_materialize(&r, Some(TASK63_BASELINE_PPM))
        );
        assert!(verify_materialize(&r, None).is_empty());

        let table = render_materialize_table(&r);
        assert!(table.contains("2 attempts"));
        assert!(table.contains("round-trip: folded == hot, worst == hot"));
        assert!(table.contains("measured hot = 10000 ppm"));
        assert!(table.contains("hops [true]; tail window DRAWS"));
    }

    /// A report tripping every gate failure yields the expected diagnostics.
    /// Covers each failure-push arm and the `!=` table branches.
    #[test]
    fn every_gate_failure_is_reported() {
        let bad = |base_at: u64, at: u64, folded: u64, from_genesis: bool| Materialization {
            base: SnapId(1),
            base_at: Moment(base_at),
            at: Moment(at),
            folded,
            from_genesis,
        };
        let mut r = valid_report();
        // hop 0 lands before its requested deadline (grid-keying failure).
        r.hops[0].requested = r.hops[0].at + 5;
        // gate (a): hot replayed genesis, wrong span, non-zero folds, and its
        // ratio (now 50%) does not beat the baseline.
        r.hot = bad(50_000_000, 100_000_000, 3, true);
        // gate (b): folded replayed genesis, wrong base/fold-count, hash mismatch.
        r.folded = bad(0, 100_000_000, 9, true);
        r.folded_hash = [1; 32];
        // worst not from-genesis + hash mismatch.
        r.worst = bad(5, 100_000_000, 0, false);
        r.worst_hash = [2; 32];
        // degradation no longer monotone (hot depth now 50e6 == folded 100e6? make
        // hot deeper than folded to break the chain).
        // gate (c): reproducer disagrees on stop and hash.
        r.replay_stop = StopReason::Deadline { vtime: VTime(999) };
        r.replay_hash = [3; 32];

        let f = verify_materialize(&r, Some(TASK63_BASELINE_PPM));
        assert!(
            f.iter()
                .any(|m| m.contains("BEFORE the requested deadline"))
        );
        assert!(f.iter().any(|m| m.contains("replayed GENESIS")));
        assert!(f.iter().any(|m| m.contains("does not beat the task-63")));
        assert!(f.iter().any(|m| m.contains("NOT bit-identical")));
        assert!(f.iter().any(|m| m.contains("worst case")));
        assert!(f.iter().any(|m| m.contains("does not reproduce the run")));

        // The renderer flags the broken round-trips.
        let table = render_materialize_table(&r);
        assert!(table.contains("round-trip: folded != hot, worst != hot"));
    }

    /// `depth_ratio_ppm` pins an EXACT parts-per-million value: `depth / at` in
    /// millionths, `at == 0` reporting 0. A known input → known ppm nails down
    /// the integer arithmetic (so a mutated `ppm` that returns a constant 0 or 1
    /// cannot survive).
    #[test]
    fn depth_ratio_ppm_is_exact() {
        let m = |base_at: u64, at: u64| Materialization {
            base: SnapId(1),
            base_at: Moment(base_at),
            at: Moment(at),
            folded: 0,
            from_genesis: false,
        };
        // depth = 10 - 2 = 8, den = 10 ⇒ 8 * 1_000_000 / 10 = 800_000.
        assert_eq!(depth_ratio_ppm(&m(2, 10)), 800_000);
        // depth == den ⇒ a full 1_000_000 (100%).
        assert_eq!(depth_ratio_ppm(&m(0, 7)), 1_000_000);
        // A zero denominator (`at == 0`) reports 0, never divides.
        assert_eq!(depth_ratio_ppm(&m(0, 0)), 0);
    }
}
