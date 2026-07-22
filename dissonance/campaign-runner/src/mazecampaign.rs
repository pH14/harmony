// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task 134 (`hm-cs5`) — the **maze** campaign driver: the first cooperative
//! Differential exploration gate, **quiet arm**.
//!
//! Drives the deterministic `maze` gauntlet workload end-to-end through the
//! generic Explorer's two-barrier [`DifferentialCampaign`] controller, with
//! **zero fault vocabulary** (every branch env is a pure seeded environment;
//! exploit mutations are the task-78 reseed-only [`quiet_mutate`] move). The
//! only thing the search varies is the guest's decision entropy — the task-84
//! `quiet` arm on the rewritten Differential plane.
//!
//! Cells are keyed **host-side** (R-L2): the workload emits raw bounded
//! integer X/Y state registers — **both at the same `Moment`** each walk step
//! — and [`MazeObservationCells`] projects the independently-reduced
//! registers at each evaluation point's **server-stamped cut** into the
//! `(x, y)` tile key. Same-`Moment` simultaneity is carried by the cut's
//! half-open prefix **count**, never by `Moment` comparison (task 127); the
//! archive occupies best-Entry-per-cell at the **actual** `sealed_at`
//! (barrier 2), with full retention from rollout one (`hm-5sv`).
//!
//! The wire-v2 discipline: the strategy forbids blessing v1 firings with
//! inferred reducers, so [`MazeDeclaredMachine`] carries the workload's
//! **explicit instrumentation declaration** — a wire-v2 catalog resolving
//! X/Y to `set` state and the goal marker to a `MustHit` occurrence. A guest
//! that declared its own (v1) catalog is upgraded in place
//! ([`sdk_events::resolve_v1_declaration`]); the portable toy (no catalog)
//! has the standalone declaration prepended, with the seal cut stamped
//! catalog-inclusive (the task-132 `DeclaredMachine` pattern, verbatim).
//!
//! Three ruled configurations, identical branch budget (task 84):
//! [`ExplorationConfig::SelectorV1`] — the **subject**, the simple
//! archive-guided explore/exploit selector; [`ExplorationConfig::PureRandom`]
//! — the pass/fail control, always-explore with the frontier held empty
//! (candidate cap and replay budget zeroed: no materialization at all);
//! [`ExplorationConfig::FrontierOff`] — the diagnostic control, the full
//! materialization machinery with an always-explore selector. On the pure
//! in-process toy the frontier-off log is expected to equal the pure-random
//! log exactly (the controller draws no campaign randomness for
//! materialization) — the machinery-neutrality tripwire; on a live backend
//! any divergence between the two is a determinism finding.
//!
//! Everything is a pure function of `(campaign_seed, machine, config)`; the
//! box determinism gate reruns a campaign and requires the identical
//! per-branch `state_hash` sequence 25/25.

use std::collections::BTreeMap;

use benchmark::exploration::{DiscoveryEvent, ExplorationConfig, ExplorationLog};
use explorer::{
    CampaignError, CellKey, DeclineTactic, DifferentialCampaign, EnvCodec, EvidenceCut,
    EvidenceLedger, ExploreExploitSelector, GenesisSelector, Ingress, Machine, MachineError,
    Moment, Nomination, ObservationCells, ObservationMap, ReducedValue, Reproducer,
    RetentionProfile, RunTrace, Selector, StopConditions, StopMask, StopReason,
};
use maze::{MazeSpec, MazeState};
use revision_coordinator::{CampaignConfigId, Coordinator, MemLedger};
use sdk_events::{NS_ASSERT, NS_STATE, Normalized, ObservationId, Payload, SdkEvent};

use crate::gamecampaign::QuietCodec;

/// The maze workload's state-register catalog (mirrored by the maze guest
/// agent under `harmony-linux/`, outside this workspace — the conventions
/// mirror-type pattern; both sides derive the walk from the shared `maze`
/// crate, so the values cannot drift in meaning).
pub mod reg {
    /// The walker's X register (`MazeState::x_register`): the corridor
    /// position (0 at the goal tile).
    pub const X: u64 = 1;
    /// The walker's Y register (`MazeState::y_register`): the level, or
    /// `levels` at the goal.
    pub const Y: u64 = 2;
}

/// The goal marker's `NS_ASSERT` local id: a `MustHit` (reachable) occurrence
/// point — task 84's permitted legibility marker, never a bug. Fired once,
/// at the step that first reaches the goal tile.
pub const GOAL_LOCAL: u32 = 1;

/// The binary-wire catalog marker event id (mirror of the sdk-events wire
/// constant; the conventions mirror-type pattern).
const CATALOG_EVENT_ID: u32 = 0;

/// The maze campaign's budget + search knobs. The campaign is a pure function
/// of these plus the machine.
#[derive(Clone, Debug)]
pub struct MazeCampaignConfig {
    /// Seeds the campaign stream.
    pub campaign_seed: u64,
    /// The maze shape (the workload manifest; fixed across campaign seeds).
    pub spec: MazeSpec,
    /// Search budget: exactly this many branches (identical across
    /// configurations — task 84's ruling).
    pub max_branches: u64,
    /// Walk steps per rollout (the rollout's natural terminal on the toy; a
    /// live guest walks until its V-time deadline).
    pub steps_per_rollout: u32,
    /// Each rollout runs this much V-time past its branch point, if set (the
    /// live-guest rollout terminal). `None` = the toy's natural terminal.
    pub deadline_delta: Option<u64>,
    /// SelectorV1 only: every Nth step explores fresh from genesis; the rest
    /// exploit a retained Entry.
    pub explore_period: u64,
    /// The two-barrier per-step materialization cap (zeroed internally for
    /// the PureRandom frontier-held-empty control).
    pub candidate_cap: usize,
    /// The controller's campaign-total materialization-replay budget (zeroed
    /// internally for PureRandom).
    pub replay_budget: u64,
    /// The V-time allowance for a live guest to reach its `setup_complete`
    /// snapshot point from boot (the base-seal boundary).
    pub setup_deadline_delta: u64,
    /// Base-seal retry step past a non-snapshottable boundary (the toy path's
    /// generic fallback; a live base seals at the snapshot point).
    pub snapshot_retry_step: u64,
    /// Base-seal retry budget.
    pub snapshot_max_attempts: usize,
    /// Whether the base MUST seal at the guest's `setup_complete` snapshot
    /// point (the box/real path: a guest that never reaches it is a dead
    /// agent — refuse loudly, never seal a dead base) and every rollout must
    /// end at its deadline (a mid-rollout death fails the campaign loudly).
    /// `false` only for the portable toy.
    pub require_snapshot_point: bool,
    /// Record the deepest branch's reproducer (env + full journal) into this
    /// task-65 `TraceStore` directory, and keep the durable evidence ledger
    /// beside it. `None` = campaign-lifetime scratch (portable smoke loops).
    pub trace_dir: Option<std::path::PathBuf>,
}

impl MazeCampaignConfig {
    /// A small portable configuration over [`MazeSpec::small`]: budgets sized
    /// so the archive-guided configuration can reach the goal while random
    /// restart plateaus (the maze crate's measured property).
    pub fn smoke(campaign_seed: u64) -> Self {
        MazeCampaignConfig {
            campaign_seed,
            spec: MazeSpec::small(),
            max_branches: 48,
            steps_per_rollout: 48,
            deadline_delta: None,
            explore_period: 4,
            candidate_cap: 2,
            replay_budget: 96,
            setup_deadline_delta: 1_000,
            snapshot_retry_step: 100,
            snapshot_max_attempts: 100,
            require_snapshot_point: false,
            trace_dir: None,
        }
    }
}

/// Why a maze campaign refused to run or died.
#[derive(Debug, thiserror::Error)]
pub enum MazeCampaignError {
    /// A machine (transport/backend) failure.
    #[error(transparent)]
    Machine(#[from] MachineError),
    /// The two-barrier Differential controller failed — loud, never absorbed.
    #[error("differential campaign: {0}")]
    Campaign(#[from] CampaignError),
    /// The Signal configuration was requested, but the maze gate's subject is
    /// the simple archive-guided selector ([`ExplorationConfig::SelectorV1`]);
    /// an advanced-selector artifact does not exist (advanced selection is
    /// downstream of this gate — `docs/DISSONANCE-STRATEGY.md`).
    #[error(
        "the Signal configuration needs an advanced-selector artifact, which does not exist; \
         the maze gate runs SelectorV1 (subject) / PureRandom / FrontierOff only"
    )]
    SignalUnavailable,
    /// The live guest never surfaced its `setup_complete` snapshot point — a
    /// dead maze agent; sealing wherever it died would record a
    /// zero/constant-cell campaign (the vacuity class).
    #[error(
        "the maze agent never reached setup_complete (guest stopped with {stop}) — bad \
         provisioning?; refusing to seal a dead base (the campaign would be vacuous)"
    )]
    SetupNotReached {
        /// The stop the guest surfaced instead of the snapshot point.
        stop: String,
    },
    /// A box-mode rollout ended somewhere other than its deadline — the maze
    /// agent DIED mid-rollout; crashed rollouts must never be recorded as
    /// ordinary samples.
    #[error(
        "rollout {branch} died: guest stopped with {stop} instead of its deadline — refusing \
         to record a dead rollout as a sample"
    )]
    RolloutDied {
        /// The branch index that died.
        branch: u64,
        /// The terminal it surfaced.
        stop: String,
    },
    /// The deep-reproducer retention failed (trace-store I/O) — loud, never a
    /// campaign that silently produced un-rekeyable output.
    #[error("deep-reproducer retention failed: {0}")]
    Retention(String),
}

/// The deepest branch's retained reproducer pointer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MazeDeepReproducer {
    /// The deepest branch's index (first-deepest on ties).
    pub branch: u64,
    /// Its depth (the max Y register observed).
    pub depth: u64,
    /// The recorded trace id, when retention was configured.
    pub trace_id: Option<String>,
}

/// What a campaign has to show for itself (the vacuity guard's evidence):
/// minima, not totals — one hollow rollout cannot hide behind busy ones.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MazeWorkEvidence {
    /// Branches actually rolled out.
    pub branches: u64,
    /// The least V-time any rollout advanced past its branch point.
    pub min_vtime_span: u64,
    /// The least number of walk steps any rollout took (its own Y-register
    /// observations — one per step by construction).
    pub min_steps: u64,
}

/// Why a campaign's evidence does not support any gate verdict.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum MazeVacuity {
    /// Zero branches ran.
    #[error("the campaign rolled out ZERO branches — nothing was measured")]
    NoBranches,
    /// A rollout advanced no V-time past its branch point.
    #[error("a rollout advanced ZERO V-time past its branch point — the guest never ran")]
    NoVTime,
    /// A rollout took no walk step (no Y-register observation of its own).
    #[error(
        "a rollout took ZERO walk steps (no Y-register observation) — the guest ran but the \
         workload never advanced"
    )]
    NoSteps,
}

/// What a maze campaign produced: the offline-analysis log, the retained deep
/// reproducer, the goal/expectation evidence, and the work evidence the gate
/// checks before it may print a verdict.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MazeCampaignOutcome {
    /// The discovery-event log (the offline report's input).
    pub log: ExplorationLog,
    /// The deepest branch (present whenever at least one branch ran).
    pub deep: Option<MazeDeepReproducer>,
    /// Total goal-marker hits across the campaign's committed evidence (the
    /// held progress evidence; 0 when the goal was never reached).
    pub goal_hits: u64,
    /// The finalized absence view's still-open `MustHit` expectations at the
    /// campaign end: 1 while the goal was never reached (the held obligation).
    ///
    /// **Known spine finding (escalated as `hm-19l0`):** the view keys must-hit
    /// satisfaction on the wire-v1 assertion *verb* (`satisfies_must_hit` in
    /// `explorer::occurrence`), which a wire-v2 catalog deliberately does not
    /// carry — so a v2-declared `MustHit` hit never clears its absence and
    /// this stays 1 even after the goal is reached. [`Self::goal_hits`] (the
    /// host-side fold over the same committed evidence) is the authoritative
    /// progress witness until that seam is ruled/fixed.
    pub open_expectations: u64,
    /// Proof this campaign actually ran the workload.
    pub work: MazeWorkEvidence,
}

impl MazeCampaignOutcome {
    /// The gate's vacuity guard: `Some(reason)` when this campaign's evidence
    /// cannot support a verdict.
    pub fn vacuity(&self) -> Option<MazeVacuity> {
        if self.work.branches == 0 {
            return Some(MazeVacuity::NoBranches);
        }
        if self.work.min_vtime_span == 0 {
            return Some(MazeVacuity::NoVTime);
        }
        if self.work.min_steps == 0 {
            return Some(MazeVacuity::NoSteps);
        }
        None
    }
}

/// Pack the `(x, y)` tile into one 64-bit cell key. Injective for the maze's
/// bounded registers (both far below 2³²).
pub fn maze_cell_key(x: u64, y: u64) -> u64 {
    (y << 32) | (x & 0xFFFF_FFFF)
}

/// Fold a run's decoded SDK events into the branch's discovery record: the
/// **distinct** `(x, y)` cell keys it touched (sorted), the max Y register
/// (its depth), and the goal-marker hits it fired. The per-step emission
/// order is X then Y at one shared `Moment`, so the key is minted at each Y
/// observation from the last-seen X.
pub fn maze_cells(events: &[SdkEvent]) -> (Vec<u64>, u64, u64) {
    let mut cells = std::collections::BTreeSet::new();
    let mut depth = 0u64;
    let mut goal_hits = 0u64;
    let mut last_x = 0u64;
    for ev in events {
        match (&ev.id, &ev.payload) {
            (ObservationId::Point { namespace, local }, Payload::State { value, .. })
                if *namespace == NS_STATE =>
            {
                match u64::from(*local) {
                    reg::X => last_x = *value,
                    reg::Y => {
                        cells.insert(maze_cell_key(last_x, *value));
                        depth = depth.max(*value);
                    }
                    _ => {}
                }
            }
            (ObservationId::Point { namespace, local }, Payload::Assertion { condition, .. })
                if *namespace == NS_ASSERT && *local == GOAL_LOCAL && *condition == Some(true) =>
            {
                goal_hits += 1;
            }
            _ => {}
        }
    }
    (cells.into_iter().collect(), depth, goal_hits)
}

/// The maze cell projection over the reduced observation map: the same
/// `(x, y)` tile [`maze_cells`] keys, read off the independently-reduced X/Y
/// registers at the evaluation point's cut. A state with no Y observation yet
/// keys to the empty cell (the shared pre-progress state).
pub struct MazeObservationCells;

impl ObservationCells for MazeObservationCells {
    fn key(&self, _cut: EvidenceCut, obs: &ObservationMap) -> CellKey {
        let get = |local: u64| match obs.get(&ObservationId::Point {
            namespace: NS_STATE,
            local: local as u32,
        }) {
            Some(ReducedValue::Scalar(v)) => Some(*v),
            _ => None,
        };
        match (get(reg::X), get(reg::Y)) {
            (Some(x), Some(y)) => maze_cell_key(x, y).to_le_bytes().to_vec(),
            _ => Vec::new(),
        }
    }
}

/// The maze workload's base-operation resolution table: each state register
/// with its declared base update operation (both `set` — the registers report
/// current position; depth is derived host-side, never a guest register).
fn maze_resolution() -> Vec<(ObservationId, sdk_events::UpdateOp)> {
    use sdk_events::UpdateOp;
    let point = |local: u64| ObservationId::Point {
        namespace: NS_STATE,
        local: local as u32,
    };
    vec![
        (point(reg::X), UpdateOp::Set),
        (point(reg::Y), UpdateOp::Set),
    ]
}

/// The maze workload's **explicit instrumentation declaration**: a wire-v2
/// catalog resolving X/Y to reducible `set` state and declaring the goal
/// marker as a `MustHit` occurrence — so the normalized evidence is reducible
/// by the Differential relations with **no unresolved-v1 state semantics**
/// (the strategy forbids blessing v1 firings with inferred reducers; this is
/// its sanctioned "equally explicit workload instrumentation declaration").
fn maze_instrumentation_catalog() -> Vec<u8> {
    use sdk_events::{Classification, DeclaredPoint, Expectation, ValueShape};
    let mut points: Vec<DeclaredPoint> = maze_resolution()
        .into_iter()
        .filter_map(|(id, op)| match id {
            ObservationId::Point { namespace, local } => Some(DeclaredPoint {
                namespace,
                local,
                name: match u64::from(local) {
                    reg::X => "maze_x".to_string(),
                    reg::Y => "maze_y".to_string(),
                    other => format!("maze_reg_{other}"),
                },
                classification: Classification::State,
                value_shape: Some(ValueShape::U64),
                base_op: Some(op),
                expectation: None,
            }),
            _ => None,
        })
        .collect();
    points.push(DeclaredPoint {
        namespace: NS_ASSERT,
        local: GOAL_LOCAL,
        name: "maze_goal_reachable".to_string(),
        classification: Classification::Occurrence,
        value_shape: None,
        base_op: None,
        expectation: Some(Expectation::MustHit),
    });
    // Statically infallible: a fixed, well-formed catalog literal.
    sdk_events::encode_v2_declaration(&points)
        .expect("the maze instrumentation catalog is well-formed")
}

/// The goal marker's observation identity — the absence view's property key
/// (the M2 report reads the campaign's satisfied/absence state under it, and
/// the `hm-19l0` fix will be validated against it).
pub fn goal_id() -> ObservationId {
    ObservationId::Point {
        namespace: NS_ASSERT,
        local: GOAL_LOCAL,
    }
}

/// A [`Machine`] adapter carrying the maze instrumentation declaration (the
/// task-132 `DeclaredMachine` pattern, verbatim): a guest that declared its
/// own (wire-v1) catalog has it upgraded in place
/// ([`sdk_events::resolve_v1_declaration`]); a guest with no catalog (the
/// portable toy) has the standalone wire-v2 declaration prepended, with the
/// seal cut stamped catalog-inclusive so the cut counts the added ordinal.
pub struct MazeDeclaredMachine<M> {
    inner: M,
    catalog: Vec<u8>,
    /// Whether `sdk_events` prepends the standalone catalog (learned on the
    /// setup drain, before any campaign seal; catalog presence is a fixed
    /// property of the guest).
    prepends_catalog: Option<bool>,
    /// The per-branch **rolling-deadline** span budget (`deadline_delta`): each
    /// rollout runs exactly this much V-time past its *own* branch origin,
    /// rather than the leftover of a single campaign-wide absolute deadline —
    /// so an exploit branching off a late-sealed Entry gets a full span budget
    /// instead of a truncated (or zero-span, vacuous) one (`hm-qcpp`). The
    /// quiet-arm rollout is exactly the run whose stop conditions carry **no**
    /// deadline (`until.deadline == None`); every seal / probe / setup run
    /// names an explicit `Some` deadline and is forwarded verbatim, so the
    /// Option-C candidate-seal machinery is untouched. `None` = no rolling (the
    /// portable toy's natural terminal; a campaign with no live V-time bound).
    rolling_delta: Option<u64>,
    /// Each live snapshot's seal `Moment`, learned from `snapshot`'s
    /// server-stamped cut — the origin lookup when a rollout branches onto it.
    /// Kept bounded: an entry is dropped with its snapshot handle.
    snap_moments: BTreeMap<explorer::SnapId, u64>,
    /// The origin `Moment` of the most recent `branch`/`replay`: the point the
    /// next rolling-deadline rollout measures its span from.
    current_origin: Option<u64>,
}

impl<M: Machine> MazeDeclaredMachine<M> {
    /// Wrap `inner` with the maze instrumentation declaration. Rolling
    /// deadlines are off until the driver sets [`Self::rolling_delta`] (a
    /// same-module assignment; the box path sets it to the live
    /// `deadline_delta`, the toy leaves it `None`).
    pub fn new(inner: M) -> Self {
        MazeDeclaredMachine {
            inner,
            catalog: maze_instrumentation_catalog(),
            prepends_catalog: None,
            rolling_delta: None,
            snap_moments: BTreeMap::new(),
            current_origin: None,
        }
    }
}

impl<M: Machine> Machine for MazeDeclaredMachine<M> {
    fn branch(&mut self, snap: explorer::SnapId, env: &Reproducer) -> Result<(), MachineError> {
        // This branch's origin is the target snapshot's seal Moment — the point
        // the rolling deadline will measure the rollout's span from.
        self.current_origin = self.snap_moments.get(&snap).copied();
        self.inner.branch(snap, env)
    }
    fn replay(&mut self, snap: explorer::SnapId) -> Result<(), MachineError> {
        self.current_origin = self.snap_moments.get(&snap).copied();
        self.inner.replay(snap)
    }
    fn run(
        &mut self,
        until: &StopConditions,
        resolve: Option<&explorer::Answer>,
    ) -> Result<StopReason, MachineError> {
        // The quiet-arm rollout carries no deadline of its own (`None`); impose
        // this branch's rolling deadline — `origin + delta` — so every rollout
        // gets a full span budget past its own branch point (`hm-qcpp`). A run
        // that names an explicit deadline is a seal replay / probe / setup leg
        // (the Option-C candidate-seal machinery names candidate Moments) and
        // is forwarded verbatim, never re-anchored.
        if let (Some(delta), None, Some(origin)) =
            (self.rolling_delta, until.deadline, self.current_origin)
        {
            let rolling = StopConditions {
                deadline: Some(Moment(origin.saturating_add(delta))),
                on: until.on,
            };
            return self.inner.run(&rolling, resolve);
        }
        self.inner.run(until, resolve)
    }
    fn snapshot(&mut self) -> Result<(explorer::SnapId, EvidenceCut), MachineError> {
        let (id, mut cut) = self.inner.snapshot()?;
        // Learn this seal's origin Moment for a later branch onto it (the
        // rolling deadline's anchor), from the server-stamped cut — before the
        // catalog ordinal bump below, which shifts the event COUNT, never the
        // Moment.
        self.snap_moments.insert(id, cut.at.0);
        // The seal cut is an SDK-vector ORDINAL count; prepending the
        // standalone catalog at position 0 shifts every firing one ordinal
        // and adds a schema tuple the cut must count (the task-132
        // `DeclaredMachine` rule — without the bump, `decode_child_suffix`'s
        // ordinal skip retains one inherited firing per child). An inner that
        // declares its own catalog is upgraded in place (no ordinal added).
        if self.prepends_catalog == Some(true) {
            cut.sdk_events += 1;
        }
        Ok((id, cut))
    }
    fn drop_snap(&mut self, snap: explorer::SnapId) -> Result<(), MachineError> {
        // The handle is gone; its origin can never be branched onto again.
        self.snap_moments.remove(&snap);
        self.inner.drop_snap(snap)
    }
    fn hash(&mut self) -> Result<[u8; 32], MachineError> {
        self.inner.hash()
    }
    fn coverage(&self) -> &[u8] {
        self.inner.coverage()
    }
    fn recorded_env(&self) -> Result<Reproducer, MachineError> {
        self.inner.recorded_env()
    }
    fn sdk_events(&mut self) -> Result<Vec<(u64, u32, Vec<u8>)>, MachineError> {
        let mut out = self.inner.sdk_events()?;
        match out.iter_mut().find(|(_, id, _)| *id == CATALOG_EVENT_ID) {
            Some((_, _, bytes)) => {
                self.prepends_catalog = Some(false);
                *bytes =
                    sdk_events::resolve_v1_declaration(bytes, &maze_resolution()).map_err(|e| {
                        MachineError::Transport(format!(
                            "guest catalog failed to upgrade to the maze instrumentation \
                             declaration: {e}"
                        ))
                    })?;
            }
            None => {
                self.prepends_catalog = Some(true);
                out.insert(0, (0u64, CATALOG_EVENT_ID, self.catalog.clone()));
            }
        }
        Ok(out)
    }
    fn console(&mut self) -> Result<Vec<(u64, Vec<u8>)>, MachineError> {
        self.inner.console()
    }
}

/// Drain + decode the machine's SDK event capture into normalized evidence.
fn drain_events<M: Machine>(machine: &mut M) -> Result<Normalized, MachineError> {
    let raw = machine.sdk_events()?;
    crate::sdk_compat::decode_sdk(&raw)
        .map_err(|e| MachineError::Transport(format!("SDK capture failed to decode: {e}")))
}

/// Seal the campaign base. A live guest seals at its `setup_complete`
/// snapshot point (`require_snapshot_point`); the portable toy falls back to
/// the generic seal-retry loop (its quiescent boot is its normal seal). The
/// task-86 `seal_base` shape, minus the billboard machinery the maze has no
/// use for.
fn seal_base<M: Machine>(
    machine: &mut M,
    cfg: &MazeCampaignConfig,
) -> Result<(explorer::SnapId, u64), MazeCampaignError> {
    let mut vt = crate::probe_vtime(machine)?;
    let snapshot_point = StopMask(1u32 << control_proto::class_bit::SNAPSHOT_POINT as u32);
    let stop = machine.run(
        &StopConditions {
            deadline: Some(Moment(vt.saturating_add(cfg.setup_deadline_delta))),
            on: snapshot_point,
        },
        None,
    )?;
    if let StopReason::SnapshotPoint { vtime } = stop {
        vt = vtime.0;
        match machine.snapshot() {
            Ok((snap, _cut)) => return Ok((snap, vt)),
            Err(e) if cfg.require_snapshot_point => return Err(e.into()),
            Err(_) => {}
        }
    } else if cfg.require_snapshot_point {
        return Err(MazeCampaignError::SetupNotReached {
            stop: format!("{stop:?}"),
        });
    } else {
        vt = stop.vtime().0;
    }
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        match machine.snapshot() {
            Ok((snap, _cut)) => return Ok((snap, vt)),
            Err(MachineError::NotQuiescent) => {
                if attempts >= cfg.snapshot_max_attempts {
                    return Err(MachineError::NotQuiescent.into());
                }
                let stop = machine.run(
                    &StopConditions {
                        deadline: Some(Moment(vt.saturating_add(cfg.snapshot_retry_step))),
                        on: StopMask::NONE,
                    },
                    None,
                )?;
                if !matches!(stop, StopReason::Deadline { .. }) {
                    return Err(MachineError::NotQuiescent.into());
                }
                vt = stop.vtime().0;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Extract the booted image's maze spec from its boot serial: the last
/// `MAZE_SPEC: w=<w> l=<l> doors=<d> seed=<hex> …` line the maze agent prints
/// before `MAZE_READY`. The box driver cross-checks this **fact** against the
/// operator's spec flags and refuses a mismatch — logs from a different maze
/// are not comparable (the ROM content-hash discipline, transplanted).
pub fn serial_maze_spec(serial: &[u8]) -> Option<MazeSpec> {
    const TAG: &[u8] = b"MAZE_SPEC:";
    let mut found = None;
    let mut from = 0;
    while from + TAG.len() <= serial.len() {
        let Some(at) = serial[from..]
            .windows(TAG.len())
            .position(|w| w == TAG)
            .map(|p| from + p)
        else {
            break;
        };
        let line_end = serial[at..]
            .iter()
            .position(|b| *b == b'\n')
            .map(|p| at + p)
            .unwrap_or(serial.len());
        if let Ok(line) = std::str::from_utf8(&serial[at + TAG.len()..line_end]) {
            let field = |key: &str| -> Option<&str> {
                line.split_whitespace()
                    .find_map(|tok| tok.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
            };
            let parse_u32 = |v: &str| v.parse::<u32>().ok();
            let parse_seed = |v: &str| {
                v.strip_prefix("0x")
                    .and_then(|h| u64::from_str_radix(h, 16).ok())
                    .or_else(|| v.parse::<u64>().ok())
            };
            if let (Some(w), Some(l), Some(d), Some(seed)) = (
                field("w").and_then(parse_u32),
                field("l").and_then(parse_u32),
                field("doors").and_then(parse_u32),
                field("seed").and_then(parse_seed),
            ) {
                found = Some(MazeSpec {
                    width: w,
                    levels: l,
                    doors: d,
                    maze_seed: seed,
                });
            }
        }
        from = at + TAG.len();
    }
    found
}

/// Lowercase hex of a state hash (the log's determinism witness).
fn hex(h: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in h {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Drive one maze campaign against `machine` under `cfg`, end-to-end through
/// the two-barrier [`DifferentialCampaign`] controller. Seals the base (a
/// live guest at its `setup_complete` snapshot point), then per branch runs
/// one two-barrier step: the selector picks genesis or a retained Entry (the
/// quiet reseed-only mutate), the rollout's normalized wire-v2 SDK evidence
/// commits through the Revision coordinator, provisional same-`Moment` cuts
/// nominate budgeted materialization replay, and occupancy admits at the
/// **actual** `sealed_at`. The per-branch log (cells touched, depth, terminal
/// `state_hash`) derives from the committed evidence batches; the deepest
/// branch's reproducer is retained when a trace directory is configured.
pub fn run_maze_campaign<M: Machine>(
    machine: M,
    codec: Box<dyn EnvCodec>,
    cfg: &MazeCampaignConfig,
    config: ExplorationConfig,
) -> Result<MazeCampaignOutcome, MazeCampaignError> {
    // The search policies, mapped onto the controller's seams. PureRandom is
    // task 84's primary control — the frontier held EMPTY (no candidate
    // materialization at all), random-restart search; FrontierOff runs the
    // identical machinery as the subject with an always-explore selector.
    let (selector, candidate_cap, replay_budget): (Box<dyn Selector>, usize, u64) = match config {
        ExplorationConfig::SelectorV1 => (
            Box::new(ExploreExploitSelector::new().with_explore_period(cfg.explore_period)),
            cfg.candidate_cap,
            cfg.replay_budget,
        ),
        ExplorationConfig::FrontierOff => (
            Box::new(GenesisSelector::new()),
            cfg.candidate_cap,
            cfg.replay_budget,
        ),
        ExplorationConfig::PureRandom => (Box::new(GenesisSelector::new()), 0, 0),
        ExplorationConfig::Signal => return Err(MazeCampaignError::SignalUnavailable),
    };

    // The workload's explicit wire-v2 instrumentation declaration rides every
    // SDK capture from here on. The rolling deadline (`deadline_delta`) rides
    // it too: the wrapper measures each rollout's span from its own branch
    // origin, so an exploit off a late-sealed Entry is never starved to a
    // zero span (`hm-qcpp`). `None` (the toy) leaves the natural terminal.
    let mut machine = MazeDeclaredMachine::new(machine);
    machine.rolling_delta = cfg.deadline_delta;

    let (base, base_vtime) = seal_base(&mut machine, cfg)?;
    // The setup drain: learns the catalog mode (upgrade-in-place vs prepend)
    // BEFORE the controller stamps its genesis cut, and keeps any setup
    // events out of branch 0's trace.
    let _setup = drain_events(&mut machine)?;
    // The controller snapshots its own genesis from this same stopped state;
    // the seal_base handle would only duplicate it.
    machine.drop_snap(base)?;

    let until = StopConditions {
        // No campaign-wide absolute deadline: the wrapper imposes each rollout's
        // deadline as `branch_origin + deadline_delta` (the rolling deadline),
        // so a candidate that seals at or past one fixed campaign deadline can
        // no longer starve its exploit children to a zero span (`hm-qcpp`). The
        // toy (`rolling_delta == None`) keeps its natural terminal here.
        deadline: None,
        // Quiet arm: no decisions surface, no assertion stops a run (the goal
        // marker is a reachable HIT); the rolling deadline or the guest's
        // natural terminal bounds the rollout.
        on: StopMask::NONE,
    };

    let quiet = QuietCodec {
        inner: codec,
        window: cfg
            .deadline_delta
            .unwrap_or(u64::from(cfg.steps_per_rollout) * MAZE_TOY_STEP),
    };
    // The durable evidence ledger: beside the retained traces when a trace
    // directory is configured, else a campaign-lifetime scratch file.
    let scratch;
    let evidence_path = match &cfg.trace_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .map_err(|e| MazeCampaignError::Retention(e.to_string()))?;
            dir.join("evidence.log")
        }
        None => {
            scratch =
                tempfile::tempdir().map_err(|e| MazeCampaignError::Retention(e.to_string()))?;
            scratch.path().join("evidence.log")
        }
    };
    let ledger = EvidenceLedger::open(&evidence_path).map_err(CampaignError::from)?;
    let coordinator = Coordinator::genesis(
        Box::new(MemLedger::new()),
        CampaignConfigId::digest(&cfg.campaign_seed.to_le_bytes()),
    )
    .map_err(CampaignError::from)?;
    let mut camp = DifferentialCampaign::new(
        machine,
        Box::new(quiet),
        Box::new(DeclineTactic::new()),
        selector,
        Box::new(MazeObservationCells),
        ledger,
        coordinator,
        explorer::CampaignConfig {
            candidate_cap,
            replay_budget,
            ingress: Ingress::Binary,
            // The declared full-retention evaluation profile, from rollout
            // one (`hm-5sv` / the bead's binding requirement).
            retention: RetentionProfile::Full,
            evidence_budget: None,
            // The quiet maze rollout surfaces no mid-run snapshot points —
            // nomination coordinates come from the rollout's own SDK-event
            // moments (the same-Moment X/Y cut source).
            nominate: Nomination::EventMoments,
            // The per-branch determinism artifact the 25/25 gate compares.
            hash_rollouts: true,
        },
        cfg.campaign_seed,
    )?;
    camp.set_stop_conditions(until);

    let mut events = Vec::new();
    let mut deepest: Option<(u64, u64, RunTrace)> = None;
    let mut goal_hits = 0u64;
    let mut work = MazeWorkEvidence {
        branches: 0,
        min_vtime_span: u64::MAX,
        min_steps: u64::MAX,
    };

    for branch in 0..cfg.max_branches {
        let report = camp.step()?;
        // The branch's committed evidence batch — the durable authority the
        // per-branch log derives from.
        let batch = camp
            .coordinator()
            .committed_inputs()
            .into_iter()
            .find(|(rev, _, _)| *rev == report.rollout_revision)
            .map(|(_, _, b)| b)
            .ok_or_else(|| {
                MazeCampaignError::Retention("rollout revision has no committed batch".into())
            })?;
        let ev =
            camp.ledger().get(&batch).cloned().ok_or_else(|| {
                MazeCampaignError::Retention("committed batch not in ledger".into())
            })?;
        // Box mode: a rollout that ended anywhere but its deadline DIED and
        // fails the campaign loudly. The toy's natural terminal is Quiescent.
        if cfg.require_snapshot_point && !matches!(ev.terminal, StopReason::Deadline { .. }) {
            return Err(MazeCampaignError::RolloutDied {
                branch,
                stop: format!("{:?}", ev.terminal),
            });
        }
        let branch_start = ev.parent_cut.map(|c| c.at.0).unwrap_or(base_vtime);
        let vtime_span = ev.terminal.vtime().0.saturating_sub(branch_start);
        // Statically infallible: `hash_rollouts` is configured above.
        let state_hash = report.state_hash.expect("hash_rollouts is configured");
        let (touched, depth, branch_goal_hits) = maze_cells(&ev.normalized.events);
        goal_hits += branch_goal_hits;
        // The branch's own walk steps: one Y observation per step by
        // construction.
        let steps = ev
            .normalized
            .events
            .iter()
            .filter(|e| {
                matches!(
                    (&e.id, &e.payload),
                    (ObservationId::Point { namespace, local }, Payload::State { .. })
                        if *namespace == NS_STATE && u64::from(*local) == reg::Y
                )
            })
            .count() as u64;
        let trace = RunTrace {
            terminal: ev.terminal.clone(),
            env: ev.env.clone(),
            coverage: None,
            events: crate::sdk_compat::guest_events_of(&ev.normalized),
            records: Vec::new(),
        };
        work.branches += 1;
        work.min_vtime_span = work.min_vtime_span.min(vtime_span);
        work.min_steps = work.min_steps.min(steps);
        if deepest.as_ref().is_none_or(|(d, _, _)| depth > *d) {
            deepest = Some((depth, branch, trace));
        }
        events.push(DiscoveryEvent {
            branch,
            touched,
            depth,
            state_hash: hex(&state_hash),
        });
    }

    // The held-obligation evidence: the finalized absence view's still-open
    // MustHit expectations (the goal marker while unreached). NB: per the
    // escalated spine finding (`hm-19l0`; see `MazeCampaignOutcome::open_expectations`),
    // a v2-declared satisfaction does not clear the view; `goal_hits` is the
    // authoritative witness.
    let open_expectations = camp.absences().absences().len() as u64;

    // Retain the deep reproducer (env sidecar + full journal).
    let deep = match deepest {
        None => None,
        Some((depth, branch, trace)) => {
            let trace_id = match &cfg.trace_dir {
                None => None,
                Some(dir) => {
                    let store = runtrace::TraceStore::open(dir)
                        .map_err(|e| MazeCampaignError::Retention(e.to_string()))?;
                    let id = store
                        .record(&trace, runtrace::Retain::Full)
                        .map_err(|e| MazeCampaignError::Retention(e.to_string()))?;
                    Some(id.to_hex())
                }
            };
            Some(MazeDeepReproducer {
                branch,
                depth,
                trace_id,
            })
        }
    };

    Ok(MazeCampaignOutcome {
        log: ExplorationLog {
            workload: "maze".to_string(),
            rom_sha256: None,
            config,
            seed: cfg.campaign_seed,
            events,
        },
        deep,
        goal_hits,
        open_expectations,
        work: if work.branches == 0 {
            MazeWorkEvidence::default()
        } else {
            work
        },
    })
}

// ---------------------------------------------------------------------------
// The portable toy: the real maze walk behind the Machine seam.
// ---------------------------------------------------------------------------

/// The toy's V-time per walk step.
pub const MAZE_TOY_STEP: u64 = 10;

/// The toy's quiescent boot V-time.
const TOY_BASE_VTIME: u64 = 1_000;

/// One computed trajectory step: its moment, the walker state after it, and
/// whether it first reached the goal (the goal marker fires on that edge).
#[derive(Clone, Copy)]
struct TrajStep {
    at: u64,
    state: MazeState,
    goal_edge: bool,
}

/// A snapshot's captured material.
#[derive(Clone)]
struct ToySnap {
    vtime: u64,
    env: Reproducer,
    state: MazeState,
    /// The sealed capture prefix (firings only — the wrapper owns the
    /// catalog), CUMULATIVE through the lineage.
    prefix: Vec<(u64, u32, Vec<u8>)>,
}

/// A deterministic toy [`Machine`] that walks the **real** shared maze logic
/// and emits its X/Y state (both at the same `Moment` each step, X then Y)
/// over the real wire layout — a pure function of `(branch base, env)`. The
/// walker state is part of every snapshot, and a branch resumes the walk
/// **from the restored state** with entropy derived from the branch env (its
/// seed folded with its task-78 reseed markers), so an exploit branch
/// genuinely returns-then-explores: the Go-Explore mechanism, not a
/// simulation of it. Lineage-aware (task 132): a snapshot captures the
/// emitted prefix and a branch restores it into the child, so cut counts are
/// cumulative.
pub struct MazeToyMachine {
    spec: MazeSpec,
    steps_per_rollout: u32,
    current: Reproducer,
    vtime: u64,
    branch_vtime: u64,
    /// The walker state at the branch point (the restored snapshot's state;
    /// the maze start at boot).
    branch_state: MazeState,
    traj: Vec<TrajStep>,
    prefix: Vec<(u64, u32, Vec<u8>)>,
    snaps: BTreeMap<u64, ToySnap>,
    next_snap: u64,
}

impl MazeToyMachine {
    /// A fresh toy walker, quiescent at boot, at the maze start.
    pub fn new(spec: MazeSpec, steps_per_rollout: u32) -> Self {
        MazeToyMachine {
            spec,
            steps_per_rollout,
            current: explorer::SpecEnvCodec.seeded(0),
            vtime: TOY_BASE_VTIME,
            branch_vtime: TOY_BASE_VTIME,
            branch_state: MazeState::start(),
            traj: Vec::new(),
            prefix: Vec::new(),
            snaps: BTreeMap::new(),
            next_snap: 1,
        }
    }

    /// The env's effective entropy seed: the base seed folded with the
    /// task-78 reseed markers (any added marker diverges the stream — the
    /// quiet exploit move's contract), mirroring the game toy's fold.
    fn effective_seed(env: &Reproducer) -> u64 {
        explorer::AdapterEnv::decode(env)
            .map(|d| {
                d.spec.reseeds().iter().fold(d.spec.seed(), |acc, (at, s)| {
                    acc ^ s.rotate_left((*at % 63) as u32)
                })
            })
            .unwrap_or(0)
    }

    /// Recompute this branch's trajectory: the real maze walk from `state`
    /// under the env-derived entropy stream, one step per `MAZE_TOY_STEP` of
    /// V-time past the branch point.
    fn retrace(&mut self, state: MazeState) {
        let mut prng = explorer::Prng::new(Self::effective_seed(&self.current));
        let mut st = state;
        self.traj = (0..self.steps_per_rollout)
            .map(|i| {
                let at = self.branch_vtime + (u64::from(i) + 1) * MAZE_TOY_STEP;
                let byte = (prng.next_u64() & 0xff) as u8;
                let next = maze::step(&self.spec, st, byte);
                let goal_edge = next.goal && !st.goal;
                st = next;
                TrajStep {
                    at,
                    state: st,
                    goal_edge,
                }
            })
            .collect();
    }

    /// The walker state as of `vtime`: the last trajectory step at or before
    /// it, else the branch state (before the first step, or at boot).
    fn state_at(&self, vtime: u64) -> MazeState {
        self.traj
            .iter()
            .take_while(|t| t.at <= vtime)
            .last()
            .map(|t| t.state)
            .unwrap_or(self.branch_state)
    }

    /// This branch's own emissions up to `vtime`, over the real wire layout:
    /// X then Y at the step's one moment, plus the goal marker on its edge.
    fn own_events(&self, vtime: u64) -> Vec<(u64, u32, Vec<u8>)> {
        let mut out = Vec::new();
        for t in self.traj.iter().take_while(|t| t.at <= vtime) {
            out.push(state_event(t.at, reg::X, t.state.x_register()));
            out.push(state_event(t.at, reg::Y, t.state.y_register()));
            if t.goal_edge {
                out.push(goal_event(t.at));
            }
        }
        out
    }

    /// The cumulative capture at `vtime`: restored ancestor prefix + own
    /// emissions (firings only; the `MazeDeclaredMachine` wrapper prepends
    /// the catalog).
    fn capture(&self, vtime: u64) -> Vec<(u64, u32, Vec<u8>)> {
        let mut out = self.prefix.clone();
        out.extend(self.own_events(vtime));
        out
    }
}

/// Encode one `set` state firing the way the guest SDK wire does
/// (`[op u8][value u64 LE]` under `NS_STATE`), so the toy exercises the real
/// `sdk_events::decode_binary` path.
fn state_event(at: u64, reg: u64, value: u64) -> (u64, u32, Vec<u8>) {
    let id = (u32::from(NS_STATE) << 24) | (reg as u32);
    let mut payload = Vec::with_capacity(9);
    payload.push(0u8); // op: set
    payload.extend_from_slice(&value.to_le_bytes());
    (at, id, payload)
}

/// Encode the goal marker's assertion HIT (`[disposition u8][detail_len u16]`,
/// empty detail) under `NS_ASSERT`.
fn goal_event(at: u64) -> (u64, u32, Vec<u8>) {
    let id = (u32::from(NS_ASSERT) << 24) | GOAL_LOCAL;
    (at, id, vec![0u8, 0, 0])
}

impl Machine for MazeToyMachine {
    fn branch(&mut self, snap: explorer::SnapId, env: &Reproducer) -> Result<(), MachineError> {
        let Some(s) = self.snaps.get(&snap.0).cloned() else {
            return Err(MachineError::UnknownSnapshot(snap.0));
        };
        explorer::AdapterEnv::decode(env)?;
        self.vtime = s.vtime;
        self.branch_vtime = s.vtime;
        self.branch_state = s.state;
        self.prefix = s.prefix;
        self.current = env.clone();
        self.retrace(s.state);
        Ok(())
    }

    fn replay(&mut self, snap: explorer::SnapId) -> Result<(), MachineError> {
        let Some(s) = self.snaps.get(&snap.0).cloned() else {
            return Err(MachineError::UnknownSnapshot(snap.0));
        };
        self.vtime = s.vtime;
        self.branch_vtime = s.vtime;
        self.branch_state = s.state;
        self.prefix = s.prefix;
        self.current = s.env.clone();
        self.retrace(s.state);
        Ok(())
    }

    fn run(
        &mut self,
        until: &StopConditions,
        _resolve: Option<&explorer::Answer>,
    ) -> Result<StopReason, MachineError> {
        match until.deadline {
            // Advance exactly to the deadline (the materialization replay's
            // seal coordinate; also the live-shaped rollout terminal).
            Some(d) => {
                self.vtime = self.vtime.max(d.0);
                Ok(StopReason::Deadline {
                    vtime: Moment(self.vtime),
                })
            }
            // The natural terminal: one quiescent tick past the last step.
            None => {
                let terminal = self
                    .branch_vtime
                    .saturating_add((u64::from(self.steps_per_rollout) + 1) * MAZE_TOY_STEP);
                self.vtime = self.vtime.max(terminal);
                Ok(StopReason::Quiescent {
                    vtime: Moment(self.vtime),
                })
            }
        }
    }

    fn snapshot(&mut self) -> Result<(explorer::SnapId, EvidenceCut), MachineError> {
        let id = self.next_snap;
        self.next_snap += 1;
        let vt = self.vtime;
        // The cut is stamped from the same stopped state the seal captures
        // (task 127): the cumulative capture prefix at or before the seal
        // moment (firings only; the wrapper's prepended catalog bumps it).
        let sealed = self.capture(vt);
        let included = sealed.len() as u64;
        self.snaps.insert(
            id,
            ToySnap {
                vtime: vt,
                env: self.current.clone(),
                state: self.state_at(vt),
                prefix: sealed,
            },
        );
        Ok((
            explorer::SnapId(id),
            EvidenceCut {
                at: Moment(vt),
                sdk_events: included,
            },
        ))
    }

    fn drop_snap(&mut self, snap: explorer::SnapId) -> Result<(), MachineError> {
        self.snaps
            .remove(&snap.0)
            .map(|_| ())
            .ok_or(MachineError::UnknownSnapshot(snap.0))
    }

    fn hash(&mut self) -> Result<[u8; 32], MachineError> {
        use sha2::{Digest, Sha256};
        let st = self.state_at(self.vtime);
        let mut h = Sha256::new();
        h.update(b"campaign-runner.mazetoy.state_hash.v1");
        h.update(st.x_register().to_le_bytes());
        h.update(st.y_register().to_le_bytes());
        h.update([u8::from(st.goal)]);
        h.update(self.vtime.to_le_bytes());
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

    fn sdk_events(&mut self) -> Result<Vec<(u64, u32, Vec<u8>)>, MachineError> {
        Ok(self.capture(self.vtime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instrumentation declaration is a real, decodable wire-v2 catalog:
    /// X and Y resolve to reducible `set` state (no unresolved-v1 state
    /// semantics anywhere in the schema) and the goal marker is a declared
    /// `MustHit` occurrence.
    #[test]
    fn instrumentation_catalog_is_fully_reducible_wire_v2() {
        use sdk_events::{Classification, Expectation, UpdateOp, ValueShape};
        let n = sdk_events::decode_binary(&[(
            sdk_events::Moment(0),
            CATALOG_EVENT_ID,
            maze_instrumentation_catalog(),
        )])
        .expect("the maze catalog decodes");
        for reg in [reg::X, reg::Y] {
            let entry = n
                .schema
                .entry(&ObservationId::Point {
                    namespace: NS_STATE,
                    local: reg as u32,
                })
                .unwrap_or_else(|| panic!("register {reg} is declared"));
            assert!(entry.is_reducible_state(), "register {reg} is reducible");
            assert_eq!(entry.base_op, Some(UpdateOp::Set));
            assert_eq!(entry.value_shape, Some(ValueShape::U64));
        }
        let goal = n.schema.entry(&goal_id()).expect("the goal is declared");
        assert_eq!(goal.classification, Classification::Occurrence);
        assert_eq!(goal.expectation, Some(Expectation::MustHit));
        // No entry in the schema is unresolved state (the wire-v2 bar).
        for entry in n.schema.entries() {
            assert!(
                entry.classification != Classification::State || entry.is_reducible_state(),
                "no unresolved state semantics in the maze schema"
            );
        }
    }

    /// `maze_cells` folds the same-Moment X-then-Y tuple into `(x, y)` keys,
    /// tracks the depth max, and counts goal hits.
    #[test]
    fn maze_cells_folds_same_moment_tuples() {
        let raw = vec![
            (0u64, CATALOG_EVENT_ID, maze_instrumentation_catalog()),
            state_event(10, reg::X, 1),
            state_event(10, reg::Y, 0),
            state_event(20, reg::X, 2),
            state_event(20, reg::Y, 0),
            state_event(30, reg::X, 0),
            state_event(30, reg::Y, 3),
            goal_event(30),
        ]
        .into_iter()
        .map(|(m, id, b)| (sdk_events::Moment(m), id, b))
        .collect::<Vec<_>>();
        let n = sdk_events::decode_binary(&raw).expect("decodes");
        let (cells, depth, goal_hits) = maze_cells(&n.events);
        assert_eq!(
            cells,
            vec![
                maze_cell_key(1, 0),
                maze_cell_key(2, 0),
                maze_cell_key(0, 3)
            ]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
        );
        assert_eq!(depth, 3);
        assert_eq!(goal_hits, 1);
    }

    /// The cell projection keys `(x, y)` only when both registers are
    /// reduced at the cut; a pre-progress state keys to the empty cell.
    #[test]
    fn observation_cells_key_needs_both_registers() {
        let cut = EvidenceCut {
            at: Moment(10),
            sdk_events: 2,
        };
        let mut obs = ObservationMap::new();
        assert!(MazeObservationCells.key(cut, &obs).is_empty());
        obs.insert(
            ObservationId::Point {
                namespace: NS_STATE,
                local: reg::X as u32,
            },
            ReducedValue::Scalar(3),
        );
        assert!(MazeObservationCells.key(cut, &obs).is_empty());
        obs.insert(
            ObservationId::Point {
                namespace: NS_STATE,
                local: reg::Y as u32,
            },
            ReducedValue::Scalar(5),
        );
        assert_eq!(
            MazeObservationCells.key(cut, &obs),
            maze_cell_key(3, 5).to_le_bytes().to_vec()
        );
    }

    /// The key packs injectively for the maze's bounded registers.
    #[test]
    fn cell_key_is_injective_on_the_grid() {
        let mut seen = std::collections::BTreeSet::new();
        for x in 0..16u64 {
            for y in 0..16u64 {
                assert!(seen.insert(maze_cell_key(x, y)));
            }
        }
    }

    /// The serial MAZE_SPEC cross-check parses the agent's line (last one
    /// wins) and refuses garbage.
    #[test]
    fn serial_maze_spec_parses_the_agent_line() {
        let serial = b"boot noise\nMAZE_SPEC: w=4 l=6 doors=4 seed=0x6d617a65 reachable=25\nMAZE_READY: launching maze-agent\n";
        assert_eq!(
            serial_maze_spec(serial),
            Some(MazeSpec {
                width: 4,
                levels: 6,
                doors: 4,
                maze_seed: 0x6d61_7a65,
            })
        );
        // The last line wins (a re-exec re-prints it).
        let twice = b"MAZE_SPEC: w=2 l=2 doors=2 seed=1\nMAZE_SPEC: w=3 l=5 doors=4 seed=7\n";
        assert_eq!(
            serial_maze_spec(twice).map(|s| (s.width, s.levels, s.doors, s.maze_seed)),
            Some((3, 5, 4, 7))
        );
        assert_eq!(serial_maze_spec(b"no marker here"), None);
        assert_eq!(
            serial_maze_spec(b"MAZE_SPEC: w=x l=6 doors=4 seed=1\n"),
            None
        );
    }

    /// The Signal configuration is refused loudly — the maze gate's subject
    /// is the simple archive-guided selector.
    #[test]
    fn signal_configuration_is_refused() {
        let cfg = MazeCampaignConfig::smoke(1);
        let machine = MazeToyMachine::new(cfg.spec, cfg.steps_per_rollout);
        assert!(matches!(
            run_maze_campaign(
                machine,
                Box::new(explorer::SpecEnvCodec),
                &cfg,
                ExplorationConfig::Signal
            ),
            Err(MazeCampaignError::SignalUnavailable)
        ));
    }

    /// The rolling deadline (`hm-qcpp`): with `rolling_delta` set, a no-deadline
    /// rollout run is imposed a deadline of `origin + delta` measured from *its
    /// own* branch point — so an exploit branching off a late-sealed Entry runs
    /// a full span past that late origin, not the leftover of one campaign-wide
    /// absolute deadline. A run that names an explicit deadline (a seal replay
    /// / probe) is forwarded verbatim — the Option-C seal machinery is untouched.
    #[test]
    fn rolling_deadline_measures_span_from_each_branch_origin() {
        const DELTA: u64 = 200;
        let mut m = MazeDeclaredMachine::new(MazeToyMachine::new(MazeSpec::small(), 12));
        m.rolling_delta = Some(DELTA);
        let env = explorer::SpecEnvCodec.seeded(0);
        let quiet = StopConditions {
            deadline: None,
            on: StopMask::NONE,
        };

        // Seal a base at boot; a genesis rollout runs DELTA past it.
        let (base, base_cut) = m.snapshot().expect("seal base");
        assert_eq!(base_cut.at.0, TOY_BASE_VTIME);
        m.branch(base, &env).expect("branch genesis");
        let stop = m.run(&quiet, None).expect("genesis rollout");
        assert_eq!(
            stop.vtime().0,
            TOY_BASE_VTIME + DELTA,
            "genesis rollout runs delta past the base origin"
        );

        // Seal an Entry at a LATE moment (well past a base+delta deadline), then
        // branch onto it as an exploit would: the rollout runs DELTA past the
        // LATE origin, not the base's fixed deadline (the hm-qcpp fix).
        let late = TOY_BASE_VTIME + 10 * DELTA;
        m.run(
            &StopConditions {
                deadline: Some(Moment(late)),
                on: StopMask::NONE,
            },
            None,
        )
        .expect("advance to a late point");
        let (entry, entry_cut) = m.snapshot().expect("seal late entry");
        assert_eq!(entry_cut.at.0, late);
        m.branch(entry, &env).expect("branch exploit");
        let stop = m.run(&quiet, None).expect("exploit rollout");
        assert_eq!(
            stop.vtime().0,
            late + DELTA,
            "exploit rollout runs delta past ITS late origin (never a zero span)"
        );

        // A run that names an explicit deadline is a seal replay / probe: it is
        // forwarded verbatim, never re-anchored to origin + delta.
        m.branch(entry, &env).expect("branch");
        let stop = m
            .run(
                &StopConditions {
                    deadline: Some(Moment(late + 5)),
                    on: StopMask::NONE,
                },
                None,
            )
            .expect("seal replay");
        assert_eq!(
            stop.vtime().0,
            late + 5,
            "an explicit deadline (seal replay) is forwarded verbatim"
        );

        // Rolling off (the toy default): a no-deadline run keeps the natural
        // terminal — the wrapper imposes nothing.
        let mut plain = MazeDeclaredMachine::new(MazeToyMachine::new(MazeSpec::small(), 12));
        let (b, _) = plain.snapshot().expect("seal");
        plain.branch(b, &env).expect("branch");
        assert!(
            matches!(
                plain.run(&quiet, None).expect("run"),
                StopReason::Quiescent { .. }
            ),
            "rolling off ⇒ the natural terminal, no imposed deadline"
        );
    }
}
