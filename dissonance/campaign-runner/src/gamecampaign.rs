// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task 86 M0 — the SMB game-workload campaign driver, **quiet arm**.
//!
//! Runs the real-game exploration campaign against the game image's play-agent
//! under `FaultPolicy::none()` with buggify off and **zero fault vocabulary**:
//! every branch env is a pure seeded environment ([`EnvCodec::seeded`] — no
//! `.perturb` anywhere in this module), so the only thing the search varies is
//! the guest's decision entropy — exactly the task-84 `quiet` arm this task
//! re-runs on a real game.
//!
//! Per the 2026-07-09 amendment, M0 runs the **existing default/baseline
//! search** (this crate's task-60/69 loop shapes) and makes **no selector
//! claim**: [`ExplorationConfig::PureRandom`] is the task-60 blind-seed loop
//! (fresh seed from the base every branch), [`ExplorationConfig::SelectorV1`]
//! is the task-69 novelty loop (explore/exploit over admitted exemplars).
//! [`ExplorationConfig::Signal`] — task 70's selector, ruled NO-GO'd out of
//! existence — is **refused loudly** ([`GameCampaignError::SignalUnavailable`])
//! until M1 has a selector artifact to test; the composed-`Explorer` on-ramp
//! is task 84's deliverable, reused when it lands (not rebuilt here).
//!
//! Cells are keyed **host-side** (R-L2): the guest emits raw state registers;
//! `sdk-events` decodes the SDK capture into ordered [`SdkEvent`]s, and
//! [`smb_cells`] folds their `Payload::State` firings into the
//! `(game mode, world, level, x-bucket)` tuple key — the analog of
//! Antithesis's discretized `(x, y)`. Retuning the key needs no guest rebuild
//! (the sanctioned first response to a FAIL).
//!
//! Everything is a pure function of `(campaign_seed, machine, config)`; the
//! box determinism gate reruns a campaign and requires the identical
//! per-branch `state_hash` sequence 25/25.

use std::collections::BTreeSet;

use benchmark::exploration::{DiscoveryEvent, ExplorationConfig, ExplorationLog};
use explorer::{
    CampaignError, CellKey, DeclineTactic, DifferentialCampaign, EnvCodec, EvidenceCut,
    EvidenceLedger, ExploreExploitSelector, GenesisSelector, Ingress, Machine, MachineError,
    Moment, Nomination, ObservationCells, ObservationMap, ReducedValue, Reproducer,
    RetentionProfile, RunTrace, Selector, StopConditions, StopMask, StopReason,
};
use revision_coordinator::{CampaignConfigId, Coordinator, MemLedger};
use sdk_events::{NS_STATE, Normalized, ObservationId, Payload, SdkEvent};

/// Local mirrors of the play-agent's state-register catalog
/// (`harmony-linux/play-agent/src/regs.rs` — the guest crates sit outside this
/// workspace, so the ids are mirrored here with the same values; the
/// conventions mirror-type pattern).
pub mod reg {
    /// `OperMode` — 1 is gameplay.
    pub const GAME_MODE: u64 = 1;
    /// 0-indexed world number.
    pub const WORLD: u64 = 2;
    /// 0-indexed level ("dash") number.
    pub const LEVEL: u64 = 3;
    /// Bucketed absolute X.
    pub const X_BUCKET: u64 = 4;
    /// Current power-up state (a `set` register; not a cell-key input).
    pub const POWERUP: u64 = 5;
    /// Furthest `(world, level)` ordinal (a `state_max` register).
    pub const DEPTH: u64 = 6;
    /// The frame clock (task 87's film addresses frames by it; ignored by the
    /// cell key).
    pub const FRAME: u64 = 7;
    /// The billboard window's guest-physical address (published once in the
    /// setup prefix).
    pub const BILLBOARD_GPA: u64 = 8;
    /// The billboard window's total length.
    pub const BILLBOARD_LEN: u64 = 9;
}

/// The game campaign's budget + search knobs. The campaign is a pure function
/// of these plus the machine.
#[derive(Clone, Debug)]
pub struct GameCampaignConfig {
    /// Seeds the campaign stream.
    pub campaign_seed: u64,
    /// Search budget: exactly this many branches (identical across
    /// configurations — task 84's ruling).
    pub max_branches: u64,
    /// Each rollout runs this much V-time past the sealed base, if set (the
    /// play-agent never exits on its own — the deadline is the rollout
    /// terminal).
    pub deadline_delta: Option<u64>,
    /// SelectorV1 only: every Nth step explores fresh from the base; the rest
    /// exploit a novel admitted exemplar (the task-69 default shape).
    pub explore_period: u64,
    /// Base-seal retry step (V-time per snapshot attempt past a
    /// non-snapshottable boundary).
    pub snapshot_retry_step: u64,
    /// Base-seal retry budget.
    pub snapshot_max_attempts: usize,
    /// The V-time allowance for the guest to reach its `setup_complete`
    /// snapshot point from wherever the boot drive left it.
    pub setup_deadline_delta: u64,
    /// The sha256 of the ROM the image carries (`game-image` echoes it at
    /// build; `GAME_ROM_SHA256` on the boot serial). Stamped into the log so
    /// the offline report can refuse logs from a different dump. `None` =
    /// unstamped (the toy, or an operator who did not pass it).
    pub rom_sha256: Option<String>,
    /// Whether the base MUST seal at the play-agent's `setup_complete`
    /// snapshot point (round-5 P1). `true` on the box/real path: a guest that
    /// never reaches it is a dead agent (bad core/ROM/hugetlb//dev/mem
    /// provisioning), and sealing wherever it died would record a
    /// zero/constant-cell campaign — refuse loudly instead. `false` only for
    /// the portable no-SDK toy, whose quiescent terminal is its normal seal.
    /// Also gates the per-rollout terminal check (round-8 P1): in box mode a
    /// rollout that ends anywhere but its deadline DIED and fails the
    /// campaign loudly instead of being recorded as an ordinary sample.
    pub require_snapshot_point: bool,
    /// Record the deepest branch's reproducer (its canonical env + full
    /// journal) into this task-65 `TraceStore` directory (round-8 P1 / the
    /// hm-5sv day-one retention discipline): without it the campaign output
    /// is un-filmable (no `REG_FRAME` moments) and un-rekeyable (no env).
    /// Tracked from branch 0. `None` = no retention (portable smoke loops).
    pub trace_dir: Option<std::path::PathBuf>,
    /// The guest's RAM size — the range a published billboard window must lie
    /// inside (task 103 finding 2). Set from the composition root's actual
    /// guest RAM (`boxrun::GUEST_RAM_LEN`), so the bound the validator checks
    /// is the bound the VM was built with, single-sourced.
    pub guest_ram_len: u64,
    /// The two-barrier controller's per-step materialization cap (task 132):
    /// at most this many provisional candidates replay to a held seal per
    /// branch. Each replay costs a real rollout on the box, so the cap is
    /// deliberately small.
    pub candidate_cap: usize,
    /// The controller's total materialization-replay budget across the
    /// campaign (task 132). SelectorV1 needs admitted Entries to exploit;
    /// PureRandom never reads the archive, but the budget applies uniformly
    /// so both configurations run the identical two-barrier protocol.
    pub replay_budget: u64,
}

impl GameCampaignConfig {
    /// A small portable/smoke configuration (the no-SDK toy: the generic
    /// seal fallback is its normal path).
    pub fn smoke(campaign_seed: u64) -> Self {
        GameCampaignConfig {
            campaign_seed,
            max_branches: 64,
            deadline_delta: None,
            explore_period: 4,
            snapshot_retry_step: 1_000_000,
            snapshot_max_attempts: 100_000,
            setup_deadline_delta: 30_000_000_000,
            rom_sha256: None,
            require_snapshot_point: false,
            trace_dir: None,
            guest_ram_len: DEFAULT_GUEST_RAM_LEN,
            candidate_cap: 2,
            replay_budget: 64,
        }
    }
}

/// The guest RAM the box composition root boots with (2 GiB — `boxrun`'s
/// `GUEST_RAM_LEN`); the default billboard-range bound for configs that do not
/// state one. The box path sets [`GameCampaignConfig::guest_ram_len`]
/// explicitly from its own constant, so the two can never drift apart
/// silently — this default only serves the portable toy, which publishes no
/// billboard at all.
pub const DEFAULT_GUEST_RAM_LEN: u64 = 2 << 30;

/// The shortest billboard window film can actually use: **`film::HEADER_LEN`
/// (32 bytes)** — the fixed header carrying the frame identity and the region
/// table, below which `FilmPlan::derive` refuses the window outright
/// (`PlanError::BillboardTooSmall`).
///
/// A published window that is nonzero and inside guest RAM can still be
/// **unusable**, and on the box film is M0's mandatory artifact — so a
/// `(gpa, len = 1)` window would otherwise green the determinism gate over a
/// clip that cannot be produced (round-3 finding). Validating against it is
/// the same "a malformed window is as fatal as a missing one" rule.
///
/// **Mirrored, not imported:** `film` is a *dev*-dependency of this crate (the
/// box film gate lives in `tests/live_film.rs`), and the campaign driver must
/// not take a library dependency on the renderer to validate a window. The
/// value is pinned against film's own constant by a dev-dep drift test
/// (`billboard_min_len_tracks_films_header`), so a change on film's side breaks
/// a test here rather than silently re-opening the hole.
pub const BILLBOARD_MIN_LEN: u64 = 32;

/// Why a game campaign refused to run or died.
#[derive(Debug, thiserror::Error)]
pub enum GameCampaignError {
    /// A machine (transport/backend) failure.
    #[error(transparent)]
    Machine(#[from] MachineError),
    /// The Signal configuration was requested but no selector artifact exists
    /// to test (task 69 M2 ruled NO-GO; the M1 referendum is queued behind a
    /// task-70 successor). Refused loudly — running the default search under
    /// the "signal" label would fake the held-out test.
    #[error(
        "the Signal configuration needs task 70's selector artifact, which does not exist \
         (task 69 M2 NO-GO); M0 runs PureRandom / SelectorV1 only"
    )]
    SignalUnavailable,
    /// The guest never surfaced its `setup_complete` snapshot point — a dead
    /// play-agent (bad core/ROM provisioning, hugetlb, `/dev/mem`), whose
    /// terminal reached us instead. Refused loudly: sealing a dead base would
    /// record a zero/constant-cell campaign (the vacuity class).
    #[error(
        "the play-agent never reached setup_complete (guest stopped with {stop}) — bad \
         provisioning (core/ROM/hugetlb//dev/mem)?; refusing to seal a dead base (the campaign \
         would be vacuous). Check the boot serial for GAME_SKIP / play-agent: FATAL lines."
    )]
    SetupNotReached {
        /// The stop the guest surfaced instead of the snapshot point.
        stop: String,
    },
    /// An exemplar env inside the quiet campaign carries fault vocabulary
    /// (host actions or standing faults) — a contradiction of the quiet arm
    /// (round-6 P1), surfaced loudly rather than propagated into further
    /// branches.
    #[error(
        "quiet-arm exemplar env carries fault vocabulary ({what}) — the game campaign is \
         FaultPolicy::none() / zero fault vocabulary; refusing to mutate it"
    )]
    QuietEnvCarriesFaults {
        /// What was found (host actions / standing faults).
        what: &'static str,
    },
    /// An exemplar env failed to decode — a malformed blob in the frontier.
    #[error("quiet-arm exemplar env failed to decode: {0}")]
    MalformedExemplar(String),
    /// The box setup prefix never published a valid billboard `(gpa, len)`
    /// window (round-9 P1): the billboard is M0's unconditional film seam, so
    /// a campaign with no window has nothing film can consume — refused
    /// loudly rather than reported as a windowless "success".
    #[error(
        "the setup prefix published no valid billboard (gpa, len) window — the billboard is \
         unconditional on the box (film's input); check the play-agent's REG_BILLBOARD_GPA/LEN \
         emission before setup_complete"
    )]
    BillboardMissing,
    /// The setup prefix published a billboard window that cannot name real
    /// guest memory (task 103 finding 2): zero-length, null-gpa, overflowing,
    /// past the end of guest RAM, or only half-published. A malformed window
    /// is the same loud failure class as a missing one — film would read
    /// nothing (or the wrong bytes), and a campaign that "passed" over it
    /// would be vacuous. Never a silent fallback to no-billboard.
    #[error(
        "the setup prefix published a MALFORMED billboard window (gpa={gpa:#x}, len={len}): \
         {why}. A billboard that cannot name real guest memory is as fatal as a missing one \
         (film reads it); check the play-agent's REG_BILLBOARD_GPA/LEN emission and its \
         hugetlb/pagemap pinning"
    )]
    BillboardMalformed {
        /// The guest-physical address as published.
        gpa: u64,
        /// The length as published.
        len: u64,
        /// Which validity rule it broke.
        why: &'static str,
    },
    /// A box-mode rollout ended somewhere other than its deadline — the
    /// play-agent DIED mid-rollout (round-8 P1; e.g. `retro_serialize`
    /// failing on a frame). Crashed rollouts must never be recorded as
    /// ordinary samples: a campaign — the determinism gate included — could
    /// "pass" over identically-crashed runs.
    #[error(
        "rollout {branch} died: guest stopped with {stop} instead of its deadline — refusing to \
         record a dead rollout as a sample"
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
    /// The two-barrier Differential controller failed (coordinator, evidence
    /// ledger, codec, or materialized-view failure) — loud, never absorbed
    /// (task 132).
    #[error("differential campaign: {0}")]
    Campaign(#[from] CampaignError),
}

/// The deepest branch's retained reproducer pointer (round-8 P1): which
/// branch, how deep, and — when a [`GameCampaignConfig::trace_dir`] was
/// given — the recorded `TraceId` (content-addressed; the env sidecar + full
/// journal live in the store).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeepReproducer {
    /// The deepest branch's index (first-deepest on ties; tracked from
    /// branch 0).
    pub branch: u64,
    /// Its depth (the `REG_DEPTH` max).
    pub depth: u64,
    /// The recorded trace id, when retention was configured.
    pub trace_id: Option<String>,
}

/// What a game campaign produced: the offline-analysis log, the retained deep
/// reproducer, the billboard window the guest published in its setup prefix
/// (film's `FilmPlan` inputs, surfaced from campaign output alone), and the
/// **work evidence** the gate checks before it may print a verdict.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GameCampaignOutcome {
    /// The discovery-event log (`--logs-out`'s artifact).
    pub log: ExplorationLog,
    /// The deepest branch (present whenever at least one branch ran).
    pub deep: Option<DeepReproducer>,
    /// The billboard `(gpa, len)` from the setup prefix's `REG_BILLBOARD_*`
    /// registers, when the guest published them (the toy does not).
    pub billboard: Option<(u64, u64)>,
    /// Proof this campaign actually ran a workload (task 103 finding 1).
    pub work: WorkEvidence,
}

/// What a campaign has to show for itself: how many branches it rolled out,
/// and the *weakest* rollout among them by V-time advanced past the sealed
/// base and by guest frames rendered (task 103 finding 1b).
///
/// The determinism gate compares per-branch `state_hash` sequences across
/// repetitions. That comparison is only meaningful if the repetitions did
/// something: a zero branch budget compares two empty sequences, and a zero
/// (or absurdly small) rollout deadline compares the sealed base to itself —
/// both "identical" 25/25, both measuring nothing. So the campaign carries its
/// evidence out with it and [`GameCampaignOutcome::vacuity`] refuses the
/// verdict when the evidence is absent, no matter which flag combination
/// produced it.
///
/// Minima, not totals: one hollow rollout in an otherwise busy campaign is
/// still a hollow rollout, and a total would hide it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct WorkEvidence {
    /// Branches actually rolled out.
    pub branches: u64,
    /// The least V-time any rollout advanced past the sealed base — the
    /// guest's own clock moved, so instructions retired.
    pub min_vtime_span: u64,
    /// The least number of frame bodies any rollout **executed**
    /// ([`smb_completed_frames`] — `REG_FRAME` *transitions*, since the agent
    /// writes that register before running the frame): the game's frame clock
    /// advanced, so the *workload* ran, not merely the guest.
    pub min_completed_frames: u64,
}

/// Why a campaign's evidence does not support any gate verdict — each variant
/// is a way to print `DETERMINISM PASS` over a run that did no work.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Vacuity {
    /// Zero branches ran (`--max-branches 0`): the gate would compare two
    /// empty `state_hash` sequences and find them identical.
    #[error(
        "the campaign rolled out ZERO branches — an empty state_hash sequence is trivially \
         identical to another empty one; there is no determinism claim here"
    )]
    NoBranches,
    /// A rollout advanced no V-time (`--deadline-delta 0`): its deadline was
    /// already met at the base, so the gate hashed the sealed base itself.
    #[error(
        "a rollout advanced ZERO V-time past the sealed base — its deadline was already met at \
         the base, so the campaign hashed the base instead of a rollout"
    )]
    NoVTime,
    /// A rollout executed no game frame: V-time moved, but not far enough for
    /// the workload to complete a single frame body (a deadline below one
    /// frame — including one that expired after the agent wrote its pre-run
    /// `REG_FRAME` marker but before the frame it announced actually ran).
    #[error(
        "a rollout COMPLETED zero game frames (no REG_FRAME transition — a lone frame marker is \
         written BEFORE its frame runs, so it proves nothing) — the guest ran, but the workload \
         never advanced a frame, so the campaign measured no gameplay"
    )]
    NoFrames,
}

impl GameCampaignOutcome {
    /// The gate's vacuity guard (task 103 finding 1b): `Some(reason)` when this
    /// campaign's evidence cannot support a verdict.
    ///
    /// Checked by the gate **before** any PASS banner, so a degenerate budget
    /// fails loudly even if it slipped past the CLI's input validation — a new
    /// flag, a new default, or a library caller are all ways past `--max-branches
    /// 0`-style rejection, and none of them may reach a green gate.
    pub fn vacuity(&self) -> Option<Vacuity> {
        if self.work.branches == 0 {
            return Some(Vacuity::NoBranches);
        }
        if self.work.min_vtime_span == 0 {
            return Some(Vacuity::NoVTime);
        }
        if self.work.min_completed_frames == 0 {
            return Some(Vacuity::NoFrames);
        }
        None
    }
}

/// The **quiet-arm mutator** (round-6 P1): the input-only exploit move.
///
/// `SpecEnvCodec::mutate` is the wrong tool here — its documented behavior
/// inserts a host-plane `Action::Host` override, i.e. a **fault**, and this
/// campaign is the quiet arm (`FaultPolicy::none()`, zero fault vocabulary).
/// The quiet arm's one legitimate exploration dimension is the **entropy
/// stream** the guest draws its inputs from, and the codebase already has the
/// exact vocabulary for perturbing it: the task-78 **reseed markers**. This
/// mutator re-encodes the exemplar's env with ONE added reseed marker at a
/// salt-derived `Moment` inside `window`: the branch replays the exemplar's
/// inputs up to that Moment (reaching its discovered state), then draws fresh
/// entropy — coverage-guided branching, faults-off.
///
/// **Structural exclusion, not convention:** this function never constructs
/// an [`environment::Action`] of any kind — it copies the exemplar's existing
/// override map verbatim and touches only the reseed table — so it *cannot*
/// produce `Action::Host`. Defense in depth: an exemplar that already carries
/// host actions or standing faults (impossible in a quiet campaign; a defect
/// if seen) is refused loudly, so fault vocabulary can never propagate
/// through the frontier.
pub fn quiet_mutate(
    env: &Reproducer,
    salt: u64,
    window: u64,
) -> Result<Reproducer, GameCampaignError> {
    let decoded = explorer::AdapterEnv::decode(env)
        .map_err(|e| GameCampaignError::MalformedExemplar(e.to_string()))?;
    if decoded.spec.host_faults().next().is_some() {
        return Err(GameCampaignError::QuietEnvCarriesFaults {
            what: "host actions",
        });
    }
    if let environment::EnvSpec::Recorded { standing, .. } = &decoded.spec
        && !standing.is_empty()
    {
        return Err(GameCampaignError::QuietEnvCarriesFaults {
            what: "standing faults",
        });
    }

    let seed = decoded.spec.seed();
    // The salt-derived reseed Moment, strictly inside the rollout window.
    let at = 1 + salt % window.max(1);
    let mut reseeds = decoded.spec.reseeds().clone();
    reseeds.insert(at, salt);
    // The floor-marker discipline (task 78 / PR #62): a non-empty reseed
    // table must carry its origin marker, else the branch would continue the
    // parent stream instead of starting at `seed`.
    reseeds.entry(0).or_insert(seed);

    let spec = environment::EnvSpec::Recorded {
        seed,
        policy: decoded.spec.policy().clone(),
        // The exemplar's guest-plane decision history, verbatim (proven
        // host-action-free above); no Action is ever minted here.
        overrides: decoded.spec.overrides().clone(),
        standing: Vec::new(),
        reseeds,
    };
    Ok(explorer::AdapterEnv {
        base_offset: decoded.base_offset,
        pos: decoded.pos,
        spec,
    }
    .encode())
}

/// Pack the `(game mode, world, level, x-bucket)` tuple into one 64-bit cell
/// key: mode and world and level in the top bytes, the bucket below. Injective
/// for mode/world/level ≤ 255 and buckets < 2^40 (SMB values are far inside).
pub fn smb_cell_key(mode: u64, world: u64, level: u64, x_bucket: u64) -> u64 {
    ((mode & 0xFF) << 56)
        | ((world & 0xFF) << 48)
        | ((level & 0xFF) << 40)
        | (x_bucket & 0xFF_FFFF_FFFF)
}

/// Fold a run's `NS_STATE` SDK firings into the branch's cell set and depth:
/// read each `Payload::State` firing's `(reg, value)` off its `Point` identity
/// (`reg = local`), track the last-seen value per tuple register, mint one cell
/// key at each `X_BUCKET` observation (the tuple's final register each window),
/// and take the max `DEPTH` value. Returns the branch's **distinct** cell keys
/// (sorted) and its depth.
pub fn smb_cells(events: &[SdkEvent]) -> (Vec<u64>, u64) {
    let mut cells = BTreeSet::new();
    let mut depth = 0u64;
    let (mut mode, mut world, mut level) = (0u64, 0u64, 0u64);
    for ev in events {
        let Payload::State { value, .. } = &ev.payload else {
            continue;
        };
        let ObservationId::Point { namespace, local } = &ev.id else {
            continue;
        };
        if *namespace != NS_STATE {
            continue;
        }
        let (reg, value) = (*local as u64, *value);
        match reg {
            reg::GAME_MODE => mode = value,
            reg::WORLD => world = value,
            reg::LEVEL => level = value,
            reg::X_BUCKET => {
                cells.insert(smb_cell_key(mode, world, level, value));
            }
            reg::DEPTH => depth = depth.max(value),
            _ => {}
        }
    }
    (cells.into_iter().collect(), depth)
}

/// Count the frame bodies a rollout actually **executed** — the workload's own
/// proof of life, and the evidence the vacuity guard rests on (task 103
/// finding 1b).
///
/// **Transitions of `REG_FRAME`, not observations of it.** The play-agent
/// writes the frame register *before* running the frame body
/// (`harmony-linux/play-agent/src/agent.rs`: `state_set(REG_FRAME, frame)` then
/// `core.run_frame`), because film addresses that frame's billboard by that
/// Moment and must see the frame's bytes at it. So a lone `REG_FRAME`
/// observation proves only that the guest wrote a marker and then the deadline
/// expired — **zero frames ran**. It takes a *second*, different value to prove
/// the first frame's body completed and the loop came back around.
///
/// Counting observations instead would let a deadline that expires between the
/// first marker and the first frame body report one frame and sail through the
/// guard — precisely the positive-but-too-small budget the guard exists to
/// reject. The emission order is not the bug and must not be "fixed" in the
/// agent: it is film's addressing contract, and reordering it would change the
/// recorded stream.
///
/// Any change in value counts, not just an increase: a changed value means the
/// agent went round the loop again, which is what "a frame ran" means. (The
/// frame clock is a `u32` the agent wraps, so demanding monotonicity would
/// eventually call a real frame no frame.)
pub fn smb_completed_frames(events: &[SdkEvent]) -> u64 {
    let mut completed = 0u64;
    let mut last: Option<u64> = None;
    for ev in events {
        let Payload::State { value, .. } = &ev.payload else {
            continue;
        };
        let ObservationId::Point { namespace, local } = &ev.id else {
            continue;
        };
        if *namespace != NS_STATE || *local as u64 != reg::FRAME {
            continue;
        }
        let value = *value;
        if last.is_some_and(|prev| prev != value) {
            completed += 1;
        }
        last = Some(value);
    }
    completed
}

/// The binary-wire catalog marker event id (mirror of the sdk-events wire
/// constant; the conventions mirror-type pattern — a raw tuple with this id
/// is the schema declaration, not a firing).
const CATALOG_EVENT_ID: u32 = 0;

/// The SMB workload's base-operation resolution table (task 132): each
/// play-agent state register the campaign reduces, with its declared base
/// update operation. The host owns the workload contract already (the
/// mirrored [`reg`] catalog); this states it as data for the explicit
/// instrumentation declaration.
fn smb_resolution() -> Vec<(ObservationId, sdk_events::UpdateOp)> {
    use sdk_events::UpdateOp;
    let point = |local: u64| ObservationId::Point {
        namespace: NS_STATE,
        local: local as u32,
    };
    vec![
        (point(reg::GAME_MODE), UpdateOp::Set),
        (point(reg::WORLD), UpdateOp::Set),
        (point(reg::LEVEL), UpdateOp::Set),
        (point(reg::X_BUCKET), UpdateOp::Set),
        (point(reg::POWERUP), UpdateOp::Set),
        (point(reg::DEPTH), UpdateOp::Max),
        (point(reg::FRAME), UpdateOp::Set),
        (point(reg::BILLBOARD_GPA), UpdateOp::Set),
        (point(reg::BILLBOARD_LEN), UpdateOp::Set),
    ]
}

/// The SMB workload's **explicit instrumentation declaration** for a guest
/// with NO catalog of its own (the portable toys): a wire-v2 catalog
/// resolving each register in [`smb_resolution`]. The strategy forbids
/// silently blessing v1 firings with reducers ("the first Differential
/// vertical cannot silently bless inference from v1 events") and sanctions
/// exactly this — "an equally explicit workload instrumentation
/// declaration" — as the alternative to a guest wire-v2 rebuild.
fn smb_instrumentation_catalog() -> Vec<u8> {
    use sdk_events::{Classification, DeclaredPoint, ValueShape};
    let points: Vec<DeclaredPoint> = smb_resolution()
        .into_iter()
        .filter_map(|(id, op)| match id {
            ObservationId::Point { namespace, local } => Some(DeclaredPoint {
                namespace,
                local,
                name: format!("smb_reg_{local}"),
                classification: Classification::State,
                value_shape: Some(ValueShape::U64),
                base_op: Some(op),
                expectation: None,
            }),
            _ => None,
        })
        .collect();
    // Statically infallible: a fixed, well-formed catalog literal.
    sdk_events::encode_v2_declaration(&points)
        .expect("the SMB instrumentation catalog is well-formed")
}

/// A [`Machine`] adapter carrying the SMB instrumentation declaration
/// (task 132): every drained capture resolves the play-agent's state
/// registers to their declared base operations, so the normalized evidence
/// is reducible by the Differential relations. A guest that declared its own
/// (wire-v1) catalog has that declaration **upgraded in place**
/// ([`sdk_events::resolve_v1_declaration`] — the guest's declared points,
/// expectations, and names are preserved through the decoder's own parsing);
/// a guest with no catalog (the portable toys) has the standalone
/// declaration prepended. Everything else delegates.
struct DeclaredMachine<M> {
    inner: M,
    catalog: Vec<u8>,
}

impl<M: Machine> DeclaredMachine<M> {
    fn new(inner: M) -> Self {
        DeclaredMachine {
            inner,
            catalog: smb_instrumentation_catalog(),
        }
    }
}

impl<M: Machine> Machine for DeclaredMachine<M> {
    fn branch(&mut self, snap: explorer::SnapId, env: &Reproducer) -> Result<(), MachineError> {
        self.inner.branch(snap, env)
    }
    fn replay(&mut self, snap: explorer::SnapId) -> Result<(), MachineError> {
        self.inner.replay(snap)
    }
    fn run(
        &mut self,
        until: &StopConditions,
        resolve: Option<&explorer::Answer>,
    ) -> Result<StopReason, MachineError> {
        self.inner.run(until, resolve)
    }
    fn snapshot(&mut self) -> Result<(explorer::SnapId, EvidenceCut), MachineError> {
        self.inner.snapshot()
    }
    fn drop_snap(&mut self, snap: explorer::SnapId) -> Result<(), MachineError> {
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
            // The guest declared its own catalog: upgrade it in place with
            // the resolution table (a malformed guest catalog is the same
            // transport-class failure a malformed capture is).
            Some((_, _, bytes)) => {
                *bytes =
                    sdk_events::resolve_v1_declaration(bytes, &smb_resolution()).map_err(|e| {
                        MachineError::Transport(format!(
                            "guest catalog failed to upgrade to the instrumentation \
                             declaration: {e}"
                        ))
                    })?;
            }
            // No guest catalog (the portable toys): prepend the standalone
            // declaration.
            None => out.insert(0, (0u64, CATALOG_EVENT_ID, self.catalog.clone())),
        }
        Ok(out)
    }
    fn console(&mut self) -> Result<Vec<(u64, Vec<u8>)>, MachineError> {
        self.inner.console()
    }
}

/// The quiet-arm [`EnvCodec`] the routed campaign hands the two-barrier
/// controller (task 132): `seeded`/`compose` delegate to the production
/// codec; `mutate` is the [`quiet_mutate`] reseed-only move — the controller
/// structurally cannot mint a host-plane fault on exploit. A fault-carrying
/// exemplar maps onto the codec's fail-closed
/// [`UnsupportedComposition`](explorer::EnvCodecError::UnsupportedComposition)
/// class (a standing-fault-carrying input is literally its documented case).
struct QuietCodec {
    inner: Box<dyn EnvCodec>,
    /// The reseed-Moment window `quiet_mutate` places its marker inside.
    window: u64,
}

impl EnvCodec for QuietCodec {
    fn seeded(&self, seed: u64) -> Reproducer {
        self.inner.seeded(seed)
    }

    fn mutate(&self, base: &Reproducer, salt: u64) -> Result<Reproducer, explorer::EnvCodecError> {
        quiet_mutate(base, salt, self.window).map_err(|e| match e {
            GameCampaignError::QuietEnvCarriesFaults { .. } => {
                explorer::EnvCodecError::UnsupportedComposition
            }
            // A malformed exemplar blob (the untrusted-input class).
            _ => explorer::EnvCodecError::Malformed(0),
        })
    }

    fn compose(
        &self,
        base: &Reproducer,
        branch_local: &Reproducer,
    ) -> Result<Reproducer, explorer::EnvCodecError> {
        self.inner.compose(base, branch_local)
    }
}

/// The SMB cell projection over the reduced observation map (task 132): the
/// same `(game mode, world, level, x-bucket)` tuple [`smb_cells`] keys, read
/// off the independently-reduced registers at the evaluation point's cut. A
/// state with no `X_BUCKET` observation yet keys to the empty cell (the
/// shared pre-progress state, exactly like [`smb_cells`] minting no key
/// before the first X write).
pub struct SmbObservationCells;

impl ObservationCells for SmbObservationCells {
    fn key(&self, _cut: EvidenceCut, obs: &ObservationMap) -> CellKey {
        let get = |reg: u64| match obs.get(&ObservationId::Point {
            namespace: NS_STATE,
            local: reg as u32,
        }) {
            Some(ReducedValue::Scalar(v)) => Some(*v),
            _ => None,
        };
        match get(reg::X_BUCKET) {
            None => Vec::new(),
            Some(x) => smb_cell_key(
                get(reg::GAME_MODE).unwrap_or(0),
                get(reg::WORLD).unwrap_or(0),
                get(reg::LEVEL).unwrap_or(0),
                x,
            )
            .to_le_bytes()
            .to_vec(),
        }
    }
}

/// Drive one game campaign against `machine` under `config`, **end-to-end
/// through the two-barrier [`DifferentialCampaign`] controller** (task 132,
/// `hm-e6q`). Seals the base at the play-agent's `setup_complete` snapshot
/// point (billboard primed + published, ROM running), then hands the machine
/// to the controller: per branch, one two-barrier step — the selector picks
/// genesis (PureRandom) or a retained Entry (SelectorV1 exploit, via the
/// quiet reseed-only mutate), the rollout's normalized SDK evidence commits
/// through the Revision coordinator, provisional cells nominate budgeted
/// materialization replay, and occupancy admits at the actual `sealed_at`.
/// The per-branch log (cells touched, depth, terminal `state_hash`) is
/// derived from the committed evidence batches; the deepest branch's
/// reproducer is retained ([`GameCampaignConfig::trace_dir`]) and the setup
/// prefix's billboard window surfaced — film's inputs, from campaign output
/// alone.
pub fn run_game_campaign<M: Machine>(
    machine: M,
    codec: Box<dyn EnvCodec>,
    cfg: &GameCampaignConfig,
    config: ExplorationConfig,
) -> Result<GameCampaignOutcome, GameCampaignError> {
    if config == ExplorationConfig::Signal {
        return Err(GameCampaignError::SignalUnavailable);
    }

    // The workload's explicit instrumentation declaration rides every SDK
    // capture from here on (task 132): the schema that resolves the
    // play-agent's state registers to their base operations.
    let mut machine = DeclaredMachine::new(machine);

    let (base, base_vtime) = seal_base(&mut machine, cfg)?;
    // Drain the SETUP prefix's SDK capture: the billboard gpa/len registers
    // were published before the seal, so they ride this capture, not any
    // branch's — surface them for film (round-8 P1). Also keeps setup events
    // out of branch 0's trace. The window is VALIDATED at registration (task
    // 103 finding 2): a published-but-unusable window is a loud refusal here,
    // on both paths, never a silent fall back to "no billboard".
    let setup_events = drain_events(&mut machine)?;
    let billboard = billboard_window_of(&setup_events.events, cfg.guest_ram_len)?;
    // Round-9 P1: on the box (`require_snapshot_point`) the billboard is
    // M0's unconditional film seam — a setup prefix that never published a
    // valid `(gpa, len)` window means film has no input, so the campaign
    // must not proceed to a "determinism PASS" with nothing to show for it.
    // The portable no-SDK toy path (`require_snapshot_point = false`) stays
    // billboard-tolerant.
    if cfg.require_snapshot_point && billboard.is_none() {
        return Err(GameCampaignError::BillboardMissing);
    }
    // The controller snapshots its own genesis from this same stopped state;
    // the seal_base handle would only duplicate it.
    machine.drop_snap(base)?;

    let until = StopConditions {
        deadline: cfg
            .deadline_delta
            .map(|d| Moment(base_vtime.saturating_add(d))),
        // Quiet arm: no decisions surface, no assertion is a bug here (the
        // agent's markers are reachable HITS, which never stop a run); the
        // deadline (or the guest's own terminal) bounds the rollout.
        on: StopMask::NONE,
    };

    // The search policies, mapped onto the controller's seams: PureRandom is
    // the always-explore selector; SelectorV1 is the explore/exploit shape
    // over the controller's retained Entries.
    let selector: Box<dyn Selector> = match config {
        ExplorationConfig::PureRandom => Box::new(GenesisSelector::new()),
        ExplorationConfig::SelectorV1 => {
            Box::new(ExploreExploitSelector::new().with_explore_period(cfg.explore_period))
        }
        // Refused above; structurally unreachable, kept total.
        ExplorationConfig::Signal => return Err(GameCampaignError::SignalUnavailable),
    };
    let quiet = QuietCodec {
        inner: codec,
        window: cfg.deadline_delta.unwrap_or(TOY_RESEED_WINDOW),
    };
    // The durable evidence ledger: beside the retained traces when a trace
    // directory is configured, else a campaign-lifetime scratch file.
    let scratch;
    let evidence_path = match &cfg.trace_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .map_err(|e| GameCampaignError::Retention(e.to_string()))?;
            dir.join("evidence.log")
        }
        None => {
            scratch =
                tempfile::tempdir().map_err(|e| GameCampaignError::Retention(e.to_string()))?;
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
        Box::new(SmbObservationCells),
        ledger,
        coordinator,
        explorer::CampaignConfig {
            candidate_cap: cfg.candidate_cap,
            replay_budget: cfg.replay_budget,
            ingress: Ingress::Binary,
            retention: RetentionProfile::Full,
            evidence_budget: None,
            // The quiet game rollout surfaces no mid-run snapshot points —
            // nomination coordinates come from the rollout's own SDK-event
            // moments (the configured-evidence-cut source).
            nominate: Nomination::EventMoments,
            // The per-branch determinism artifact the 25/25 gate compares.
            hash_rollouts: true,
        },
        cfg.campaign_seed,
    )?;
    camp.set_stop_conditions(until);

    let mut events = Vec::new();
    // The deepest branch's (depth, branch, trace), tracked from branch 0
    // (round-8 P1 / hm-5sv retention discipline); strictly-greater keeps the
    // first-deepest.
    let mut deepest: Option<(u64, u64, RunTrace)> = None;
    // The gate's work evidence (task 103 finding 1b): the weakest rollout in
    // the campaign, so one hollow branch cannot hide behind 63 busy ones.
    let mut work = WorkEvidence {
        branches: 0,
        min_vtime_span: u64::MAX,
        min_completed_frames: u64::MAX,
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
                GameCampaignError::Retention("rollout revision has no committed batch".into())
            })?;
        let ev =
            camp.ledger().get(&batch).cloned().ok_or_else(|| {
                GameCampaignError::Retention("committed batch not in ledger".into())
            })?;
        // Round-8 P1: in box mode the play-agent never exits — a rollout that
        // ended anywhere but its deadline DIED (crash/halt mid-rollout) and
        // must fail the campaign loudly, never be hashed and recorded like an
        // ordinary sample (the determinism gate could "pass" over
        // identically-crashed runs). The toy's natural terminal is Quiescent.
        if cfg.require_snapshot_point && !matches!(ev.terminal, StopReason::Deadline { .. }) {
            return Err(GameCampaignError::RolloutDied {
                branch,
                stop: format!("{:?}", ev.terminal),
            });
        }
        // The rollout's own work evidence: how far the guest's clock advanced
        // past this branch's own start (the sealed base for a genesis branch,
        // the exploited Entry's cut for a child — its suffix is its work).
        let branch_start = ev.parent_cut.map(|c| c.at.0).unwrap_or(base_vtime);
        let vtime_span = ev.terminal.vtime().0.saturating_sub(branch_start);
        // Statically infallible: `hash_rollouts` is set in the controller
        // config above, so every report carries the terminal hash.
        let state_hash = report.state_hash.expect("hash_rollouts is configured");
        let (touched, depth) = smb_cells(&ev.normalized.events);
        let completed_frames = smb_completed_frames(&ev.normalized.events);
        // Round-9 P1: the retained trace carries the batch's RECORDED,
        // genesis-complete environment — never a pre-run proposal (the R1
        // discipline).
        let trace = RunTrace {
            terminal: ev.terminal.clone(),
            env: ev.env.clone(),
            coverage: None,
            events: crate::sdk_compat::guest_events_of(&ev.normalized),
            records: Vec::new(),
        };
        work.branches += 1;
        work.min_vtime_span = work.min_vtime_span.min(vtime_span);
        work.min_completed_frames = work.min_completed_frames.min(completed_frames);

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

    // Retain the deep reproducer (env sidecar + full journal — the REG_FRAME
    // moments film derives its shot list from ride the journal's events).
    let deep = match deepest {
        None => None,
        Some((depth, branch, trace)) => {
            let trace_id = match &cfg.trace_dir {
                None => None,
                Some(dir) => {
                    let store = runtrace::TraceStore::open(dir)
                        .map_err(|e| GameCampaignError::Retention(e.to_string()))?;
                    let id = store
                        .record(&trace, runtrace::Retain::Full)
                        .map_err(|e| GameCampaignError::Retention(e.to_string()))?;
                    Some(id.to_hex())
                }
            };
            Some(DeepReproducer {
                branch,
                depth,
                trace_id,
            })
        }
    };

    Ok(GameCampaignOutcome {
        log: ExplorationLog {
            workload: "smb".to_string(),
            rom_sha256: cfg.rom_sha256.clone(),
            config,
            seed: cfg.campaign_seed,
            events,
        },
        deep,
        billboard,
        // No branch ran ⇒ there are no minima; report zeroes rather than the
        // `u64::MAX` sentinels, so the evidence reads as the nothing it is.
        work: if work.branches == 0 {
            WorkEvidence::default()
        } else {
            work
        },
    })
}

/// The task-86 gate-2 determinism floor: 25 bit-identical repetitions.
pub const DETERMINISM_BAR: usize = 25;

/// What the determinism gate concluded from `repeat` bit-identical campaigns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GateVerdict {
    /// At/above the floor, with work evidence: the task-86 gate-2 PASS.
    Pass {
        /// The repetitions that came back bit-identical.
        repeat: usize,
    },
    /// Bit-identical, but below the [`DETERMINISM_BAR`] — a smoke, and it must
    /// say so: it is NOT the gate (round-8 P1).
    BelowFloor {
        /// The repetitions run.
        repeat: usize,
    },
    /// One campaign: nothing was compared, so nothing is claimed.
    Single,
}

impl GateVerdict {
    /// The operator-facing line for this verdict (`None` for [`Self::Single`],
    /// which claims nothing and so says nothing).
    pub fn banner(&self) -> Option<String> {
        match self {
            GateVerdict::Pass { repeat } => Some(format!(
                "[campaign-runner] game box DETERMINISM PASS: {repeat}/{repeat} identical per-branch \
                 state_hash sequences (gate floor {DETERMINISM_BAR})."
            )),
            GateVerdict::BelowFloor { repeat } => Some(format!(
                "[campaign-runner] game box determinism smoke: {repeat}/{repeat} identical — BELOW the \
                 {DETERMINISM_BAR}-run gate floor, NOT the task-86 determinism gate."
            )),
            GateVerdict::Single => None,
        }
    }
}

/// Decide the determinism gate over a campaign that came back bit-identical
/// `repeat` times — the box driver's verdict, hoisted here so it is portable
/// and testable (the driver itself needs `/dev/kvm`).
///
/// **The vacuity guard is this function's whole point** (task 103 finding 1b):
/// bit-identical repetitions of a campaign that did no work are still
/// bit-identical, so identity alone can never be the gate's evidence. A
/// campaign with no branches, no V-time, or no frames yields `Err(Vacuity)`
/// here and the driver fails loudly — there is no path from a hollow run to a
/// PASS banner, whatever `repeat` says and whatever flags produced it.
pub fn determinism_verdict(
    outcome: &GameCampaignOutcome,
    repeat: usize,
) -> Result<GateVerdict, Vacuity> {
    if let Some(v) = outcome.vacuity() {
        return Err(v);
    }
    Ok(match repeat {
        r if r >= DETERMINISM_BAR => GateVerdict::Pass { repeat: r },
        0 | 1 => GateVerdict::Single,
        r => GateVerdict::BelowFloor { repeat: r },
    })
}

/// Drain + decode the machine's SDK event capture into normalized evidence (the
/// task-73 seam). The game toy's stream is clean, so a decode error can only
/// mean a corrupt capture reached us over the control seam — surface it as the
/// transport-class control error it is (there is precedent for treating a
/// malformed capture as a control failure), never a panic.
fn drain_events<M: Machine>(machine: &mut M) -> Result<Normalized, MachineError> {
    let raw = machine.sdk_events()?;
    crate::sdk_compat::decode_sdk(&raw)
        .map_err(|e| MachineError::Transport(format!("SDK capture failed to decode: {e}")))
}

/// Extract the booted image's ROM sha256 from its boot serial: the last
/// `GAME_ROM_SHA256: <hex>` line game-init prints (from the hash baked in at
/// image-build time) before `GAME_READY`. Returns the lowercased hex, or
/// `None` when the transcript carries no such line (a ROM-less image prints
/// `GAME_SKIP` instead). The box driver cross-checks this **fact** against
/// the operator's `--rom-sha256` **claim** and refuses a mismatch (round-9
/// P1 — the content-hash discipline).
pub fn serial_rom_sha256(serial: &[u8]) -> Option<String> {
    const TAG: &[u8] = b"GAME_ROM_SHA256:";
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
        let rest = &serial[at + TAG.len()..];
        let hex: Vec<u8> = rest
            .iter()
            .copied()
            .skip_while(|b| *b == b' ')
            .take_while(u8::is_ascii_hexdigit)
            .collect();
        if hex.len() == 64 {
            // Guest bytes are untrusted input, but a 64-hexdigit run is ASCII
            // by construction.
            found = Some(
                String::from_utf8(hex)
                    .expect("hex digits are ASCII")
                    .to_lowercase(),
            );
        }
        from = at + TAG.len();
    }
    found
}

/// Extract the billboard `(gpa, len)` from the setup prefix's decoded state
/// events (the last write of each register wins — the agent publishes once)
/// and **validate it at registration** (task 103 finding 2).
///
/// `Ok(None)` means the guest published no billboard at all (the portable
/// toy); the box's `require_snapshot_point` gate turns that into
/// [`GameCampaignError::BillboardMissing`]. Anything published but unusable —
/// a half-publish, a zero length, a null gpa, a range that overflows or runs
/// past the end of guest RAM — is [`GameCampaignError::BillboardMalformed`],
/// the same loud class: the round-8 missing-billboard check only tested for
/// *presence*, so a guest that published `len = 0` (or a wild gpa) slipped
/// through it into a campaign whose film seam reads nothing.
fn billboard_window_of(
    events: &[SdkEvent],
    guest_ram_len: u64,
) -> Result<Option<(u64, u64)>, GameCampaignError> {
    let (mut gpa, mut len) = (None, None);
    for ev in events {
        let Payload::State { value, .. } = &ev.payload else {
            continue;
        };
        let ObservationId::Point { namespace, local } = &ev.id else {
            continue;
        };
        if *namespace != NS_STATE {
            continue;
        }
        match *local as u64 {
            reg::BILLBOARD_GPA => gpa = Some(*value),
            reg::BILLBOARD_LEN => len = Some(*value),
            _ => {}
        }
    }
    let (gpa, len) = match (gpa, len) {
        (None, None) => return Ok(None),
        (Some(gpa), Some(len)) => (gpa, len),
        // A half-publish is a broken agent, not an absent billboard: the two
        // registers are written together in the setup prefix.
        (gpa, len) => {
            return Err(GameCampaignError::BillboardMalformed {
                gpa: gpa.unwrap_or(0),
                len: len.unwrap_or(0),
                why: "only one of REG_BILLBOARD_GPA / REG_BILLBOARD_LEN was published — the \
                      agent must publish both",
            });
        }
    };
    let why = if len == 0 {
        Some("zero-length window — film would read no bytes")
    } else if len < BILLBOARD_MIN_LEN {
        // Round-3 finding: "nonzero and in-RAM" is not the same as "usable".
        Some(
            "shorter than the 32-byte billboard header — film's FilmPlan::derive refuses any \
             window under a header (BillboardTooSmall), and film is M0's MANDATORY artifact, so \
             this window would green the determinism gate over a clip that cannot be produced",
        )
    } else if gpa == 0 {
        Some("null guest-physical address — gpa 0 is never a pinned billboard")
    } else {
        match gpa.checked_add(len) {
            None => Some("gpa + len overflows a u64"),
            Some(end) if end > guest_ram_len => Some("the window runs past the end of guest RAM"),
            Some(_) => None,
        }
    };
    match why {
        Some(why) => Err(GameCampaignError::BillboardMalformed { gpa, len, why }),
        None => Ok(Some((gpa, len))),
    }
}

/// Seal the campaign base. Preferred boundary: the play-agent's
/// `setup_complete` **snapshot point** (the billboard gpa/len registers are
/// published in the setup prefix, so every branch inherits them). Arm the
/// snapshot-point class and run; if the guest surfaces it, seal there.
///
/// The generic seal-retry fallback below is legal ONLY for the portable
/// no-SDK toy (`require_snapshot_point = false`): a real box guest that never
/// reaches `setup_complete` is a **dead agent** (bad core/ROM provisioning,
/// hugetlb, `/dev/mem` — it crashed or halted during init), and sealing
/// wherever it died would record a zero/constant-cell campaign — the vacuity
/// class (round-5 P1). In box mode that is a loud
/// [`GameCampaignError::SetupNotReached`], never a seal.
fn seal_base<M: Machine>(
    machine: &mut M,
    cfg: &GameCampaignConfig,
) -> Result<(explorer::SnapId, u64), GameCampaignError> {
    let mut vt = crate::probe_vtime(machine)?;
    // The snapshot-point class bit, single-sourced from the control plane's
    // pinned mapping (the same construction as `StopMask::ASSERTION`).
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
            // In box mode the setup boundary must also SEAL there — running
            // past it into the frame loop would move the base off the setup
            // prefix. Only the toy path may fall through.
            Err(e) if cfg.require_snapshot_point => return Err(e.into()),
            Err(_) => {}
        }
    } else if cfg.require_snapshot_point {
        // The workload gate: no setup_complete ⇒ no campaign, loudly.
        return Err(GameCampaignError::SetupNotReached {
            stop: format!("{stop:?}"),
        });
    } else {
        vt = stop.vtime().0;
    }
    // Fallback: the task-60 seal-retry loop (portable toy path only).
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

/// Lowercase hex of a state hash (the log's determinism witness).
fn hex(h: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in h {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// ---------------------------------------------------------------------------
// The portable toy: an SMB-shaped guest for the laptop gates.
// ---------------------------------------------------------------------------

/// A deterministic toy [`Machine`] that emits SMB-shaped SDK state events — a
/// pure function of its branch env's seed. Rightward progress with resets and
/// occasional level-ups, so campaigns produce varied cells and depths; the
/// portable tests drive [`run_game_campaign`] against it end-to-end (through
/// the real wire encode → `sdk_events::decode_binary` → [`smb_cells`] path, so
/// a register or layout drift breaks a test, not the box).
pub struct GameToyMachine {
    current: Reproducer,
    vtime: u64,
    /// The branch point's V-time (the toy's own emissions are stamped
    /// relative to it, so cuts stay coherent through the lineage).
    branch_vtime: u64,
    /// The restored ancestor SDK prefix (the production machine restores it
    /// into a branched child, so counts are CUMULATIVE — task 132).
    prefix: Vec<(u64, u32, Vec<u8>)>,
    /// snap id -> (vtime, env, captured prefix).
    #[allow(clippy::type_complexity)] // not order-observable: a plain snap table
    snaps: std::collections::BTreeMap<u64, (u64, Reproducer, Vec<(u64, u32, Vec<u8>)>)>,
    next_snap: u64,
}

/// The toy's quiescent boot V-time.
const TOY_BASE_VTIME: u64 = 1_000;

/// The reseed-Moment window the toy path uses when no rollout deadline is
/// configured (the quiet mutator places its salt-derived marker inside the
/// rollout's Moment span; the toy ignores absolute Moments, so only the
/// derived table contents matter to it).
const TOY_RESEED_WINDOW: u64 = 1_000;

impl Default for GameToyMachine {
    fn default() -> Self {
        GameToyMachine::new()
    }
}

impl GameToyMachine {
    /// A fresh toy guest, quiescent at boot.
    pub fn new() -> Self {
        GameToyMachine {
            current: explorer::SpecEnvCodec.seeded(0),
            vtime: TOY_BASE_VTIME,
            branch_vtime: TOY_BASE_VTIME,
            prefix: Vec::new(),
            snaps: std::collections::BTreeMap::new(),
            next_snap: 1,
        }
    }

    /// The current-branch capture: the restored ancestor prefix plus the
    /// toy's own per-window emissions, stamped relative to the branch point
    /// (one tuple emission per window + the depth max register + the frame
    /// clock — the play-agent's per-window shape, over the real wire
    /// layout).
    fn capture(&self) -> Vec<(u64, u32, Vec<u8>)> {
        let mut out = self.prefix.clone();
        for (w, world, level, xb, best) in self.windows() {
            let at = self.branch_vtime + w * 12;
            let (id, p) = state_event(reg::GAME_MODE, 0, 1);
            out.push((at, id, p));
            let (id, p) = state_event(reg::WORLD, 0, world);
            out.push((at + 1, id, p));
            let (id, p) = state_event(reg::LEVEL, 0, level);
            out.push((at + 2, id, p));
            let (id, p) = state_event(reg::X_BUCKET, 0, xb);
            out.push((at + 3, id, p));
            let (id, p) = state_event(reg::DEPTH, 1, best);
            out.push((at + 4, id, p));
            let (id, p) = state_event(reg::FRAME, 0, w * 12);
            out.push((at + 5, id, p));
        }
        out
    }

    /// The simulated per-window SMB observations for the current env:
    /// `(window, world, level, x_bucket, depth_ordinal)`. The behavior folds
    /// the env's **reseed-marker table** into the effective seed (a crude
    /// stand-in for "fresh entropy after the marker"), so the quiet mutator's
    /// exploit branches genuinely diverge on the toy exactly as a reseeded
    /// guest would.
    fn windows(&self) -> Vec<(u64, u64, u64, u64, u64)> {
        let seed = explorer::AdapterEnv::decode(&self.current)
            .map(|d| {
                d.spec.reseeds().iter().fold(d.spec.seed(), |acc, (at, s)| {
                    acc ^ s.rotate_left((*at % 63) as u32)
                })
            })
            .unwrap_or(0);
        let mut p = explorer::Prng::new(seed);
        let mut x = 40u64;
        let (mut world, mut level) = (0u64, 0u64);
        let mut best = 0u64;
        (0..16)
            .map(|w| {
                // Rightward drift with a pit hazard: a bad draw resets X (the
                // random-input plateau); enough progress clears the level.
                let draw = p.next_u64();
                x = if draw.is_multiple_of(7) {
                    40
                } else {
                    x + draw % 160
                };
                if x >= 1024 {
                    x = 40;
                    level += 1;
                    if level == 4 {
                        level = 0;
                        world += 1;
                    }
                }
                best = best.max(world * 4 + level);
                (w, world, level, x / 128, best)
            })
            .collect()
    }
}

/// Encode one state event the way the guest SDK wire does (`[op u8][value u64
/// LE]` in `NS_STATE` = namespace 2) so the toy exercises the REAL
/// `sdk_events::decode_binary` path.
fn state_event(reg: u64, op: u8, value: u64) -> (u32, Vec<u8>) {
    let id = (2u32 << 24) | (reg as u32);
    let mut payload = Vec::with_capacity(9);
    payload.push(op);
    payload.extend_from_slice(&value.to_le_bytes());
    (id, payload)
}

impl Machine for GameToyMachine {
    fn branch(&mut self, snap: explorer::SnapId, env: &Reproducer) -> Result<(), MachineError> {
        let Some((vt, _, prefix)) = self.snaps.get(&snap.0) else {
            return Err(MachineError::UnknownSnapshot(snap.0));
        };
        explorer::AdapterEnv::decode(env)?;
        self.vtime = *vt;
        self.branch_vtime = *vt;
        self.prefix = prefix.clone();
        self.current = env.clone();
        Ok(())
    }

    fn replay(&mut self, snap: explorer::SnapId) -> Result<(), MachineError> {
        let Some((vt, env, prefix)) = self.snaps.get(&snap.0) else {
            return Err(MachineError::UnknownSnapshot(snap.0));
        };
        self.vtime = *vt;
        self.branch_vtime = *vt;
        self.prefix = prefix.clone();
        self.current = env.clone();
        Ok(())
    }

    fn run(
        &mut self,
        until: &StopConditions,
        _resolve: Option<&explorer::Answer>,
    ) -> Result<StopReason, MachineError> {
        // The play-agent never exits on its own: a configured deadline is the
        // rollout terminal; without one the toy goes quiescent after its 16
        // windows.
        match until.deadline {
            Some(d) => {
                self.vtime = self.vtime.max(d.0);
                Ok(StopReason::Deadline {
                    vtime: explorer::Moment(self.vtime),
                })
            }
            None => {
                let terminal = self.vtime.saturating_add(16 * 12);
                self.vtime = terminal;
                Ok(StopReason::Quiescent {
                    vtime: explorer::Moment(terminal),
                })
            }
        }
    }

    fn snapshot(&mut self) -> Result<(explorer::SnapId, explorer::EvidenceCut), MachineError> {
        let id = self.next_snap;
        self.next_snap += 1;
        // The cut is stamped from the same state the seal records (task 127):
        // the capture prefix at or before the seal moment, CUMULATIVE through
        // the restored ancestor prefix (task 132).
        let vt = self.vtime;
        let sealed: Vec<(u64, u32, Vec<u8>)> = self
            .capture()
            .into_iter()
            .filter(|(at, _, _)| *at <= vt)
            .collect();
        let included = sealed.len() as u64;
        self.snaps.insert(id, (vt, self.current.clone(), sealed));
        Ok((
            explorer::SnapId(id),
            explorer::EvidenceCut {
                at: explorer::Moment(vt),
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
        let mut h = Sha256::new();
        h.update(b"campaign-runner.gametoy.state_hash.v1");
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
        // The cumulative capture: the restored ancestor prefix plus this
        // branch's own emissions (the production contract, task 132).
        Ok(self.capture())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorer::SpecEnvCodec;

    /// The instrumentation declaration is a real, decodable wire-v2 catalog
    /// that resolves EVERY register in the resolution table to reducible
    /// state under its declared op (task 132; kills the catalog/resolution
    /// stub mutants — a fake catalog or an empty table fails here, not just
    /// on the box).
    #[test]
    fn instrumentation_catalog_resolves_every_register() {
        use sdk_events::UpdateOp;
        let table = smb_resolution();
        assert!(
            table.len() >= 8,
            "the resolution table covers the play-agent register set"
        );
        let n = sdk_events::decode_binary(&[(
            sdk_events::Moment(0),
            0, // the catalog marker event id
            smb_instrumentation_catalog(),
        )])
        .expect("the standalone declaration decodes");
        for (id, op) in &table {
            let entry = n.schema.entry(id).expect("every register is declared");
            assert!(
                entry.is_reducible_state(),
                "register {id:?} must be reducible state"
            );
            assert_eq!(entry.base_op, Some(*op), "register {id:?} keeps its op");
        }
        // The specific ops the workload contract pins: DEPTH is the one Max
        // register; the tuple/billboard registers are Set.
        let point = |local: u64| ObservationId::Point {
            namespace: NS_STATE,
            local: local as u32,
        };
        let op_of = |local: u64| {
            table
                .iter()
                .find(|(id, _)| *id == point(local))
                .map(|(_, op)| *op)
        };
        assert_eq!(op_of(reg::DEPTH), Some(UpdateOp::Max));
        assert_eq!(op_of(reg::X_BUCKET), Some(UpdateOp::Set));
        assert_eq!(op_of(reg::POWERUP), Some(UpdateOp::Set));
    }

    /// A guest-declared v1 catalog is upgraded IN PLACE (never doubled): the
    /// wrapped capture carries exactly one catalog tuple whose schema
    /// resolves the registers (the box round-1 smoke finding as a portable
    /// pin).
    #[test]
    fn declared_machine_upgrades_a_guest_catalog_in_place() {
        // A machine whose capture carries its own v1 catalog + one firing.
        // The counters prove the wrapper CALLS the inner machine (delegation,
        // not a stub).
        #[derive(Default)]
        struct V1Guest {
            replays: u32,
            drops: u32,
        }
        impl Machine for V1Guest {
            fn branch(
                &mut self,
                _s: explorer::SnapId,
                _e: &Reproducer,
            ) -> Result<(), MachineError> {
                Ok(())
            }
            fn replay(&mut self, _s: explorer::SnapId) -> Result<(), MachineError> {
                self.replays += 1;
                Ok(())
            }
            fn run(
                &mut self,
                _u: &StopConditions,
                _r: Option<&explorer::Answer>,
            ) -> Result<StopReason, MachineError> {
                Ok(StopReason::Quiescent { vtime: Moment(1) })
            }
            fn snapshot(&mut self) -> Result<(explorer::SnapId, EvidenceCut), MachineError> {
                Ok((explorer::SnapId(1), EvidenceCut::default()))
            }
            fn drop_snap(&mut self, _s: explorer::SnapId) -> Result<(), MachineError> {
                self.drops += 1;
                Ok(())
            }
            fn hash(&mut self) -> Result<[u8; 32], MachineError> {
                Ok([7u8; 32])
            }
            fn coverage(&self) -> &[u8] {
                &[3]
            }
            fn console(&mut self) -> Result<Vec<(u64, Vec<u8>)>, MachineError> {
                Ok(vec![(9, b"line".to_vec())])
            }
            fn recorded_env(&self) -> Result<Reproducer, MachineError> {
                Ok(SpecEnvCodec.seeded(0))
            }
            fn sdk_events(&mut self) -> Result<Vec<(u64, u32, Vec<u8>)>, MachineError> {
                // A v1 catalog declaring only X_BUCKET, then one firing.
                let v1 = {
                    // magic "SDKC" + version 1 + count 1 + (kind, id, name).
                    let mut b = Vec::new();
                    b.extend_from_slice(&u32::from_le_bytes(*b"SDKC").to_le_bytes());
                    b.push(1); // SDK_WIRE_VERSION v1
                    b.extend_from_slice(&1u32.to_le_bytes());
                    b.push(4); // kind byte: KIND_STATE (guest wire v1)
                    b.extend_from_slice(&(reg::X_BUCKET as u32).to_le_bytes());
                    b.extend_from_slice(&(1u16).to_le_bytes());
                    b.push(b'x');
                    b
                };
                let (id, p) = state_event(reg::X_BUCKET, 0, 3);
                Ok(vec![(0, CATALOG_EVENT_ID, v1), (1, id, p)])
            }
        }
        let mut m = DeclaredMachine::new(V1Guest::default());
        let out = m.sdk_events().expect("capture");
        let catalogs = out
            .iter()
            .filter(|(_, id, _)| *id == CATALOG_EVENT_ID)
            .count();
        assert_eq!(catalogs, 1, "the guest catalog is upgraded, never doubled");
        let n = crate::sdk_compat::decode_sdk(&out).expect("upgraded capture decodes");
        let x = ObservationId::Point {
            namespace: NS_STATE,
            local: reg::X_BUCKET as u32,
        };
        assert!(
            n.schema.entry(&x).expect("declared").is_reducible_state(),
            "the upgraded declaration resolves the register"
        );
        // Delegation is verbatim (kills the delegation stub mutants).
        assert_eq!(m.hash().expect("hash"), [7u8; 32]);
        assert_eq!(m.coverage(), &[3]);
        assert_eq!(
            m.console().expect("console delegates"),
            vec![(9u64, b"line".to_vec())]
        );
        m.replay(explorer::SnapId(1)).expect("replay delegates");
        m.drop_snap(explorer::SnapId(1)).expect("drop delegates");
        assert_eq!(
            (m.inner.replays, m.inner.drops),
            (1, 1),
            "the wrapper reached the inner machine"
        );
    }

    /// The quiet codec's exploit-mutate maps the two refusal classes onto the
    /// codec's typed errors: a fault-carrying exemplar is the fail-closed
    /// UnsupportedComposition; junk bytes are the malformed-blob class.
    #[test]
    fn quiet_codec_maps_refusals_onto_codec_errors() {
        let quiet = QuietCodec {
            inner: Box::new(SpecEnvCodec),
            window: 1_000,
        };
        // A fault-carrying env (one host action) is refused as unsupported.
        let decoded = explorer::AdapterEnv::decode(&SpecEnvCodec.seeded(1)).expect("decodes");
        let spec = environment::EnvSpec::Recorded {
            seed: decoded.spec.seed(),
            policy: decoded.spec.policy().clone(),
            overrides: decoded.spec.overrides().clone(),
            standing: Vec::new(),
            reseeds: std::collections::BTreeMap::new(),
        };
        // Splice a host fault in via the environment vocabulary.
        let mut with_fault = explorer::AdapterEnv {
            base_offset: decoded.base_offset,
            pos: decoded.pos,
            spec,
        };
        if let environment::EnvSpec::Recorded { overrides, .. } = &mut with_fault.spec {
            overrides.insert(
                1,
                environment::Action::Host(environment::HostFault::InjectInterrupt { vector: 32 }),
            );
        }
        assert!(matches!(
            quiet.mutate(&with_fault.encode(), 7),
            Err(explorer::EnvCodecError::UnsupportedComposition)
        ));
        // Junk bytes are the malformed-blob class.
        let junk = Reproducer {
            blob_version: 1,
            bytes: vec![0xFF; 4],
        };
        assert!(matches!(
            quiet.mutate(&junk, 7),
            Err(explorer::EnvCodecError::Malformed(_))
        ));
        // A clean env mutates to a reseed-only variant (no faults minted).
        let out = quiet
            .mutate(&SpecEnvCodec.seeded(3), 7)
            .expect("clean env mutates");
        let d = explorer::AdapterEnv::decode(&out).expect("decodes");
        assert!(d.spec.host_faults().next().is_none());
        assert!(!d.spec.reseeds().is_empty());
    }

    /// `SmbObservationCells` keys the `(mode, world, level, x-bucket)` tuple
    /// off the reduced map: no `X_BUCKET` ⇒ the empty (pre-progress) cell;
    /// missing tuple registers default to 0; non-scalar values are ignored
    /// (kills the reduced-value match-arm mutants portably).
    #[test]
    fn smb_observation_cells_key_the_reduced_tuple() {
        use explorer::{ObservationMap, ReducedValue};
        let cells = SmbObservationCells;
        let cut = EvidenceCut::default();
        let point = |local: u64| ObservationId::Point {
            namespace: NS_STATE,
            local: local as u32,
        };
        // Pre-X state: the empty cell.
        let mut obs = ObservationMap::new();
        obs.insert(point(reg::GAME_MODE), ReducedValue::Scalar(1));
        assert!(cells.key(cut, &obs).is_empty());
        // The full tuple packs exactly smb_cell_key.
        obs.insert(point(reg::WORLD), ReducedValue::Scalar(2));
        obs.insert(point(reg::LEVEL), ReducedValue::Scalar(3));
        obs.insert(point(reg::X_BUCKET), ReducedValue::Scalar(7));
        assert_eq!(
            cells.key(cut, &obs),
            smb_cell_key(1, 2, 3, 7).to_le_bytes().to_vec()
        );
        // Missing mode/world/level default to 0 (x alone still mints a cell).
        let mut only_x = ObservationMap::new();
        only_x.insert(point(reg::X_BUCKET), ReducedValue::Scalar(7));
        assert_eq!(
            cells.key(cut, &only_x),
            smb_cell_key(0, 0, 0, 7).to_le_bytes().to_vec()
        );
        // A non-scalar (accumulated) value never reaches the tuple.
        let mut acc = ObservationMap::new();
        acc.insert(
            point(reg::X_BUCKET),
            ReducedValue::Accumulated([7].into_iter().collect()),
        );
        assert!(cells.key(cut, &acc).is_empty());
    }

    /// The branch budget the in-memory smoke campaigns below drive
    /// ([`GameCampaignConfig::smoke`]'s `max_branches`), **shrunk to 8 under Miri**
    /// (task 104, `hm-d4y`). The properties these campaigns pin — a rerun is
    /// bit-identical, distinct seeds diverge, the selector diverges from blind
    /// search, cells and depth flow out of the SDK events — are all budget-
    /// INDEPENDENT: they hold at any positive branch count, so a smaller budget
    /// costs the assertions nothing (verified: at 8 branches the seed-42 campaign
    /// still reaches depth ≥ 1 and mints the same 10 cells it does at 64). Under
    /// the interpreter each branch is ~8 s of interpreted work, so 64 → 8 takes
    /// this family from ~19 min of the Miri run to ~2.5. Native runs keep the full
    /// 64 and are byte-for-byte unchanged.
    const SMOKE_BRANCHES: u64 = if cfg!(miri) { 8 } else { 64 };

    /// A **box-shaped** guest: seals at its `setup_complete` snapshot point,
    /// publishes a billboard in the setup prefix, and runs every rollout to its
    /// deadline. The knobs are exactly the ways the real box driver can be
    /// handed a degenerate budget — a rollout that advances no V-time
    /// (`--deadline-delta 0`) and a rollout whose deadline expires before a
    /// frame body completes — so the vacuity guard can be driven against the
    /// same shape the gate sees.
    ///
    /// It emits `REG_FRAME` the way the real agent does: **one marker written
    /// before each frame body runs**. A rollout that emits `n` markers therefore
    /// completed `n - 1` frames — and `n = 1` (the marker, then the deadline)
    /// completed **none**, which is exactly the round-2 finding.
    struct BoxGuest {
        sealed: bool,
        published: bool,
        vtime: u64,
        base_vtime: u64,
        next_snap: u64,
        env: Reproducer,
        /// What the setup prefix publishes (`None` = nothing at all).
        billboard: Option<(u64, u64)>,
        /// V-time each rollout advances past the base.
        rollout_span: u64,
        /// Pre-run `REG_FRAME` markers each rollout emits (⇒ `n - 1` frames
        /// actually executed).
        frame_markers: u64,
    }

    impl BoxGuest {
        /// The healthy shape: a valid billboard, rollouts that run and render.
        fn healthy() -> Self {
            BoxGuest {
                sealed: false,
                published: false,
                vtime: 1_000,
                base_vtime: 0,
                next_snap: 1,
                env: SpecEnvCodec.seeded(0),
                billboard: Some((0x04e0_0000, 15_838)),
                rollout_span: 2_000_000_000,
                frame_markers: 120,
            }
        }
    }

    impl Machine for BoxGuest {
        fn branch(&mut self, _s: explorer::SnapId, env: &Reproducer) -> Result<(), MachineError> {
            self.env = env.clone();
            self.vtime = self.base_vtime;
            Ok(())
        }
        fn replay(&mut self, _s: explorer::SnapId) -> Result<(), MachineError> {
            self.vtime = self.base_vtime;
            Ok(())
        }
        fn run(
            &mut self,
            until: &StopConditions,
            _resolve: Option<&explorer::Answer>,
        ) -> Result<StopReason, MachineError> {
            // probe_vtime's already-met deadline: report position, run nothing.
            if until.deadline == Some(Moment(0)) {
                return Ok(StopReason::Deadline {
                    vtime: Moment(self.vtime),
                });
            }
            if !self.sealed {
                self.sealed = true;
                self.base_vtime = self.vtime;
                return Ok(StopReason::SnapshotPoint {
                    vtime: Moment(self.vtime),
                });
            }
            // A rollout: advance its span (possibly zero) and stop at the deadline.
            self.vtime = self.base_vtime + self.rollout_span;
            Ok(StopReason::Deadline {
                vtime: Moment(self.vtime),
            })
        }
        fn snapshot(&mut self) -> Result<(explorer::SnapId, explorer::EvidenceCut), MachineError> {
            let id = self.next_snap;
            self.next_snap += 1;
            // The cut is stamped from the stopped state (task 127). BoxGuest
            // models per-rollout (run-local) captures, so the cumulative
            // prefix base is 0.
            Ok((
                explorer::SnapId(id),
                explorer::EvidenceCut {
                    at: Moment(self.vtime),
                    sdk_events: 0,
                },
            ))
        }
        fn drop_snap(&mut self, _s: explorer::SnapId) -> Result<(), MachineError> {
            Ok(())
        }
        fn hash(&mut self) -> Result<[u8; 32], MachineError> {
            Ok([0u8; 32])
        }
        fn coverage(&self) -> &[u8] {
            &[]
        }
        fn recorded_env(&self) -> Result<Reproducer, MachineError> {
            Ok(self.env.clone())
        }
        fn sdk_events(&mut self) -> Result<Vec<(u64, u32, Vec<u8>)>, MachineError> {
            // The setup prefix's drain: the billboard registers, once.
            if self.sealed && !self.published {
                self.published = true;
                let mut out = Vec::new();
                if let Some((gpa, len)) = self.billboard {
                    let (gid, gp) = state_event(reg::BILLBOARD_GPA, 0, gpa);
                    let (lid, lp) = state_event(reg::BILLBOARD_LEN, 0, len);
                    out.push((self.base_vtime, gid, gp));
                    out.push((self.base_vtime + 1, lid, lp));
                }
                return Ok(out);
            }
            // A rollout's drain: one state tuple per frame the agent announced,
            // with REG_FRAME written BEFORE that frame's body ran (the agent's
            // real order — film addresses the billboard by that Moment).
            let mut out = Vec::new();
            for f in 0..self.frame_markers {
                let at = self.base_vtime + f + 1;
                let (id, p) = state_event(reg::GAME_MODE, 0, 1);
                out.push((at, id, p));
                let (id, p) = state_event(reg::X_BUCKET, 0, f % 8);
                out.push((at, id, p));
                let (id, p) = state_event(reg::FRAME, 0, f);
                out.push((at, id, p));
            }
            Ok(out)
        }
    }

    /// The box config the driver builds (`require_snapshot_point`), with the
    /// budget knobs under test.
    fn box_cfg(max_branches: u64, deadline_delta: Option<u64>) -> GameCampaignConfig {
        GameCampaignConfig {
            max_branches,
            deadline_delta,
            require_snapshot_point: true,
            ..GameCampaignConfig::smoke(7)
        }
    }

    fn run_box(guest: BoxGuest, cfg: &GameCampaignConfig) -> GameCampaignOutcome {
        run_game_campaign(
            guest,
            Box::new(SpecEnvCodec),
            cfg,
            ExplorationConfig::PureRandom,
        )
        .expect("the box-shaped guest campaigns cleanly")
    }

    /// Task 103 finding 1b — **the vacuous determinism PASS**. Each fixture is
    /// a degenerate budget that the round-9 gate would have called
    /// `DETERMINISM PASS: 25/25` (identical sequences, because identically
    /// empty), and each must now be refused by the gate before any banner —
    /// whatever `--repeat` says.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "runs a full game campaign; its durable evidence ledger (task 132) opens a \
                  real file even for `trace_dir: None` (a campaign-lifetime scratch tempdir), \
                  which Miri isolation forbids. Campaign logic is covered by the portable \
                  nextest suite; Miri still guards the crate's only unsafe via \
                  mock::tests::mock_vmm_composes_maps_memory_and_ticks_per_exit."
    )]
    fn the_determinism_gate_refuses_a_run_that_did_no_work() {
        // (a) `--max-branches 0`: the gate would compare two empty state_hash
        //     sequences and find them identical.
        let outcome = run_box(BoxGuest::healthy(), &box_cfg(0, Some(2_000_000_000)));
        assert!(outcome.log.events.is_empty());
        assert_eq!(outcome.vacuity(), Some(Vacuity::NoBranches));
        assert_eq!(
            determinism_verdict(&outcome, DETERMINISM_BAR),
            Err(Vacuity::NoBranches),
            "25 repetitions of a zero-branch campaign is not a determinism gate"
        );

        // (b) `--deadline-delta 0`: the rollout's deadline is already met at the
        //     base, so every branch hashes the sealed base itself.
        let guest = BoxGuest {
            rollout_span: 0,
            ..BoxGuest::healthy()
        };
        let outcome = run_box(guest, &box_cfg(8, Some(0)));
        assert_eq!(outcome.log.events.len(), 8, "the branches DID run…");
        assert_eq!(
            outcome.vacuity(),
            Some(Vacuity::NoVTime),
            "…but none of them advanced the guest's clock"
        );
        assert_eq!(
            determinism_verdict(&outcome, DETERMINISM_BAR),
            Err(Vacuity::NoVTime)
        );

        // (c) The budget that sneaks past a positivity check: V-time advances,
        //     but the guest emits nothing at all — not one frame.
        let guest = BoxGuest {
            frame_markers: 0,
            rollout_span: 1,
            ..BoxGuest::healthy()
        };
        let outcome = run_box(guest, &box_cfg(8, Some(1)));
        assert_eq!(
            outcome.vacuity(),
            Some(Vacuity::NoFrames),
            "a rollout that renders no frame measured no gameplay"
        );
        assert_eq!(
            determinism_verdict(&outcome, DETERMINISM_BAR),
            Err(Vacuity::NoFrames)
        );

        // (c2) **The round-2 finding.** The agent writes REG_FRAME *before*
        //      running the frame it announces (film's addressing contract), so
        //      a deadline expiring between the marker and the frame body leaves
        //      exactly ONE observation — and zero frames executed. Counting
        //      observations would have called that one frame and let 25
        //      identical repetitions of it print DETERMINISM PASS.
        let guest = BoxGuest {
            frame_markers: 1,
            rollout_span: 1_000,
            ..BoxGuest::healthy()
        };
        let outcome = run_box(guest, &box_cfg(8, Some(1_000)));
        assert_eq!(
            outcome.work.min_completed_frames, 0,
            "one pre-run marker announces a frame; it does not run one"
        );
        assert_eq!(
            outcome.vacuity(),
            Some(Vacuity::NoFrames),
            "a lone frame marker is not a frame"
        );
        assert_eq!(
            determinism_verdict(&outcome, DETERMINISM_BAR),
            Err(Vacuity::NoFrames)
        );

        // …and TWO markers prove the first frame's body completed (the loop came
        // back around to announce the next) — the boundary the guard turns on.
        let guest = BoxGuest {
            frame_markers: 2,
            rollout_span: 1_000,
            ..BoxGuest::healthy()
        };
        let outcome = run_box(guest, &box_cfg(8, Some(1_000)));
        assert_eq!(outcome.work.min_completed_frames, 1);
        assert_eq!(outcome.vacuity(), None, "one completed frame is real work");

        // (d) One hollow rollout among busy ones still sinks the gate: the
        //     evidence is the WEAKEST rollout, never the total.
        let guest = BoxGuest::healthy();
        let outcome = run_box(guest, &box_cfg(8, Some(2_000_000_000)));
        assert_eq!(outcome.vacuity(), None);
        let hollowed = GameCampaignOutcome {
            work: WorkEvidence {
                min_completed_frames: 0,
                ..outcome.work
            },
            ..outcome.clone()
        };
        assert_eq!(hollowed.vacuity(), Some(Vacuity::NoFrames));

        // And the healthy campaign still passes at the floor, smokes below it,
        // and claims nothing at one repetition — the round-8 floor intact.
        assert_eq!(
            determinism_verdict(&outcome, DETERMINISM_BAR),
            Ok(GateVerdict::Pass { repeat: 25 })
        );
        assert!(
            determinism_verdict(&outcome, DETERMINISM_BAR)
                .unwrap()
                .banner()
                .unwrap()
                .contains("DETERMINISM PASS")
        );
        assert_eq!(
            determinism_verdict(&outcome, 2),
            Ok(GateVerdict::BelowFloor { repeat: 2 })
        );
        assert!(
            !determinism_verdict(&outcome, 24)
                .unwrap()
                .banner()
                .unwrap()
                .contains("DETERMINISM PASS"),
            "24 repetitions is a smoke, not the gate"
        );
        assert_eq!(determinism_verdict(&outcome, 1), Ok(GateVerdict::Single));
        assert_eq!(determinism_verdict(&outcome, 1).unwrap().banner(), None);
    }

    /// The healthy box campaign's evidence is the real thing: every branch
    /// rolled out, advanced the clock, and completed frames — 120 pre-run
    /// markers ⇒ 119 frame bodies actually executed (the last announced frame
    /// is cut off by the deadline, exactly as on the box).
    #[test]
    #[cfg_attr(
        miri,
        ignore = "runs a full game campaign; its durable evidence ledger (task 132) opens a \
                  real file even for `trace_dir: None` (a campaign-lifetime scratch tempdir), \
                  which Miri isolation forbids. Campaign logic is covered by the portable \
                  nextest suite; Miri still guards the crate's only unsafe via \
                  mock::tests::mock_vmm_composes_maps_memory_and_ticks_per_exit."
    )]
    fn a_real_campaign_carries_its_work_evidence() {
        let guest = BoxGuest::healthy();
        let outcome = run_box(guest, &box_cfg(4, Some(2_000_000_000)));
        assert_eq!(
            outcome.work,
            WorkEvidence {
                branches: 4,
                min_vtime_span: 2_000_000_000,
                min_completed_frames: 119,
            }
        );
        assert_eq!(outcome.billboard, Some((0x04e0_0000, 15_838)));
    }

    /// [`smb_completed_frames`] counts frame-clock TRANSITIONS, not markers:
    /// the unit-level pin of the round-2 finding.
    #[test]
    fn completed_frames_counts_transitions_not_markers() {
        // Each fixture is a run of `REG_FRAME` state firings through the REAL
        // wire decode, exactly the shape a rollout's capture produces.
        let frames = |vals: &[u64]| -> Vec<SdkEvent> {
            decoded_state(&vals.iter().map(|v| (reg::FRAME, *v)).collect::<Vec<_>>())
        };
        // No markers at all, and a lone marker, are both zero completed frames.
        assert_eq!(smb_completed_frames(&frames(&[])), 0);
        assert_eq!(smb_completed_frames(&frames(&[0])), 0);
        // The second marker proves the first frame's body ran.
        assert_eq!(smb_completed_frames(&frames(&[0, 1])), 1);
        assert_eq!(smb_completed_frames(&frames(&[0, 1, 2])), 2);
        // A repeated value is not a new frame (no transition, no work).
        assert_eq!(smb_completed_frames(&frames(&[7, 7, 7])), 0);
        // The frame clock is a u32 the agent wraps: a wrap is still a frame, so
        // the count is on CHANGE, not on increase.
        assert_eq!(smb_completed_frames(&frames(&[u32::MAX as u64, 0])), 1);
        // Non-FRAME registers never count, whatever their values do.
        let other = decoded_state(&[(reg::X_BUCKET, 3), (reg::X_BUCKET, 3)]);
        assert_eq!(smb_completed_frames(&other), 0);
    }

    /// Task 103 finding 2 — **a malformed billboard must not slip past the
    /// round-8 `BillboardMissing` presence check**. Every fixture here is a
    /// window the guest actually published, and every one of them is unusable:
    /// film would read nothing, or the wrong bytes. Each must fail the campaign
    /// loudly, before a single rollout runs.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "runs a full game campaign (the valid-billboard cases seal a base and run \
                  rollouts); its durable evidence ledger (task 132) opens a real file even for \
                  `trace_dir: None` (a campaign-lifetime scratch tempdir), which Miri isolation \
                  forbids. Campaign logic is covered by the portable nextest suite; Miri still \
                  guards the crate's only unsafe via \
                  mock::tests::mock_vmm_composes_maps_memory_and_ticks_per_exit."
    )]
    fn a_malformed_billboard_refuses_the_campaign() {
        let ram = GameCampaignConfig::smoke(0).guest_ram_len;
        let refuses = |billboard: Option<(u64, u64)>| -> GameCampaignError {
            let guest = BoxGuest {
                billboard,
                ..BoxGuest::healthy()
            };
            run_game_campaign(
                guest,
                Box::new(SpecEnvCodec),
                &box_cfg(8, Some(2_000_000_000)),
                ExplorationConfig::PureRandom,
            )
            .expect_err("a malformed billboard must refuse the campaign")
        };

        // Zero length: the round-8 check saw `Some((gpa, 0))` and called it present.
        assert!(matches!(
            refuses(Some((0x04e0_0000, 0))),
            GameCampaignError::BillboardMalformed { len: 0, .. }
        ));
        // Round-3 finding: nonzero and in-RAM, but SHORTER THAN FILM'S HEADER —
        // `FilmPlan::derive` refuses it (`BillboardTooSmall`), so the campaign
        // would green the gate over a mandatory artifact that cannot be built.
        assert!(matches!(
            refuses(Some((0x0000_1000, 1))),
            GameCampaignError::BillboardMalformed { len: 1, .. }
        ));
        // One byte below the header is still unusable…
        assert!(matches!(
            refuses(Some((0x04e0_0000, BILLBOARD_MIN_LEN - 1))),
            GameCampaignError::BillboardMalformed { .. }
        ));
        // …and exactly a header's worth is the boundary film accepts.
        let guest = BoxGuest {
            billboard: Some((0x04e0_0000, BILLBOARD_MIN_LEN)),
            ..BoxGuest::healthy()
        };
        assert_eq!(
            run_box(guest, &box_cfg(2, Some(2_000_000_000))).billboard,
            Some((0x04e0_0000, BILLBOARD_MIN_LEN))
        );
        // Null gpa: guest-physical 0 is never a pinned billboard.
        assert!(matches!(
            refuses(Some((0, 15_838))),
            GameCampaignError::BillboardMalformed { gpa: 0, .. }
        ));
        // Overflowing range: `gpa + len` wraps a u64.
        assert!(matches!(
            refuses(Some((u64::MAX - 8, 4_096))),
            GameCampaignError::BillboardMalformed { .. }
        ));
        // Past the end of guest RAM: the window names memory the VM does not have.
        assert!(matches!(
            refuses(Some((ram - 1_024, 4_096))),
            GameCampaignError::BillboardMalformed { .. }
        ));
        // …and the last byte inside RAM is fine (the boundary is inclusive-end).
        let guest = BoxGuest {
            billboard: Some((ram - 4_096, 4_096)),
            ..BoxGuest::healthy()
        };
        assert_eq!(
            run_box(guest, &box_cfg(2, Some(2_000_000_000))).billboard,
            Some((ram - 4_096, 4_096))
        );

        // Still-absent stays BillboardMissing (round 9's finding, unchanged).
        assert!(matches!(refuses(None), GameCampaignError::BillboardMissing));
    }

    /// The drift pin for [`BILLBOARD_MIN_LEN`] (round-3 finding): the bound the
    /// campaign validates against IS film's header length. `film` is a
    /// dev-dependency — the driver must not take a *library* dependency on the
    /// renderer to validate a window — so the value is mirrored in the lib and
    /// pinned to its source here. If film's header grows, this test fails
    /// instead of the hole silently re-opening (the same single-source
    /// discipline as `guest_ram_len` ← `boxrun::GUEST_RAM_LEN`).
    #[test]
    fn billboard_min_len_tracks_films_header() {
        assert_eq!(
            BILLBOARD_MIN_LEN as usize,
            film::HEADER_LEN,
            "BILLBOARD_MIN_LEN must mirror film's HEADER_LEN — a window shorter than film's \
             header is refused by FilmPlan::derive, so the campaign must refuse it too"
        );
    }

    /// Decode `(reg, value)` state writes through the REAL wire path, so these
    /// fixtures are the same shape a guest's setup prefix produces.
    fn decoded_state(regs: &[(u64, u64)]) -> Vec<SdkEvent> {
        let raw: Vec<(u64, u32, Vec<u8>)> = regs
            .iter()
            .enumerate()
            .map(|(i, (reg, value))| {
                let (id, payload) = state_event(*reg, 0, *value);
                (i as u64 + 1, id, payload)
            })
            .collect();
        crate::sdk_compat::decode_sdk(&raw)
            .expect("the toy state stream decodes")
            .events
    }

    /// A half-published window (one register, not both) is a broken agent, not
    /// an absent billboard — `gpa.zip(len)` used to swallow it into `None`,
    /// which the toy path then accepted silently and the box path mislabelled
    /// as "missing".
    #[test]
    fn a_half_published_billboard_is_malformed() {
        let only_gpa = decoded_state(&[(reg::BILLBOARD_GPA, 0x04e0_0000)]);
        assert!(matches!(
            billboard_window_of(&only_gpa, DEFAULT_GUEST_RAM_LEN),
            Err(GameCampaignError::BillboardMalformed {
                gpa: 0x04e0_0000,
                len: 0,
                ..
            })
        ));
        let only_len = decoded_state(&[(reg::BILLBOARD_LEN, 15_838)]);
        assert!(matches!(
            billboard_window_of(&only_len, DEFAULT_GUEST_RAM_LEN),
            Err(GameCampaignError::BillboardMalformed {
                gpa: 0,
                len: 15_838,
                ..
            })
        ));
        // Neither register: a genuine no-billboard guest (the portable toy).
        assert!(matches!(
            billboard_window_of(&[], DEFAULT_GUEST_RAM_LEN),
            Ok(None)
        ));
        // Both, valid: the window rides out.
        let both = decoded_state(&[
            (reg::BILLBOARD_GPA, 0x04e0_0000),
            (reg::BILLBOARD_LEN, 15_838),
        ]);
        assert!(matches!(
            billboard_window_of(&both, DEFAULT_GUEST_RAM_LEN),
            Ok(Some((0x04e0_0000, 15_838)))
        ));
    }

    fn run(config: ExplorationConfig, seed: u64) -> ExplorationLog {
        run_outcome(config, seed).log
    }

    fn run_outcome(config: ExplorationConfig, seed: u64) -> GameCampaignOutcome {
        let m = GameToyMachine::new();
        let cfg = GameCampaignConfig {
            max_branches: SMOKE_BRANCHES,
            ..GameCampaignConfig::smoke(seed)
        };
        run_game_campaign(m, Box::new(SpecEnvCodec), &cfg, config).unwrap()
    }

    /// Round-8 P1: the deepest branch is tracked from branch 0 and, with a
    /// trace_dir, retained as a real task-65 store entry (env sidecar + full
    /// journal — the re-key/film currency).
    #[test]
    #[cfg_attr(
        miri,
        ignore = "real filesystem (tempdir + TraceStore) — Miri isolation forbids it; the \
                  retention logic itself is Miri-covered via the in-memory campaign path"
    )]
    fn deep_reproducer_is_tracked_and_retained() {
        // Without retention configured: the deep branch is still identified.
        let outcome = run_outcome(ExplorationConfig::PureRandom, 42);
        let deep = outcome.deep.expect("branches ran, so a deepest exists");
        assert!(deep.trace_id.is_none());
        let max_depth = outcome.log.events.iter().map(|e| e.depth).max().unwrap();
        assert_eq!(deep.depth, max_depth);
        // First-deepest: no earlier branch has this depth.
        let first_at = outcome
            .log
            .events
            .iter()
            .position(|e| e.depth == max_depth)
            .unwrap() as u64;
        assert_eq!(deep.branch, first_at);

        // With retention: the store carries the env sidecar + journal.
        let dir = tempfile::tempdir().unwrap();
        let m = GameToyMachine::new();
        let cfg = GameCampaignConfig {
            trace_dir: Some(dir.path().to_path_buf()),
            ..GameCampaignConfig::smoke(42)
        };
        let outcome = run_game_campaign(
            m,
            Box::new(SpecEnvCodec),
            &cfg,
            ExplorationConfig::PureRandom,
        )
        .unwrap();
        let id = outcome
            .deep
            .unwrap()
            .trace_id
            .expect("retention configured");
        let files: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            files
                .iter()
                .any(|f| f.starts_with(&id) && f.ends_with(".env")),
            "env sidecar for {id} in {files:?}"
        );
        assert!(
            files
                .iter()
                .any(|f| f.starts_with(&id) && f.ends_with(".trace")),
            "full journal for {id} in {files:?}"
        );
        // The toy publishes no billboard registers.
        assert_eq!(outcome.billboard, None);
    }

    /// Round-8 P1: a box-mode rollout that ends anywhere but its deadline is
    /// a loud RolloutDied — never recorded as an ordinary sample.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "runs a full game campaign; its durable evidence ledger (task 132) opens a \
                  real file even for `trace_dir: None` (a campaign-lifetime scratch tempdir), \
                  which Miri isolation forbids. Campaign logic is covered by the portable \
                  nextest suite; Miri still guards the crate's only unsafe via \
                  mock::tests::mock_vmm_composes_maps_memory_and_ticks_per_exit."
    )]
    fn a_dying_rollout_fails_the_box_campaign_loudly() {
        /// Seals at a snapshot point, then every rollout crashes (the
        /// retro_serialize-fails-on-frame-1 shape).
        struct SealsThenCrashes {
            sealed: bool,
            vtime: u64,
            next_snap: u64,
            env: Reproducer,
            billboard_drained: bool,
        }
        impl Machine for SealsThenCrashes {
            fn branch(
                &mut self,
                _s: explorer::SnapId,
                env: &Reproducer,
            ) -> Result<(), MachineError> {
                self.env = env.clone();
                Ok(())
            }
            fn replay(&mut self, _s: explorer::SnapId) -> Result<(), MachineError> {
                Ok(())
            }
            fn run(
                &mut self,
                until: &StopConditions,
                _resolve: Option<&explorer::Answer>,
            ) -> Result<StopReason, MachineError> {
                self.vtime += 100;
                // probe_vtime's already-met deadline (0): report position.
                if until.deadline == Some(explorer::Moment(0)) {
                    return Ok(StopReason::Deadline {
                        vtime: explorer::Moment(self.vtime),
                    });
                }
                if !self.sealed {
                    self.sealed = true;
                    return Ok(StopReason::SnapshotPoint {
                        vtime: explorer::Moment(self.vtime),
                    });
                }
                Ok(StopReason::Crash {
                    vtime: explorer::Moment(self.vtime),
                    info: vec![1],
                })
            }
            fn snapshot(
                &mut self,
            ) -> Result<(explorer::SnapId, explorer::EvidenceCut), MachineError> {
                let id = self.next_snap;
                self.next_snap += 1;
                Ok((explorer::SnapId(id), explorer::EvidenceCut::default()))
            }
            fn drop_snap(&mut self, _s: explorer::SnapId) -> Result<(), MachineError> {
                Ok(())
            }
            fn hash(&mut self) -> Result<[u8; 32], MachineError> {
                Ok([0u8; 32])
            }
            fn coverage(&self) -> &[u8] {
                &[]
            }
            fn recorded_env(&self) -> Result<Reproducer, MachineError> {
                Ok(self.env.clone())
            }
            fn sdk_events(&mut self) -> Result<Vec<(u64, u32, Vec<u8>)>, MachineError> {
                // The first drain is the setup prefix: publish the billboard
                // window (a box-shaped guest always does — the round-9
                // billboard gate would otherwise refuse the campaign before
                // any rollout runs).
                if self.sealed && !self.billboard_drained {
                    self.billboard_drained = true;
                    let (gid, gp) = state_event(reg::BILLBOARD_GPA, 0, 0x4000_0000);
                    let (lid, lp) = state_event(reg::BILLBOARD_LEN, 0, 36 * 1024);
                    return Ok(vec![(1_000, gid, gp), (1_001, lid, lp)]);
                }
                Ok(Vec::new())
            }
        }
        let m = SealsThenCrashes {
            sealed: false,
            vtime: 1_000,
            next_snap: 1,
            env: SpecEnvCodec.seeded(0),
            billboard_drained: false,
        };
        let cfg = GameCampaignConfig {
            require_snapshot_point: true,
            ..GameCampaignConfig::smoke(7)
        };
        let err = run_game_campaign(
            m,
            Box::new(SpecEnvCodec),
            &cfg,
            ExplorationConfig::PureRandom,
        )
        .unwrap_err();
        assert!(
            matches!(err, GameCampaignError::RolloutDied { branch: 0, .. }),
            "expected RolloutDied on the first crashed rollout, got {err:?}"
        );
    }

    /// Round-9 P1: a box campaign whose setup prefix never published a valid
    /// billboard `(gpa, len)` window is refused BEFORE any rollout — the
    /// billboard is M0's unconditional film seam, so a windowless
    /// "determinism success" would be a campaign with nothing film can
    /// consume.
    #[test]
    fn box_campaign_without_billboard_refuses() {
        /// Seals at a snapshot point, rollouts end at their deadline (the
        /// otherwise-healthy shape) — but never publishes billboard registers.
        struct SealsNoBillboard {
            sealed: bool,
            vtime: u64,
            next_snap: u64,
        }
        impl Machine for SealsNoBillboard {
            fn branch(
                &mut self,
                _s: explorer::SnapId,
                _env: &Reproducer,
            ) -> Result<(), MachineError> {
                Ok(())
            }
            fn replay(&mut self, _s: explorer::SnapId) -> Result<(), MachineError> {
                Ok(())
            }
            fn run(
                &mut self,
                until: &StopConditions,
                _resolve: Option<&explorer::Answer>,
            ) -> Result<StopReason, MachineError> {
                self.vtime += 100;
                if until.deadline == Some(explorer::Moment(0)) {
                    return Ok(StopReason::Deadline {
                        vtime: explorer::Moment(self.vtime),
                    });
                }
                if !self.sealed {
                    self.sealed = true;
                    return Ok(StopReason::SnapshotPoint {
                        vtime: explorer::Moment(self.vtime),
                    });
                }
                Ok(StopReason::Deadline {
                    vtime: explorer::Moment(self.vtime),
                })
            }
            fn snapshot(
                &mut self,
            ) -> Result<(explorer::SnapId, explorer::EvidenceCut), MachineError> {
                let id = self.next_snap;
                self.next_snap += 1;
                Ok((explorer::SnapId(id), explorer::EvidenceCut::default()))
            }
            fn drop_snap(&mut self, _s: explorer::SnapId) -> Result<(), MachineError> {
                Ok(())
            }
            fn hash(&mut self) -> Result<[u8; 32], MachineError> {
                Ok([0u8; 32])
            }
            fn coverage(&self) -> &[u8] {
                &[]
            }
            fn recorded_env(&self) -> Result<Reproducer, MachineError> {
                Ok(SpecEnvCodec.seeded(0))
            }
        }
        let m = SealsNoBillboard {
            sealed: false,
            vtime: 1_000,
            next_snap: 1,
        };
        let cfg = GameCampaignConfig {
            require_snapshot_point: true,
            ..GameCampaignConfig::smoke(7)
        };
        let err = run_game_campaign(
            m,
            Box::new(SpecEnvCodec),
            &cfg,
            ExplorationConfig::PureRandom,
        )
        .unwrap_err();
        assert!(
            matches!(err, GameCampaignError::BillboardMissing),
            "expected BillboardMissing, got {err:?}"
        );
    }

    /// Round-9 P1: the retained deep trace carries the machine's RECORDED
    /// environment, never the pre-run proposal — two campaigns identical in
    /// every proposal but differing in what the machine actually recorded
    /// must retain different traces (distinct TraceIds).
    #[test]
    #[cfg_attr(
        miri,
        ignore = "real filesystem (tempdir + TraceStore) — Miri isolation forbids it, same as \
                  deep_reproducer_is_tracked_and_retained"
    )]
    fn retained_trace_uses_the_recorded_env_not_the_proposal() {
        /// Delegates to the toy but reports a salted recorded env.
        struct SaltedRecording {
            inner: GameToyMachine,
            salt: u64,
        }
        impl Machine for SaltedRecording {
            fn branch(
                &mut self,
                s: explorer::SnapId,
                env: &Reproducer,
            ) -> Result<(), MachineError> {
                self.inner.branch(s, env)
            }
            fn replay(&mut self, s: explorer::SnapId) -> Result<(), MachineError> {
                self.inner.replay(s)
            }
            fn run(
                &mut self,
                until: &StopConditions,
                resolve: Option<&explorer::Answer>,
            ) -> Result<StopReason, MachineError> {
                self.inner.run(until, resolve)
            }
            fn snapshot(
                &mut self,
            ) -> Result<(explorer::SnapId, explorer::EvidenceCut), MachineError> {
                self.inner.snapshot()
            }
            fn drop_snap(&mut self, s: explorer::SnapId) -> Result<(), MachineError> {
                self.inner.drop_snap(s)
            }
            fn hash(&mut self) -> Result<[u8; 32], MachineError> {
                self.inner.hash()
            }
            fn coverage(&self) -> &[u8] {
                self.inner.coverage()
            }
            fn recorded_env(&self) -> Result<Reproducer, MachineError> {
                Ok(SpecEnvCodec.seeded(self.salt))
            }
            fn sdk_events(&mut self) -> Result<Vec<(u64, u32, Vec<u8>)>, MachineError> {
                self.inner.sdk_events()
            }
        }
        let deep_id = |salt: u64| {
            let dir = tempfile::tempdir().unwrap();
            let cfg = GameCampaignConfig {
                max_branches: 4,
                trace_dir: Some(dir.path().to_path_buf()),
                ..GameCampaignConfig::smoke(7)
            };
            let m = SaltedRecording {
                inner: GameToyMachine::new(),
                salt,
            };
            run_game_campaign(
                m,
                Box::new(SpecEnvCodec),
                &cfg,
                ExplorationConfig::PureRandom,
            )
            .expect("campaign runs")
            .deep
            .expect("deep branch tracked")
            .trace_id
            .expect("retention configured")
        };
        assert_ne!(
            deep_id(1),
            deep_id(2),
            "identical proposals with different recorded envs must retain distinct traces — \
             the trace env must come from recorded_env(), not the proposal"
        );
    }

    /// The ROM serial cross-check's parser (round-9 P1): the exact game-init
    /// line yields the baked hash; GAME_SKIP transcripts yield None; the last
    /// line wins; a truncated hash is not a hash; case is normalized.
    #[test]
    fn serial_rom_sha256_parses_the_game_init_line() {
        let h = "0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea";
        let serial = format!("boot...\nGAME_ROM_SHA256: {h}\nGAME_READY: launching play-agent\n");
        assert_eq!(serial_rom_sha256(serial.as_bytes()), Some(h.to_string()));

        // Case-normalized.
        let upper = format!("GAME_ROM_SHA256: {}\n", h.to_uppercase());
        assert_eq!(serial_rom_sha256(upper.as_bytes()), Some(h.to_string()));

        // The last full-length line wins (a re-printed banner).
        let twice = format!(
            "GAME_ROM_SHA256: {}\nGAME_ROM_SHA256: {h}\n",
            "a".repeat(64)
        );
        assert_eq!(serial_rom_sha256(twice.as_bytes()), Some(h.to_string()));

        // ROM-less image: no line at all.
        assert_eq!(
            serial_rom_sha256(b"GAME_SKIP: no ROM in this image\n"),
            None
        );
        // A truncated hex run is not a hash.
        assert_eq!(
            serial_rom_sha256(format!("GAME_ROM_SHA256: {}\n", &h[..40]).as_bytes()),
            None
        );
        assert_eq!(serial_rom_sha256(b""), None);
    }

    /// The campaign is a pure function of (campaign_seed, config): a rerun is
    /// bit-identical, including every branch's state_hash — the portable
    /// stand-in for the box 25/25 determinism gate.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "runs a full game campaign; its durable evidence ledger (task 132) opens a \
                  real file even for `trace_dir: None` (a campaign-lifetime scratch tempdir), \
                  which Miri isolation forbids. Campaign logic is covered by the portable \
                  nextest suite; Miri still guards the crate's only unsafe via \
                  mock::tests::mock_vmm_composes_maps_memory_and_ticks_per_exit."
    )]
    fn campaign_replays_bit_identically() {
        for config in [ExplorationConfig::PureRandom, ExplorationConfig::SelectorV1] {
            let a = run(config, 7);
            let b = run(config, 7);
            assert_eq!(a, b);
            assert_eq!(a.events.len() as u64, SMOKE_BRANCHES);
        }
    }

    /// Different campaign seeds explore different branches.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "runs a full game campaign; its durable evidence ledger (task 132) opens a \
                  real file even for `trace_dir: None` (a campaign-lifetime scratch tempdir), \
                  which Miri isolation forbids. Campaign logic is covered by the portable \
                  nextest suite; Miri still guards the crate's only unsafe via \
                  mock::tests::mock_vmm_composes_maps_memory_and_ticks_per_exit."
    )]
    fn distinct_seeds_diverge() {
        assert_ne!(
            run(ExplorationConfig::PureRandom, 1),
            run(ExplorationConfig::PureRandom, 2)
        );
    }

    /// The tuple key flows end-to-end: cells are non-empty, keyed on the
    /// gameplay tuple, and depth tracks the toy's level-ups.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "runs a full game campaign; its durable evidence ledger (task 132) opens a \
                  real file even for `trace_dir: None` (a campaign-lifetime scratch tempdir), \
                  which Miri isolation forbids. Campaign logic is covered by the portable \
                  nextest suite; Miri still guards the crate's only unsafe via \
                  mock::tests::mock_vmm_composes_maps_memory_and_ticks_per_exit."
    )]
    fn cells_and_depth_flow_from_sdk_events() {
        let log = run(ExplorationConfig::PureRandom, 42);
        let total: BTreeSet<u64> = log.events.iter().flat_map(|e| e.touched.clone()).collect();
        assert!(!total.is_empty(), "the toy must discover cells");
        // Every key decodes to gameplay mode 1.
        for k in &total {
            assert_eq!(k >> 56, 1, "cell key {k:#x} must carry game mode 1");
        }
        assert!(log.events.iter().any(|e| e.depth > 0), "depth must advance");
    }

    /// A machine standing in for a DEAD play-agent (round-5 P1): its init
    /// crashed to the guest terminal, so no snapshot point is ever surfaced —
    /// and it is perfectly snapshottable where it died, which is exactly why
    /// the workload gate (not a snapshot failure) must refuse the seal.
    struct CrashedAgentMachine {
        vtime: u64,
        next_snap: u64,
        env: Reproducer,
    }

    impl CrashedAgentMachine {
        fn new() -> Self {
            CrashedAgentMachine {
                vtime: 1_000,
                next_snap: 1,
                env: SpecEnvCodec.seeded(0),
            }
        }
    }

    impl Machine for CrashedAgentMachine {
        fn branch(
            &mut self,
            _snap: explorer::SnapId,
            env: &Reproducer,
        ) -> Result<(), MachineError> {
            self.env = env.clone();
            Ok(())
        }
        fn replay(&mut self, _snap: explorer::SnapId) -> Result<(), MachineError> {
            Ok(())
        }
        fn run(
            &mut self,
            _until: &StopConditions,
            _resolve: Option<&explorer::Answer>,
        ) -> Result<StopReason, MachineError> {
            self.vtime += 100;
            Ok(StopReason::Crash {
                vtime: explorer::Moment(self.vtime),
                info: vec![0xDE, 0xAD],
            })
        }
        fn snapshot(&mut self) -> Result<(explorer::SnapId, explorer::EvidenceCut), MachineError> {
            let id = self.next_snap;
            self.next_snap += 1;
            Ok((explorer::SnapId(id), explorer::EvidenceCut::default()))
        }
        fn drop_snap(&mut self, _snap: explorer::SnapId) -> Result<(), MachineError> {
            Ok(())
        }
        fn hash(&mut self) -> Result<[u8; 32], MachineError> {
            Ok([0u8; 32])
        }
        fn coverage(&self) -> &[u8] {
            &[]
        }
        fn recorded_env(&self) -> Result<Reproducer, MachineError> {
            Ok(self.env.clone())
        }
    }

    /// Round-6 P1: the quiet mutator is structurally fault-free — a
    /// 256-mutation sweep (fresh exemplars and chained mutate-of-mutate)
    /// yields envs with ZERO host actions and no standing faults, while the
    /// mutations still genuinely vary (every env distinct).
    #[test]
    fn quiet_mutation_sweep_produces_zero_host_actions() {
        let codec = SpecEnvCodec;
        let mut distinct = BTreeSet::new();
        let mut env = codec.seeded(42);
        for salt in 0..256u64 {
            let source = if salt.is_multiple_of(4) {
                codec.seeded(salt) // a fresh exemplar every few steps…
            } else {
                env.clone() // …and chains, so reseed tables accumulate
            };
            env = quiet_mutate(&source, salt * 31 + 7, 1_000).unwrap();
            let d = explorer::AdapterEnv::decode(&env).unwrap();
            assert!(
                d.spec.host_faults().next().is_none(),
                "salt {salt} minted a host action in the QUIET arm"
            );
            if let environment::EnvSpec::Recorded { standing, .. } = &d.spec {
                assert!(standing.is_empty(), "salt {salt} minted a standing fault");
            }
            assert!(
                !d.spec.reseeds().is_empty(),
                "the mutation must land on the entropy dimension"
            );
            distinct.insert(env.bytes.clone());
        }
        assert_eq!(distinct.len(), 256, "mutations must actually vary");
    }

    /// Defense in depth: an exemplar that already carries fault vocabulary
    /// (impossible in a quiet campaign; a defect if seen) is refused loudly,
    /// so faults can never propagate through the frontier.
    #[test]
    fn quiet_mutate_refuses_fault_carrying_envs() {
        use environment::{EnvSpec, FaultPolicy, HostFault};
        let mut spec = EnvSpec::Seeded {
            seed: 1,
            policy: FaultPolicy::none(),
        };
        spec.perturb(HostFault::InjectInterrupt { vector: 32 }, 5);
        let env = explorer::AdapterEnv {
            base_offset: 0,
            pos: 0,
            spec,
        }
        .encode();
        assert!(matches!(
            quiet_mutate(&env, 1, 100),
            Err(GameCampaignError::QuietEnvCarriesFaults {
                what: "host actions"
            })
        ));
    }

    /// The round-5 P1 regression: in box mode (`require_snapshot_point`), a
    /// guest that dies before `setup_complete` is a loud workload-gate error
    /// — never a sealed dead base and a zero-cell campaign.
    #[test]
    fn a_dead_agent_never_seals_a_base_in_box_mode() {
        let m = CrashedAgentMachine::new();
        let cfg = GameCampaignConfig {
            require_snapshot_point: true,
            ..GameCampaignConfig::smoke(1)
        };
        let err = run_game_campaign(
            m,
            Box::new(SpecEnvCodec),
            &cfg,
            ExplorationConfig::PureRandom,
        )
        .unwrap_err();
        assert!(
            matches!(err, GameCampaignError::SetupNotReached { .. }),
            "expected the workload gate, got {err:?}"
        );
    }

    /// The Signal configuration is refused until a selector artifact exists.
    #[test]
    fn signal_is_refused_loudly() {
        let m = GameToyMachine::new();
        let err = run_game_campaign(
            m,
            Box::new(SpecEnvCodec),
            &GameCampaignConfig::smoke(1),
            ExplorationConfig::Signal,
        )
        .unwrap_err();
        assert!(matches!(err, GameCampaignError::SignalUnavailable));
    }

    /// SelectorV1 exploits (mutated exemplars) while PureRandom never does —
    /// their branch streams diverge under the same campaign seed.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "runs a full game campaign; its durable evidence ledger (task 132) opens a \
                  real file even for `trace_dir: None` (a campaign-lifetime scratch tempdir), \
                  which Miri isolation forbids. Campaign logic is covered by the portable \
                  nextest suite; Miri still guards the crate's only unsafe via \
                  mock::tests::mock_vmm_composes_maps_memory_and_ticks_per_exit."
    )]
    fn selector_v1_and_pure_random_diverge() {
        let a = run(ExplorationConfig::PureRandom, 7);
        let b = run(ExplorationConfig::SelectorV1, 7);
        assert_ne!(
            a.events, b.events,
            "the novelty loop must not equal blind seed search"
        );
    }

    /// The cell key packs and separates its fields.
    #[test]
    fn cell_key_is_injective_over_smb_ranges() {
        let mut seen = BTreeSet::new();
        for mode in 0..4u64 {
            for world in 0..8u64 {
                for level in 0..4u64 {
                    for xb in 0..64u64 {
                        assert!(seen.insert(smb_cell_key(mode, world, level, xb)));
                    }
                }
            }
        }
    }

    /// `smb_cells` ignores non-tuple registers (FRAME floods must not mint
    /// cells) and takes the max depth.
    #[test]
    fn smb_cells_ignores_frame_and_maxes_depth() {
        // The tuple/frame/depth firings through the REAL wire decode.
        let feats = decoded_state(&[
            (reg::GAME_MODE, 1),
            (reg::WORLD, 0),
            (reg::LEVEL, 2),
            (reg::FRAME, 999),
            (reg::X_BUCKET, 3),
            (reg::DEPTH, 2),
            (reg::DEPTH, 5),
            (reg::DEPTH, 4),
        ]);
        let (cells, depth) = smb_cells(&feats);
        assert_eq!(cells, vec![smb_cell_key(1, 0, 2, 3)]);
        assert_eq!(depth, 5);
    }

    /// The `GameToyMachine::capture` stamping is pinned coordinate-by-coordinate:
    /// each window's six events land at `branch_vtime + w*12` plus offsets
    /// `0..=5`, carry the play-agent's wire reg-ids and ops, and the FRAME clock
    /// is exactly `w*12`. Flipping any `+`/`*` in the stamping (or replacing the
    /// whole body) moves an event off a pinned coordinate and fails here — the
    /// portable stand-in for reading the seal on the box.
    #[test]
    fn capture_stamps_each_window_at_base_plus_w_times_twelve() {
        let m = GameToyMachine::new();
        let cap = m.capture();
        // A fresh toy has an empty ancestor prefix: exactly 16 windows × 6
        // events, none dropped, duplicated, or collapsed to a stub vec.
        assert_eq!(cap.len(), 16 * 6, "16 windows of 6 events, no prefix");
        // The per-window event shape, in emission order.
        let regs = [
            reg::GAME_MODE,
            reg::WORLD,
            reg::LEVEL,
            reg::X_BUCKET,
            reg::DEPTH,
            reg::FRAME,
        ];
        // DEPTH is the only `state_max` register (op 1); the rest are `set` (op 0).
        let ops = [0u8, 0, 0, 0, 1, 0];
        for w in 0..16u64 {
            let at0 = TOY_BASE_VTIME + w * 12;
            for (i, (at, id, payload)) in cap[w as usize * 6..w as usize * 6 + 6].iter().enumerate()
            {
                assert_eq!(*at, at0 + i as u64, "window {w} event {i} vtime");
                assert_eq!(
                    *id,
                    (2u32 << 24) | regs[i] as u32,
                    "window {w} event {i} reg id"
                );
                assert_eq!(payload[0], ops[i], "window {w} event {i} op byte");
            }
            // The FRAME clock is `w*12` (pins the value-side `* 12`).
            let frame = &cap[w as usize * 6 + 5].2;
            assert_eq!(
                u64::from_le_bytes(frame[1..9].try_into().unwrap()),
                w * 12,
                "window {w} FRAME clock"
            );
            // GAME_MODE is always gameplay (1) — a fixed emission through the wire codec.
            let mode = &cap[w as usize * 6].2;
            assert_eq!(
                u64::from_le_bytes(mode[1..9].try_into().unwrap()),
                1,
                "window {w} GAME_MODE value"
            );
        }
    }

    /// `run` terminals are exact: with no deadline the toy goes quiescent
    /// `16*12` V-time past its start; with a deadline it advances to and reports
    /// that moment. A body-replacement or an `16*12`→`16/12` lands a different
    /// terminal and fails here.
    #[test]
    fn run_terminal_and_deadline_are_exact() {
        // No deadline: quiescent at base + 192 (the `16 * 12` window budget).
        let mut m = GameToyMachine::new();
        let quiescent = m
            .run(
                &StopConditions {
                    deadline: None,
                    on: StopMask::NONE,
                },
                None,
            )
            .unwrap();
        assert_eq!(
            quiescent,
            StopReason::Quiescent {
                vtime: Moment(TOY_BASE_VTIME + 16 * 12),
            }
        );

        // A future deadline: advance to it and report Deadline at that moment.
        let mut m = GameToyMachine::new();
        let deadline = m
            .run(
                &StopConditions {
                    deadline: Some(Moment(5_000)),
                    on: StopMask::NONE,
                },
                None,
            )
            .unwrap();
        assert_eq!(
            deadline,
            StopReason::Deadline {
                vtime: Moment(5_000),
            }
        );
    }

    /// `snapshot` assigns strictly increasing ids (`+= 1`, never `*=`/`-=`) and
    /// seals the capture prefix half-open at the snapshot vtime (`at <= vt`),
    /// counting exactly the events at or before it.
    #[test]
    fn snapshot_ids_increment_and_seal_is_bounded_at_vtime() {
        let mut m = GameToyMachine::new();
        // Fresh: vtime is the base; only window-0's GAME_MODE lands at exactly
        // the base, so the half-open seal admits exactly one event.
        let (id0, cut0) = m.snapshot().unwrap();
        assert_eq!(id0, explorer::SnapId(1));
        assert_eq!(cut0.at, Moment(TOY_BASE_VTIME));
        assert_eq!(cut0.sdk_events, 1, "only base+0 is <= the base vtime");

        // A second seal at the same state gets the NEXT id — `*= 1` would stall
        // at 1, `-= 1` would fall to 0.
        let (id1, _) = m.snapshot().unwrap();
        assert_eq!(id1, explorer::SnapId(2));

        // Advance one window's worth of stamps into range, then re-seal: window
        // 0's events at base..=base+5 are all <= base+5 while window 1 (base+12)
        // is not — exactly six admitted. Flipping `<=` to `>` would instead
        // admit the 90 events strictly after the cut.
        let _ = m
            .run(
                &StopConditions {
                    deadline: Some(Moment(TOY_BASE_VTIME + 5)),
                    on: StopMask::NONE,
                },
                None,
            )
            .unwrap();
        let (id2, cut2) = m.snapshot().unwrap();
        assert_eq!(id2, explorer::SnapId(3));
        assert_eq!(cut2.at, Moment(TOY_BASE_VTIME + 5));
        assert_eq!(
            cut2.sdk_events, 6,
            "window-0's six events are all <= base+5"
        );
    }

    /// `replay` restores the snapshotted vtime, branch point, and capture
    /// prefix, and rejects an unknown snapshot loudly. The whole-body `Ok(())`
    /// mutant leaves the machine wherever it was and never errors — every
    /// assertion below refutes it.
    #[test]
    fn replay_restores_state_and_rejects_unknown_snapshots() {
        let mut m = GameToyMachine::new();
        // Seal the base moment.
        let (snap_base, _) = m.snapshot().unwrap();
        // Advance vtime, then seal a later, distinct snapshot.
        let _ = m
            .run(
                &StopConditions {
                    deadline: Some(Moment(TOY_BASE_VTIME + 100)),
                    on: StopMask::NONE,
                },
                None,
            )
            .unwrap();
        let (snap_late, _) = m.snapshot().unwrap();

        // Load the late snapshot: branch point + vtime move to base+100 and its
        // multi-event sealed prefix is restored.
        m.replay(snap_late).unwrap();
        assert_eq!(m.vtime, TOY_BASE_VTIME + 100);
        assert_eq!(m.branch_vtime, TOY_BASE_VTIME + 100);
        assert!(
            m.prefix.len() >= 6,
            "the late seal restored a multi-event prefix, got {}",
            m.prefix.len()
        );

        // Replay the base snapshot: every field returns to the base state.
        m.replay(snap_base).unwrap();
        assert_eq!(m.vtime, TOY_BASE_VTIME);
        assert_eq!(m.branch_vtime, TOY_BASE_VTIME);
        assert_eq!(
            m.prefix.len(),
            1,
            "the base seal admitted exactly one event"
        );

        // An unknown snapshot is a loud error, never a silent success.
        assert!(matches!(
            m.replay(explorer::SnapId(999)),
            Err(MachineError::UnknownSnapshot(999)),
        ));
    }
}
