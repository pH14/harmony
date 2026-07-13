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
//! [`link::LinkSensor`] turns them into `(Moment, Feature)` on
//! `LINK_STATE_CHANNEL`; [`smb_cells`] folds those features into the
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
    EnvCodec, Reproducer, Feature, Machine, MachineError, Moment, Prng, RunTrace, Sensor,
    StopConditions, StopMask, StopReason,
};
use link::{LINK_STATE_CHANNEL, LinkSensor};

/// Local mirrors of the play-agent's state-register catalog
/// (`guest/play-agent/src/regs.rs` — the guest crates sit outside this
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

/// Fold a run's `LINK_STATE_CHANNEL` features into the branch's cell set and
/// depth: unpack each feature back into `(reg, value)` (the inverse of
/// `LinkSensor`'s `pack_state`), track the last-seen value per tuple register,
/// mint one cell key at each `X_BUCKET` observation (the tuple's final
/// register each window), and take the max `DEPTH` value. Returns the branch's
/// **distinct** cell keys (sorted) and its depth.
pub fn smb_cells(features: &[(Moment, Feature)]) -> (Vec<u64>, u64) {
    let mut cells = BTreeSet::new();
    let mut depth = 0u64;
    let (mut mode, mut world, mut level) = (0u64, 0u64, 0u64);
    for (_, f) in features {
        if f.channel != LINK_STATE_CHANNEL {
            continue;
        }
        let reg = f.id.0 >> 48;
        let value = f.id.0 & 0x0000_FFFF_FFFF_FFFF;
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
/// (`guest/play-agent/src/agent.rs`: `state_set(REG_FRAME, frame)` then
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
pub fn smb_completed_frames(features: &[(Moment, Feature)]) -> u64 {
    let mut completed = 0u64;
    let mut last: Option<u64> = None;
    for (_, f) in features {
        if f.channel != LINK_STATE_CHANNEL || f.id.0 >> 48 != reg::FRAME {
            continue;
        }
        let value = f.id.0 & 0x0000_FFFF_FFFF_FFFF;
        if last.is_some_and(|prev| prev != value) {
            completed += 1;
        }
        last = Some(value);
    }
    completed
}

/// A novelty-frontier entry for the SelectorV1 configuration.
struct Exemplar {
    env: Reproducer,
}

/// Drive one game campaign against `machine` under `config`. Seals the base
/// at the play-agent's `setup_complete` snapshot point (billboard primed +
/// published, ROM running), then per branch: mint a pure seeded env
/// (PureRandom) or exploit a novel exemplar (SelectorV1), run to the
/// deadline, fold the SDK state events into cells + depth, and log the
/// branch's terminal `state_hash`. The deepest branch's reproducer is
/// retained ([`GameCampaignConfig::trace_dir`], round-8 P1) and the setup
/// prefix's billboard window surfaced — film's inputs, from campaign output
/// alone.
pub fn run_game_campaign<M: Machine>(
    machine: &mut M,
    codec: &dyn EnvCodec,
    cfg: &GameCampaignConfig,
    config: ExplorationConfig,
) -> Result<GameCampaignOutcome, GameCampaignError> {
    if config == ExplorationConfig::Signal {
        return Err(GameCampaignError::SignalUnavailable);
    }

    let (base, base_vtime) = seal_base(machine, cfg)?;
    // Drain the SETUP prefix's SDK capture: the billboard gpa/len registers
    // were published before the seal, so they ride this capture, not any
    // branch's — surface them for film (round-8 P1). Also keeps setup events
    // out of branch 0's trace. The window is VALIDATED at registration (task
    // 103 finding 2): a published-but-unusable window is a loud refusal here,
    // on both paths, never a silent fall back to "no billboard".
    let setup_events = drain_events(machine)?;
    let billboard = billboard_window_of(&setup_events, cfg.guest_ram_len)?;
    // Round-9 P1: on the box (`require_snapshot_point`) the billboard is
    // M0's unconditional film seam — a setup prefix that never published a
    // valid `(gpa, len)` window means film has no input, so the campaign
    // must not proceed to a "determinism PASS" with nothing to show for it.
    // The portable no-SDK toy path (`require_snapshot_point = false`) stays
    // billboard-tolerant.
    if cfg.require_snapshot_point && billboard.is_none() {
        return Err(GameCampaignError::BillboardMissing);
    }

    let until = StopConditions {
        deadline: cfg
            .deadline_delta
            .map(|d| Moment(base_vtime.saturating_add(d))),
        // Quiet arm: no decisions surface, no assertion is a bug here (the
        // agent's markers are reachable HITS, which never stop a run); the
        // deadline (or the guest's own terminal) bounds the rollout.
        on: StopMask::NONE,
    };

    let sensor = LinkSensor::new();
    let mut prng = Prng::new(cfg.campaign_seed);
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    let mut frontier: Vec<Exemplar> = Vec::new();
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
        let step = branch + 1;
        let exploit = config == ExplorationConfig::SelectorV1
            && !frontier.is_empty()
            && !step.is_multiple_of(cfg.explore_period);
        let env = if exploit {
            let pick = (prng.next_u64() % frontier.len() as u64) as usize;
            // The QUIET-ARM exploit move (round-6 P1): reseed the entropy
            // stream at a salt-derived Moment — never `codec.mutate`, whose
            // contract inserts a host-plane fault override.
            quiet_mutate(
                &frontier[pick].env,
                prng.next_u64(),
                cfg.deadline_delta.unwrap_or(TOY_RESEED_WINDOW),
            )?
        } else {
            codec.seeded(prng.next_u64())
        };

        machine.branch(base, &env)?;
        let stop = machine.run(&until, None)?;
        // Round-8 P1: in box mode the play-agent never exits — a rollout that
        // ended anywhere but its deadline DIED (crash/halt mid-rollout) and
        // must fail the campaign loudly, never be hashed and recorded like an
        // ordinary sample (the determinism gate could "pass" over
        // identically-crashed runs). The toy's natural terminal is Quiescent.
        if cfg.require_snapshot_point && !matches!(stop, StopReason::Deadline { .. }) {
            return Err(GameCampaignError::RolloutDied {
                branch,
                stop: format!("{stop:?}"),
            });
        }
        // The rollout's own work evidence, read off the terminal before the
        // stop is moved into the trace: how far the guest's clock actually
        // advanced past the base. A `deadline_delta` of 0 (or a deadline the
        // base already meets) lands here as a zero span.
        let vtime_span = stop.vtime().0.saturating_sub(base_vtime);
        let state_hash = machine.hash()?;
        // Round-9 P1: the retained trace carries the RECORDED environment —
        // genesis-complete, every decision actually drawn, keyed from the
        // branch origin — never the pre-run proposal `env` (whose adapter
        // coords are all-zero, so same-seed runs under different deadlines
        // would collide on TraceId and the sidecar could not reconstruct the
        // actual timeline; the R1 discipline).
        let trace = RunTrace {
            terminal: stop,
            env: machine.recorded_env()?,
            coverage: None,
            events: drain_events(machine)?,
            records: Vec::new(),
        };
        let features = sensor.observe(&trace);
        let (touched, depth) = smb_cells(&features);
        work.branches += 1;
        work.min_vtime_span = work.min_vtime_span.min(vtime_span);
        work.min_completed_frames = work
            .min_completed_frames
            .min(smb_completed_frames(&features));

        // SelectorV1's thin novelty archive: admit an exemplar iff the branch
        // claimed a fresh cell.
        let mut novel = false;
        for &c in &touched {
            if seen.insert(c) {
                novel = true;
            }
        }
        if novel && config == ExplorationConfig::SelectorV1 {
            frontier.push(Exemplar { env });
        }

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

    machine.drop_snap(base)?;

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
                "[conductor] game box DETERMINISM PASS: {repeat}/{repeat} identical per-branch \
                 state_hash sequences (gate floor {DETERMINISM_BAR})."
            )),
            GateVerdict::BelowFloor { repeat } => Some(format!(
                "[conductor] game box determinism smoke: {repeat}/{repeat} identical — BELOW the \
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

/// Drain + decode the machine's SDK event capture (the task-73 seam).
fn drain_events<M: Machine>(
    machine: &mut M,
) -> Result<Vec<(Moment, explorer::GuestEvent)>, MachineError> {
    let raw: Vec<(Moment, u32, Vec<u8>)> = machine
        .sdk_events()?
        .into_iter()
        .map(|(m, id, b)| (Moment(m), id, b))
        .collect();
    Ok(link::decode_events(&raw))
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
    events: &[(Moment, explorer::GuestEvent)],
    guest_ram_len: u64,
) -> Result<Option<(u64, u64)>, GameCampaignError> {
    let (mut gpa, mut len) = (None, None);
    for (_, ev) in events {
        if ev.kind != link::KIND_STATE {
            continue;
        }
        let (Some(explorer::Value::UInt(reg)), Some(explorer::Value::UInt(value))) =
            (ev.attrs.get("reg"), ev.attrs.get("value"))
        else {
            continue;
        };
        match *reg {
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
            Ok(snap) => return Ok((snap, vt)),
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
            Ok(snap) => return Ok((snap, vt)),
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
/// the real wire encode → `link::decode_events` → `LinkSensor` → [`smb_cells`]
/// path, so a register or layout drift breaks a test, not the box).
pub struct GameToyMachine {
    current: Reproducer,
    vtime: u64,
    snaps: std::collections::BTreeMap<u64, (u64, Reproducer)>,
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
            snaps: std::collections::BTreeMap::new(),
            next_snap: 1,
        }
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
        let mut p = Prng::new(seed);
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
/// `link::decode_events` path.
fn state_event(reg: u64, op: u8, value: u64) -> (u32, Vec<u8>) {
    let id = (2u32 << 24) | (reg as u32);
    let mut payload = Vec::with_capacity(9);
    payload.push(op);
    payload.extend_from_slice(&value.to_le_bytes());
    (id, payload)
}

impl Machine for GameToyMachine {
    fn branch(&mut self, snap: explorer::SnapId, env: &Reproducer) -> Result<(), MachineError> {
        let Some((vt, _)) = self.snaps.get(&snap.0) else {
            return Err(MachineError::UnknownSnapshot(snap.0));
        };
        explorer::AdapterEnv::decode(env)?;
        self.vtime = *vt;
        self.current = env.clone();
        Ok(())
    }

    fn replay(&mut self, snap: explorer::SnapId) -> Result<(), MachineError> {
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
        let terminal = self.vtime.saturating_add(16 * 12);
        self.vtime = terminal;
        Ok(StopReason::Quiescent {
            vtime: explorer::Moment(terminal),
        })
    }

    fn snapshot(&mut self) -> Result<explorer::SnapId, MachineError> {
        let id = self.next_snap;
        self.next_snap += 1;
        self.snaps.insert(id, (self.vtime, self.current.clone()));
        Ok(explorer::SnapId(id))
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
        h.update(b"conductor.gametoy.state_hash.v1");
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
        // One tuple emission per window (set ops) + the depth max register —
        // the play-agent's per-window shape, over the real wire layout.
        let mut out = Vec::new();
        for (w, world, level, xb, best) in self.windows() {
            let at = TOY_BASE_VTIME + w * 12;
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
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use explorer::SpecEnvCodec;

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
        fn snapshot(&mut self) -> Result<explorer::SnapId, MachineError> {
            let id = self.next_snap;
            self.next_snap += 1;
            Ok(explorer::SnapId(id))
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

    fn run_box(guest: &mut BoxGuest, cfg: &GameCampaignConfig) -> GameCampaignOutcome {
        run_game_campaign(guest, &SpecEnvCodec, cfg, ExplorationConfig::PureRandom)
            .expect("the box-shaped guest campaigns cleanly")
    }

    /// Task 103 finding 1b — **the vacuous determinism PASS**. Each fixture is
    /// a degenerate budget that the round-9 gate would have called
    /// `DETERMINISM PASS: 25/25` (identical sequences, because identically
    /// empty), and each must now be refused by the gate before any banner —
    /// whatever `--repeat` says.
    #[test]
    fn the_determinism_gate_refuses_a_run_that_did_no_work() {
        // (a) `--max-branches 0`: the gate would compare two empty state_hash
        //     sequences and find them identical.
        let outcome = run_box(&mut BoxGuest::healthy(), &box_cfg(0, Some(2_000_000_000)));
        assert!(outcome.log.events.is_empty());
        assert_eq!(outcome.vacuity(), Some(Vacuity::NoBranches));
        assert_eq!(
            determinism_verdict(&outcome, DETERMINISM_BAR),
            Err(Vacuity::NoBranches),
            "25 repetitions of a zero-branch campaign is not a determinism gate"
        );

        // (b) `--deadline-delta 0`: the rollout's deadline is already met at the
        //     base, so every branch hashes the sealed base itself.
        let mut guest = BoxGuest {
            rollout_span: 0,
            ..BoxGuest::healthy()
        };
        let outcome = run_box(&mut guest, &box_cfg(8, Some(0)));
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
        let mut guest = BoxGuest {
            frame_markers: 0,
            rollout_span: 1,
            ..BoxGuest::healthy()
        };
        let outcome = run_box(&mut guest, &box_cfg(8, Some(1)));
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
        let mut guest = BoxGuest {
            frame_markers: 1,
            rollout_span: 1_000,
            ..BoxGuest::healthy()
        };
        let outcome = run_box(&mut guest, &box_cfg(8, Some(1_000)));
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
        let mut guest = BoxGuest {
            frame_markers: 2,
            rollout_span: 1_000,
            ..BoxGuest::healthy()
        };
        let outcome = run_box(&mut guest, &box_cfg(8, Some(1_000)));
        assert_eq!(outcome.work.min_completed_frames, 1);
        assert_eq!(outcome.vacuity(), None, "one completed frame is real work");

        // (d) One hollow rollout among busy ones still sinks the gate: the
        //     evidence is the WEAKEST rollout, never the total.
        let mut guest = BoxGuest::healthy();
        let outcome = run_box(&mut guest, &box_cfg(8, Some(2_000_000_000)));
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
    fn a_real_campaign_carries_its_work_evidence() {
        let mut guest = BoxGuest::healthy();
        let outcome = run_box(&mut guest, &box_cfg(4, Some(2_000_000_000)));
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
        use explorer::FeatureId;
        let frame = |value: u64| {
            (
                Moment(value + 1),
                Feature {
                    channel: LINK_STATE_CHANNEL,
                    id: FeatureId((reg::FRAME << 48) | value),
                },
            )
        };
        // No markers at all, and a lone marker, are both zero completed frames.
        assert_eq!(smb_completed_frames(&[]), 0);
        assert_eq!(smb_completed_frames(&[frame(0)]), 0);
        // The second marker proves the first frame's body ran.
        assert_eq!(smb_completed_frames(&[frame(0), frame(1)]), 1);
        assert_eq!(smb_completed_frames(&[frame(0), frame(1), frame(2)]), 2);
        // A repeated value is not a new frame (no transition, no work).
        assert_eq!(smb_completed_frames(&[frame(7), frame(7), frame(7)]), 0);
        // The frame clock is a u32 the agent wraps: a wrap is still a frame, so
        // the count is on CHANGE, not on increase.
        assert_eq!(smb_completed_frames(&[frame(u32::MAX as u64), frame(0)]), 1);
        // Non-FRAME registers never count, whatever their values do.
        let other = (
            Moment(9),
            Feature {
                channel: LINK_STATE_CHANNEL,
                id: FeatureId((reg::X_BUCKET << 48) | 3),
            },
        );
        assert_eq!(smb_completed_frames(&[other, other]), 0);
    }

    /// Task 103 finding 2 — **a malformed billboard must not slip past the
    /// round-8 `BillboardMissing` presence check**. Every fixture here is a
    /// window the guest actually published, and every one of them is unusable:
    /// film would read nothing, or the wrong bytes. Each must fail the campaign
    /// loudly, before a single rollout runs.
    #[test]
    fn a_malformed_billboard_refuses_the_campaign() {
        let ram = GameCampaignConfig::smoke(0).guest_ram_len;
        let refuses = |billboard: Option<(u64, u64)>| -> GameCampaignError {
            let mut guest = BoxGuest {
                billboard,
                ..BoxGuest::healthy()
            };
            run_game_campaign(
                &mut guest,
                &SpecEnvCodec,
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
        let mut guest = BoxGuest {
            billboard: Some((0x04e0_0000, BILLBOARD_MIN_LEN)),
            ..BoxGuest::healthy()
        };
        assert_eq!(
            run_box(&mut guest, &box_cfg(2, Some(2_000_000_000))).billboard,
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
        let mut guest = BoxGuest {
            billboard: Some((ram - 4_096, 4_096)),
            ..BoxGuest::healthy()
        };
        assert_eq!(
            run_box(&mut guest, &box_cfg(2, Some(2_000_000_000))).billboard,
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
    fn decoded_state(regs: &[(u64, u64)]) -> Vec<(Moment, explorer::GuestEvent)> {
        let raw: Vec<(Moment, u32, Vec<u8>)> = regs
            .iter()
            .enumerate()
            .map(|(i, (reg, value))| {
                let (id, payload) = state_event(*reg, 0, *value);
                (Moment(i as u64 + 1), id, payload)
            })
            .collect();
        link::decode_events(&raw)
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
        let mut m = GameToyMachine::new();
        let cfg = GameCampaignConfig {
            max_branches: SMOKE_BRANCHES,
            ..GameCampaignConfig::smoke(seed)
        };
        run_game_campaign(&mut m, &SpecEnvCodec, &cfg, config).unwrap()
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
        let mut m = GameToyMachine::new();
        let cfg = GameCampaignConfig {
            trace_dir: Some(dir.path().to_path_buf()),
            ..GameCampaignConfig::smoke(42)
        };
        let outcome =
            run_game_campaign(&mut m, &SpecEnvCodec, &cfg, ExplorationConfig::PureRandom).unwrap();
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
            fn snapshot(&mut self) -> Result<explorer::SnapId, MachineError> {
                let id = self.next_snap;
                self.next_snap += 1;
                Ok(explorer::SnapId(id))
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
        let mut m = SealsThenCrashes {
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
        let err = run_game_campaign(&mut m, &SpecEnvCodec, &cfg, ExplorationConfig::PureRandom)
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
            fn snapshot(&mut self) -> Result<explorer::SnapId, MachineError> {
                let id = self.next_snap;
                self.next_snap += 1;
                Ok(explorer::SnapId(id))
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
        let mut m = SealsNoBillboard {
            sealed: false,
            vtime: 1_000,
            next_snap: 1,
        };
        let cfg = GameCampaignConfig {
            require_snapshot_point: true,
            ..GameCampaignConfig::smoke(7)
        };
        let err = run_game_campaign(&mut m, &SpecEnvCodec, &cfg, ExplorationConfig::PureRandom)
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
            fn snapshot(&mut self) -> Result<explorer::SnapId, MachineError> {
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
            let mut m = SaltedRecording {
                inner: GameToyMachine::new(),
                salt,
            };
            run_game_campaign(&mut m, &SpecEnvCodec, &cfg, ExplorationConfig::PureRandom)
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
    fn distinct_seeds_diverge() {
        assert_ne!(
            run(ExplorationConfig::PureRandom, 1),
            run(ExplorationConfig::PureRandom, 2)
        );
    }

    /// The tuple key flows end-to-end: cells are non-empty, keyed on the
    /// gameplay tuple, and depth tracks the toy's level-ups.
    #[test]
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
        fn snapshot(&mut self) -> Result<explorer::SnapId, MachineError> {
            let id = self.next_snap;
            self.next_snap += 1;
            Ok(explorer::SnapId(id))
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
        let mut m = CrashedAgentMachine::new();
        let cfg = GameCampaignConfig {
            require_snapshot_point: true,
            ..GameCampaignConfig::smoke(1)
        };
        let err = run_game_campaign(&mut m, &SpecEnvCodec, &cfg, ExplorationConfig::PureRandom)
            .unwrap_err();
        assert!(
            matches!(err, GameCampaignError::SetupNotReached { .. }),
            "expected the workload gate, got {err:?}"
        );
    }

    /// The Signal configuration is refused until a selector artifact exists.
    #[test]
    fn signal_is_refused_loudly() {
        let mut m = GameToyMachine::new();
        let err = run_game_campaign(
            &mut m,
            &SpecEnvCodec,
            &GameCampaignConfig::smoke(1),
            ExplorationConfig::Signal,
        )
        .unwrap_err();
        assert!(matches!(err, GameCampaignError::SignalUnavailable));
    }

    /// SelectorV1 exploits (mutated exemplars) while PureRandom never does —
    /// their branch streams diverge under the same campaign seed.
    #[test]
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
        use explorer::FeatureId;
        let f = |reg: u64, value: u64| Feature {
            channel: LINK_STATE_CHANNEL,
            id: FeatureId((reg << 48) | value),
        };
        let feats = vec![
            (Moment(1), f(reg::GAME_MODE, 1)),
            (Moment(2), f(reg::WORLD, 0)),
            (Moment(3), f(reg::LEVEL, 2)),
            (Moment(4), f(reg::FRAME, 999)),
            (Moment(5), f(reg::X_BUCKET, 3)),
            (Moment(6), f(reg::DEPTH, 2)),
            (Moment(7), f(reg::DEPTH, 5)),
            (Moment(8), f(reg::DEPTH, 4)),
        ];
        let (cells, depth) = smb_cells(&feats);
        assert_eq!(cells, vec![smb_cell_key(1, 0, 2, 3)]);
        assert_eq!(depth, 5);
    }
}
