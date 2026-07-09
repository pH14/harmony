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
}

impl GameCampaignConfig {
    /// A small portable/smoke configuration.
    pub fn smoke(campaign_seed: u64) -> Self {
        GameCampaignConfig {
            campaign_seed,
            max_branches: 64,
            deadline_delta: None,
            explore_period: 4,
            snapshot_retry_step: 1_000_000,
            snapshot_max_attempts: 100_000,
            setup_deadline_delta: 30_000_000_000,
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

/// Drive one game campaign against `machine` under `config` and return its
/// discovery-event log. Seals the base at the play-agent's `setup_complete`
/// snapshot point (billboard published, ROM running), then per branch: mint a
/// pure seeded env (PureRandom) or exploit a novel exemplar (SelectorV1), run
/// to the deadline, fold the SDK state events into cells + depth, and log the
/// branch's terminal `state_hash`.
pub fn run_game_campaign<M: Machine>(
    machine: &mut M,
    codec: &dyn EnvCodec,
    cfg: &GameCampaignConfig,
    config: ExplorationConfig,
) -> Result<ExplorationLog, GameCampaignError> {
    if config == ExplorationConfig::Signal {
        return Err(GameCampaignError::SignalUnavailable);
    }

    let (base, base_vtime) = seal_base(machine, cfg)?;
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

    for branch in 0..cfg.max_branches {
        let step = branch + 1;
        let exploit = config == ExplorationConfig::SelectorV1
            && !frontier.is_empty()
            && !step.is_multiple_of(cfg.explore_period);
        let env = if exploit {
            let pick = (prng.next_u64() % frontier.len() as u64) as usize;
            codec.mutate(&frontier[pick].env, prng.next_u64())
        } else {
            codec.seeded(prng.next_u64())
        };

        machine.branch(base, &env)?;
        let stop = machine.run(&until, None)?;
        let state_hash = machine.hash()?;
        let raw: Vec<(Moment, u32, Vec<u8>)> = machine
            .sdk_events()?
            .into_iter()
            .map(|(m, id, b)| (Moment(m), id, b))
            .collect();
        let trace = RunTrace {
            terminal: stop,
            env: env.clone(),
            coverage: None,
            events: link::decode_events(&raw),
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

        events.push(DiscoveryEvent {
            branch,
            touched,
            depth,
            state_hash: hex(&state_hash),
        });
    }

    machine.drop_snap(base)?;
    Ok(ExplorationLog {
        workload: "smb".to_string(),
        config,
        seed: cfg.campaign_seed,
        events,
    })
}

/// Seal the campaign base. Preferred boundary: the play-agent's
/// `setup_complete` **snapshot point** (the billboard gpa/len registers are
/// published in the setup prefix, so every branch inherits them). Arm the
/// snapshot-point class and run; if the guest surfaces it, seal there. A
/// machine with no SDK (the portable toy) runs to its own terminal instead —
/// fall back to the task-60 retry loop (snapshot, stepping past
/// non-snapshottable boundaries).
fn seal_base<M: Machine>(
    machine: &mut M,
    cfg: &GameCampaignConfig,
) -> Result<(explorer::SnapId, u64), MachineError> {
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
        if let Ok(snap) = machine.snapshot() {
            return Ok((snap, vt));
        }
    } else {
        vt = stop.vtime().0;
    }
    // Fallback: the task-60 seal-retry loop.
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        match machine.snapshot() {
            Ok(snap) => return Ok((snap, vt)),
            Err(MachineError::NotQuiescent) => {
                if attempts >= cfg.snapshot_max_attempts {
                    return Err(MachineError::NotQuiescent);
                }
                let stop = machine.run(
                    &StopConditions {
                        deadline: Some(VTime(vt.saturating_add(cfg.snapshot_retry_step))),
                        on: StopMask::NONE,
                    },
                    None,
                )?;
                if !matches!(stop, StopReason::Deadline { .. }) {
                    return Err(MachineError::NotQuiescent);
                }
                vt = stop.vtime().0;
            }
            Err(e) => return Err(e),
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

    /// The simulated per-window SMB observations for the current env's seed:
    /// `(window, world, level, x_bucket, depth_ordinal)`.
    fn windows(&self) -> Vec<(u64, u64, u64, u64, u64)> {
        let seed = explorer::AdapterEnv::decode(&self.current)
            .map(|d| d.spec.seed())
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
        let mut m = GameToyMachine::new();
        run_game_campaign(
            &mut m,
            &SpecEnvCodec,
            &GameCampaignConfig::smoke(seed),
            config,
        )
        .unwrap()
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
