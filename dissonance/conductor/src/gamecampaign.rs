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
    EnvCodec, Environment, Feature, Machine, MachineError, Moment, Prng, RunTrace, Sensor,
    StopConditions, StopMask, StopReason, VTime,
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
        }
    }
}

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
/// reproducer, and the billboard window the guest published in its setup
/// prefix (film's `FilmPlan` inputs, surfaced from campaign output alone).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GameCampaignOutcome {
    /// The discovery-event log (`--logs-out`'s artifact).
    pub log: ExplorationLog,
    /// The deepest branch (present whenever at least one branch ran).
    pub deep: Option<DeepReproducer>,
    /// The billboard `(gpa, len)` from the setup prefix's `REG_BILLBOARD_*`
    /// registers, when the guest published them (the toy does not).
    pub billboard: Option<(u64, u64)>,
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
    env: &Environment,
    salt: u64,
    window: u64,
) -> Result<Environment, GameCampaignError> {
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

/// A novelty-frontier entry for the SelectorV1 configuration.
struct Exemplar {
    env: Environment,
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
    // out of branch 0's trace.
    let setup_events = drain_events(machine)?;
    let billboard = billboard_window_of(&setup_events);

    let until = StopConditions {
        deadline: cfg
            .deadline_delta
            .map(|d| VTime(base_vtime.saturating_add(d))),
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
        let state_hash = machine.hash()?;
        let trace = RunTrace {
            terminal: stop,
            env: env.clone(),
            coverage: None,
            events: drain_events(machine)?,
            records: Vec::new(),
        };
        let (touched, depth) = smb_cells(&sensor.observe(&trace));

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

/// Extract the billboard `(gpa, len)` from the setup prefix's decoded state
/// events (the last write of each register wins — the agent publishes once).
fn billboard_window_of(events: &[(Moment, explorer::GuestEvent)]) -> Option<(u64, u64)> {
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
    gpa.zip(len)
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
            deadline: Some(VTime(vt.saturating_add(cfg.setup_deadline_delta))),
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
                        deadline: Some(VTime(vt.saturating_add(cfg.snapshot_retry_step))),
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
    current: Environment,
    vtime: u64,
    snaps: std::collections::BTreeMap<u64, (u64, Environment)>,
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
    fn branch(&mut self, snap: explorer::SnapId, env: &Environment) -> Result<(), MachineError> {
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
            vtime: explorer::VTime(terminal),
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

    fn recorded_env(&self) -> Result<Environment, MachineError> {
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

    fn run(config: ExplorationConfig, seed: u64) -> ExplorationLog {
        run_outcome(config, seed).log
    }

    fn run_outcome(config: ExplorationConfig, seed: u64) -> GameCampaignOutcome {
        let mut m = GameToyMachine::new();
        run_game_campaign(
            &mut m,
            &SpecEnvCodec,
            &GameCampaignConfig::smoke(seed),
            config,
        )
        .unwrap()
    }

    /// Round-8 P1: the deepest branch is tracked from branch 0 and, with a
    /// trace_dir, retained as a real task-65 store entry (env sidecar + full
    /// journal — the re-key/film currency).
    #[test]
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
            env: Environment,
        }
        impl Machine for SealsThenCrashes {
            fn branch(
                &mut self,
                _s: explorer::SnapId,
                env: &Environment,
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
                if until.deadline == Some(explorer::VTime(0)) {
                    return Ok(StopReason::Deadline {
                        vtime: explorer::VTime(self.vtime),
                    });
                }
                if !self.sealed {
                    self.sealed = true;
                    return Ok(StopReason::SnapshotPoint {
                        vtime: explorer::VTime(self.vtime),
                    });
                }
                Ok(StopReason::Crash {
                    vtime: explorer::VTime(self.vtime),
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
            fn recorded_env(&self) -> Result<Environment, MachineError> {
                Ok(self.env.clone())
            }
        }
        let mut m = SealsThenCrashes {
            sealed: false,
            vtime: 1_000,
            next_snap: 1,
            env: SpecEnvCodec.seeded(0),
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

    /// The campaign is a pure function of (campaign_seed, config): a rerun is
    /// bit-identical, including every branch's state_hash — the portable
    /// stand-in for the box 25/25 determinism gate.
    #[test]
    fn campaign_replays_bit_identically() {
        for config in [ExplorationConfig::PureRandom, ExplorationConfig::SelectorV1] {
            let a = run(config, 7);
            let b = run(config, 7);
            assert_eq!(a, b);
            assert_eq!(a.events.len(), 64);
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
        env: Environment,
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
            env: &Environment,
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
                vtime: explorer::VTime(self.vtime),
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
        fn recorded_env(&self) -> Result<Environment, MachineError> {
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
