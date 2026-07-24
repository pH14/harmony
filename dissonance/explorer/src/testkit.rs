// SPDX-License-Identifier: AGPL-3.0-or-later
//! Crate-internal test kit for the Differential campaign tests: a pure,
//! deterministic scripted [`Machine`] that emits reducible SDK state, a trivial
//! reproducer codec, and the campaign/ledger/coordinator builders shared by the
//! `campaign` and `retention` test modules.

use std::collections::BTreeMap;
use std::rc::Rc;

use revision_coordinator::{CampaignConfigId, Coordinator, EvidenceBatchId, MemLedger};
use sdk_events::{Classification, DeclaredPoint, NS_ASSERT, NS_STATE, UpdateOp, ValueShape};

use crate::campaign::{CATALOG_EVENT_ID, CampaignConfig, DifferentialCampaign, Ingress};
use crate::defaults::{DeclineTactic, GenesisSelector};
use crate::error::MachineError;
use crate::evidence::{CompletedRunEvidence, DefaultObservationCells, EvidenceRole, RunId};
use crate::ledger::EvidenceLedger;
use crate::retention::RetentionProfile;
use crate::seam::{EnvCodec, Machine};
use crate::spine::{EvidenceCut, Moment};
use crate::{Answer, Reproducer, SnapId, StopConditions, StopReason};

// ---- v1 SDK-catalog wire constants and encoders (consolidated per PR #155 F5) ----
//
// The `wire` module's catalog/firing byte constants are `pub(crate)` to
// `sdk-events`, so the explorer's test fixtures mirror the ones they need here.
// A **v1** catalog carries an explicit assertion verb (via the kind byte), so a
// firing decodes with its `AssertType` populated — the form the absence fold
// ([`satisfies_must_hit`](crate::occurrence)) and the [`OccurrenceOracle`](crate::occurrence)
// key on. The production host encoder (`sdk_events::encode_v2_declaration`) emits
// only wire v2, which declares no verb (`AssertType` decodes `None`), so these
// fixtures are the only way a crate test drives the verdict folds. PR #155 F5
// flagged three hand-written copies of this encoder (occurrence.rs ×2,
// retention.rs); they now all call [`encode_v1_catalog`]/[`assert_firing`].

/// v1 catalog kind byte: an `always` assertion (verb [`AssertType::Always`]).
pub(crate) const KIND_ALWAYS: u8 = 0;
/// v1 catalog kind byte: a `sometimes` assertion (must-hit, verb `Sometimes`).
pub(crate) const KIND_SOMETIMES: u8 = 1;
/// v1 catalog kind byte: an `unreachable` assertion (must-not-hit, verb `Unreachable`).
pub(crate) const KIND_UNREACHABLE: u8 = 3;
/// v1 catalog kind byte: a state register (unresolved base op on the v1 wire).
pub(crate) const KIND_STATE: u8 = 4;

/// Assertion firing disposition: a HIT (condition true — `sometimes`/`reachable`).
pub(crate) const DISP_HIT: u8 = 0;
/// Assertion firing disposition: a VIOLATION (condition false — `always`/`unreachable`).
pub(crate) const DISP_VIOLATION: u8 = 1;

/// The 24-bit runtime local-id mask (the `wire::LOCAL_MASK`), for composing an
/// `event_id` from a namespace and local id.
const LOCAL_MASK: u32 = (1 << 24) - 1;

/// Encode a **v1 SDK catalog** declaration from `(kind, local, name)` points:
/// `magic("SDKC") + version(1) + count(u32) + [kind(u8), local(u32), name_lp16]*`.
/// The v1 wire carries the assertion verb in the kind byte (see [`KIND_SOMETIMES`]
/// et al.), so a firing at a declared coordinate decodes with its `AssertType`
/// resolved — what the verdict folds key on. The single fixture the three F5
/// copies collapse into.
pub(crate) fn encode_v1_catalog(points: &[(u8, u32, &str)]) -> Vec<u8> {
    let magic = u32::from_le_bytes(*b"SDKC");
    let mut b = Vec::new();
    b.extend_from_slice(&magic.to_le_bytes());
    b.push(1); // SDK_WIRE_VERSION (v1)
    b.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for (kind, local, name) in points {
        b.push(*kind);
        b.extend_from_slice(&local.to_le_bytes());
        b.extend_from_slice(&(name.len() as u16).to_le_bytes());
        b.extend_from_slice(name.as_bytes());
    }
    b
}

/// A v1 **assertion firing** `[disposition u8][detail_len u16 = 0]` at `NS_ASSERT`
/// `local`, returned as the `(event_id, body)` pair [`decode_binary`](sdk_events::decode_binary)
/// consumes. `DISP_HIT`/`DISP_VIOLATION` select the disposition; empty detail.
pub(crate) fn assert_firing(local: u32, disposition: u8) -> (u32, Vec<u8>) {
    let id = ((NS_ASSERT as u32) << 24) | (local & LOCAL_MASK);
    let mut body = vec![disposition];
    body.extend_from_slice(&0u16.to_le_bytes());
    (id, body)
}

/// A v1 **state firing** `[op u8 = set][value u64]` at `NS_STATE` `reg`, returned
/// as the `(event_id, body)` pair [`decode_binary`](sdk_events::decode_binary)
/// consumes. On the v1 wire the register is unresolved (no declared base op), so
/// this is reportable-but-non-reducible evidence.
pub(crate) fn state_firing(reg: u32, value: u64) -> (u32, Vec<u8>) {
    let id = ((NS_STATE as u32) << 24) | (reg & LOCAL_MASK);
    let mut body = vec![0u8]; // op byte: set (the v1 wire STATE_SET)
    body.extend_from_slice(&value.to_le_bytes());
    (id, body)
}

/// One scripted state emission: at `at`, register `reg` takes value `value`
/// (declared `set`), and that moment is a sealable point.
#[derive(Clone)]
pub(crate) struct Emit {
    pub(crate) at: u64,
    pub(crate) reg: u32,
    pub(crate) value: u64,
}

/// A run's script: its state emissions (in moment order) and terminal moment.
#[derive(Clone)]
pub(crate) struct Program {
    pub(crate) emits: Vec<Emit>,
    pub(crate) terminal: u64,
}

/// A pure, deterministic machine whose SDK trajectory is a function of the
/// branch base and the env's seed — same `(base, env)` ⇒ identical
/// trajectory, identical cuts, identical hashes.
///
/// **Lineage-aware** (task 132): a snapshot captures the emitted prefix, and
/// a `branch` off it restores that prefix into the child (the production
/// machine restores the ancestor SDK prefix, so counts are CUMULATIVE
/// through the lineage). A child program's own emits must land at or after
/// the branch moment (test-authoring discipline, mirroring the physical cut
/// contract).
pub(crate) struct ScriptedMachine {
    program: Rc<dyn Fn(u64) -> Program>,
    regs: Vec<(u32, UpdateOp)>,
    seed: u64,
    emits: Vec<Emit>,
    terminal: u64,
    cursor: usize,
    clock: u64,
    recorded: Reproducer,
    /// snap id -> (moment, included count, emitted prefix).
    snaps: BTreeMap<u64, (u64, u64, Vec<Emit>)>,
    next_snap: u64,
    /// Half-open clock windows `[lo, hi)` at which a `snapshot` is refused
    /// [`MachineError::NotQuiescent`] — the toy's mid-exit model (a
    /// reseed-shifted RNG draw in flight at the boundary, task 136).
    non_quiescent: Vec<(u64, u64)>,
    /// The env's staged-schedule `Moment`s, **relative to the branch origin**
    /// (the toy analog of a marker-carrying reproducer): re-anchored at each
    /// `branch` into [`staged_abs`](Self::staged_abs), exactly as the
    /// production adapter re-anchors blob-frame keys.
    markers: Vec<u64>,
    /// The absolute staged `Moment`s of the current branch. A `snapshot` while
    /// any lies ahead of the clock is refused `NotQuiescent` (the production
    /// `SnapshotWhileArmed`, collapsed at the adapter); a Moment at-or-behind
    /// the clock has drained (the server drains exactly at each `Moment`).
    staged_abs: Vec<u64>,
    /// Every `run` deadline observed, in order — lets a test pin the
    /// marker-clamped leg schedule (and the zero-probe re-materialization).
    pub(crate) deadlines_seen: Vec<Option<u64>>,
}

impl ScriptedMachine {
    pub(crate) fn new(regs: Vec<(u32, UpdateOp)>, program: Rc<dyn Fn(u64) -> Program>) -> Self {
        Self {
            program,
            regs,
            seed: 0,
            emits: Vec::new(),
            terminal: 0,
            cursor: 0,
            clock: 0,
            recorded: Reproducer {
                blob_version: 1,
                bytes: 0u64.to_le_bytes().to_vec(),
            },
            snaps: BTreeMap::new(),
            next_snap: 1,
            non_quiescent: Vec::new(),
            markers: Vec::new(),
            staged_abs: Vec::new(),
            deadlines_seen: Vec::new(),
        }
    }

    /// Refuse snapshots [`MachineError::NotQuiescent`] while the clock sits in
    /// any half-open `[lo, hi)` window (the task-136 mid-exit model).
    pub(crate) fn with_non_quiescent(mut self, windows: Vec<(u64, u64)>) -> Self {
        self.non_quiescent = windows;
        self
    }

    /// Stage `markers` (relative to the branch origin) on every `branch` —
    /// the toy analog of a marker-carrying env. Pair with a codec whose
    /// `staged_moments` reports the same keys.
    pub(crate) fn with_markers(mut self, markers: Vec<u64>) -> Self {
        self.markers = markers;
        self
    }

    fn catalog(&self) -> Vec<u8> {
        let mut points: Vec<DeclaredPoint> = self
            .regs
            .iter()
            .map(|(reg, op)| DeclaredPoint {
                namespace: NS_STATE,
                local: *reg,
                name: format!("r{reg}"),
                classification: Classification::State,
                value_shape: Some(ValueShape::U64),
                base_op: Some(*op),
                expectation: None,
            })
            .collect();
        // A declared, never-fired must-hit (`sometimes`) assertion point: every
        // campaign's finalized absence view then has observable content, so the
        // absence accessor and its retention-survival are exact-value testable.
        points.push(DeclaredPoint {
            namespace: sdk_events::NS_ASSERT,
            local: 99,
            name: "never-satisfied".into(),
            classification: Classification::Occurrence,
            value_shape: None,
            base_op: None,
            expectation: Some(sdk_events::Expectation::MustHit),
        });
        sdk_events::encode_v2_declaration(&points).expect("valid v2 declaration")
    }

    pub(crate) fn seed_of(env: &Reproducer) -> u64 {
        let mut b = [0u8; 8];
        let n = env.bytes.len().min(8);
        b[..n].copy_from_slice(&env.bytes[..n]);
        u64::from_le_bytes(b)
    }

    fn included_at(&self, moment: u64) -> u64 {
        self.emits.iter().filter(|e| e.at <= moment).count() as u64
    }
}

impl Machine for ScriptedMachine {
    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
        // Restore the snapshot's emitted prefix (the ancestor SDK prefix a
        // production branch restores into the child), then append the
        // child's own script at BRANCH-RELATIVE moments (offset by the
        // branch point, so a child's own events land at or after it — the
        // physical cut contract). An unknown snap restores nothing (the
        // pre-lineage toy behavior, kept for direct-drive unit tests); a
        // genesis snap has moment 0, so genesis scripts read as absolute.
        let (moment, included, prefix) =
            self.snaps
                .get(&snap.0)
                .cloned()
                .unwrap_or((0, 0, Vec::new()));
        self.seed = Self::seed_of(env);
        let prog = (self.program)(self.seed);
        self.clock = moment;
        self.cursor = included as usize;
        self.emits = prefix;
        self.emits.extend(prog.emits.into_iter().map(|e| Emit {
            at: moment + e.at,
            ..e
        }));
        self.terminal = moment + prog.terminal;
        self.recorded = env.clone();
        // Re-anchor the env's staged Moments at the branch origin (the
        // production adapter's wire conversion), staging them for this branch.
        self.staged_abs = self.markers.iter().map(|&m| moment + m).collect();
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        // Restore to the sealed moment (verbatim); the campaign tests never
        // diverge a replay, so reset the cursor to the snapshot's included
        // count.
        let (moment, included, prefix) = self
            .snaps
            .get(&snap.0)
            .cloned()
            .ok_or(MachineError::UnknownSnapshot(snap.0))?;
        self.clock = moment;
        self.cursor = included as usize;
        self.emits = prefix;
        // A seal only ever exists fully drained (snapshot refuses while any
        // staged Moment lies ahead), so the verbatim restore has nothing
        // staged.
        self.staged_abs.clear();
        Ok(())
    }

    fn run(
        &mut self,
        until: &StopConditions,
        _resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        self.deadlines_seen.push(until.deadline.map(|d| d.0));
        match until.deadline {
            // Materialize replay: advance to the sealable point at the deadline.
            Some(d) => {
                while self.cursor < self.emits.len() && self.emits[self.cursor].at < d.0 {
                    self.clock = self.emits[self.cursor].at;
                    self.cursor += 1;
                }
                if self.cursor < self.emits.len() && self.emits[self.cursor].at == d.0 {
                    self.clock = d.0;
                    self.cursor += 1;
                    Ok(StopReason::SnapshotPoint { vtime: Moment(d.0) })
                } else if d.0 < self.terminal {
                    // A mid-run deadline lands exactly (the server's armed
                    // `run_until` stops between instructions at the deadline
                    // — the exact-arrival model the marker clamp rides on).
                    self.clock = d.0;
                    Ok(StopReason::Deadline { vtime: Moment(d.0) })
                } else {
                    self.clock = self.terminal.max(d.0);
                    Ok(StopReason::Quiescent {
                        vtime: Moment(self.clock),
                    })
                }
            }
            // Rollout: surface each emission's sealable point, then terminate.
            // An open-loop rollout stops AT its terminal and captures nothing
            // beyond it — an emit past the terminal is not surfaced here; it
            // belongs to a later marker-clamped run-forward (a deadline leg),
            // not the rollout. Modelling the terminal faithfully is what lets
            // the seal-suffix truncation surface (task 144 / hm-aqf0): the
            // pre-144 toy surfaced every emit regardless of the terminal, so
            // the rollout always already carried the advanced span and the
            // seal cut could never out-count the rollout's graph rows.
            None => {
                if self.cursor < self.emits.len() && self.emits[self.cursor].at <= self.terminal {
                    self.clock = self.emits[self.cursor].at;
                    self.cursor += 1;
                    Ok(StopReason::SnapshotPoint {
                        vtime: Moment(self.clock),
                    })
                } else {
                    self.clock = self.terminal;
                    Ok(StopReason::Quiescent {
                        vtime: Moment(self.terminal),
                    })
                }
            }
        }
    }

    fn snapshot(&mut self) -> Result<(SnapId, EvidenceCut), MachineError> {
        // The production seal contract, collapsed as the adapter does: a
        // staged Moment still ahead (`SnapshotWhileArmed`) or a scripted
        // mid-exit window (`NotQuiescent`) both refuse the seal with
        // [`MachineError::NotQuiescent`].
        if self.staged_abs.iter().any(|&m| m > self.clock)
            || self
                .non_quiescent
                .iter()
                .any(|&(lo, hi)| self.clock >= lo && self.clock < hi)
        {
            return Err(MachineError::NotQuiescent);
        }
        let id = self.next_snap;
        self.next_snap += 1;
        let included = self.included_at(self.clock);
        let prefix: Vec<Emit> = self.emits.iter().take(included as usize).cloned().collect();
        self.snaps.insert(id, (self.clock, included, prefix));
        // Stamp the cut in the PRODUCTION frame (task 144, folding hm-udgn /
        // F6): the server stamps `vmm.sdk_events().len()` — raw capture
        // positions, **catalog included** (`control.rs`) — not a firings-only
        // count. `sdk_events()` here returns `[catalog] + firings[..cursor]`,
        // and `cursor == included` at a valid seal, so the honest stamp is
        // `1 + included` (the one catalog position plus the included firings).
        // This keeps every cut on ONE frame with the raw-capture lengths the
        // suffix decode and the seal reconciliation use — the toy's old
        // firings-only stamp was the sole reason those two frames diverged.
        Ok((
            SnapId(id),
            EvidenceCut {
                at: Moment(self.clock),
                sdk_events: 1 + included,
            },
        ))
    }

    fn drop_snap(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.snaps.remove(&snap.0);
        Ok(())
    }

    fn hash(&mut self) -> Result<[u8; 32], MachineError> {
        let mut h = [0u8; 32];
        h[..8].copy_from_slice(&self.seed.to_le_bytes());
        h[8..16].copy_from_slice(&self.clock.to_le_bytes());
        Ok(h)
    }

    fn coverage(&self) -> &[u8] {
        &[]
    }

    fn recorded_env(&self) -> Result<Reproducer, MachineError> {
        Ok(self.recorded.clone())
    }

    fn sdk_events(&mut self) -> Result<Vec<(u64, u32, Vec<u8>)>, MachineError> {
        // The catalog (schema) then every firing emitted up to the cursor.
        let mut out = vec![(0u64, CATALOG_EVENT_ID, self.catalog())];
        for e in self.emits.iter().take(self.cursor) {
            let id = ((NS_STATE as u32) << 24) | (e.reg & 0x00FF_FFFF);
            let op = self
                .regs
                .iter()
                .find(|(r, _)| *r == e.reg)
                .map(|(_, op)| *op)
                .unwrap_or(UpdateOp::Set);
            let op_byte = match op {
                UpdateOp::Set => 0u8,
                UpdateOp::Max => 1,
                UpdateOp::Min => 2,
                UpdateOp::Accumulate => 3,
            };
            let mut bytes = vec![op_byte];
            bytes.extend_from_slice(&e.value.to_le_bytes());
            out.push((e.at, id, bytes));
        }
        Ok(out)
    }
}

/// A trivial reproducer codec: the seed is the whole env; a mutation salts the
/// seed; composition returns the branch-local (genesis-complete by
/// construction in these genesis-rooted tests).
pub(crate) struct ToyCodec;

impl EnvCodec for ToyCodec {
    fn seeded(&self, seed: u64) -> Reproducer {
        Reproducer {
            blob_version: 1,
            bytes: seed.to_le_bytes().to_vec(),
        }
    }
    fn mutate(
        &self,
        base: &Reproducer,
        salt: u64,
    ) -> Result<Reproducer, crate::error::EnvCodecError> {
        let s = ScriptedMachine::seed_of(base) ^ salt;
        Ok(self.seeded(s))
    }
    fn compose(
        &self,
        _base: &Reproducer,
        branch_local: &Reproducer,
    ) -> Result<Reproducer, crate::error::EnvCodecError> {
        Ok(branch_local.clone())
    }
}

/// The toy codec of a marker-carrying reproducer (task 136): every env
/// declares the same staged-schedule keys, mirroring a
/// [`ScriptedMachine::with_markers`] machine built with the identical list —
/// exactly the production invariant (the adapter's codec and server stage from
/// the same blob).
pub(crate) struct MarkerCodec {
    pub(crate) markers: Vec<u64>,
}

impl EnvCodec for MarkerCodec {
    fn seeded(&self, seed: u64) -> Reproducer {
        ToyCodec.seeded(seed)
    }
    fn mutate(
        &self,
        base: &Reproducer,
        salt: u64,
    ) -> Result<Reproducer, crate::error::EnvCodecError> {
        ToyCodec.mutate(base, salt)
    }
    fn compose(
        &self,
        base: &Reproducer,
        branch_local: &Reproducer,
    ) -> Result<Reproducer, crate::error::EnvCodecError> {
        ToyCodec.compose(base, branch_local)
    }
    fn staged_moments(&self, _env: &Reproducer) -> Result<Vec<u64>, crate::error::EnvCodecError> {
        Ok(self.markers.clone())
    }
}

// ---- builders ----

pub(crate) fn config(cap: usize, budget: u64) -> CampaignConfig {
    CampaignConfig {
        candidate_cap: cap,
        replay_budget: budget,
        ingress: Ingress::Binary,
        retention: RetentionProfile::Full,
        evidence_budget: None,
        ..CampaignConfig::default()
    }
}

pub(crate) fn coordinator() -> Coordinator {
    Coordinator::genesis(
        Box::new(MemLedger::new()),
        CampaignConfigId::digest(b"test-config"),
    )
    .expect("genesis")
}

pub(crate) fn ledger() -> (tempfile::TempDir, EvidenceLedger) {
    let dir = tempfile::tempdir().expect("tempdir");
    let led = EvidenceLedger::open(&dir.path().join("evidence.log")).expect("open");
    (dir, led)
}

/// A campaign over a scripted machine whose runs emit `reg=1` (`set`) at
/// `value = seed % modulo`, at a single sealable moment, then terminate.
pub(crate) fn simple_program(modulo: u64) -> Rc<dyn Fn(u64) -> Program> {
    Rc::new(move |seed| Program {
        emits: vec![Emit {
            at: 10,
            reg: 1,
            value: seed % modulo,
        }],
        terminal: 20,
    })
}

pub(crate) fn campaign(
    program: Rc<dyn Fn(u64) -> Program>,
    cfg: CampaignConfig,
    seed: u64,
) -> (tempfile::TempDir, DifferentialCampaign<ScriptedMachine>) {
    let (dir, led) = ledger();
    let camp = campaign_over(program, cfg, seed, led);
    (dir, camp)
}

/// Like [`campaign`], but over an explicit (possibly pre-existing) ledger —
/// the restart/rebuild tests reopen a ledger file and resume with it.
pub(crate) fn campaign_over(
    program: Rc<dyn Fn(u64) -> Program>,
    cfg: CampaignConfig,
    seed: u64,
    led: EvidenceLedger,
) -> DifferentialCampaign<ScriptedMachine> {
    let machine = ScriptedMachine::new(vec![(1, UpdateOp::Set)], program);
    campaign_with(machine, Box::new(ToyCodec), cfg, seed, led)
}

/// Like [`campaign_over`], but over an explicit machine and codec — the
/// task-136 SealAnchor regressions pair a marker-staging [`ScriptedMachine`]
/// with its matching codec, and the task-155 gates pair a [`VerbMachine`] with a
/// [`MarkerCodec`]. Generic over any [`Machine`].
pub(crate) fn campaign_with<M: Machine>(
    machine: M,
    codec: Box<dyn EnvCodec>,
    cfg: CampaignConfig,
    seed: u64,
    led: EvidenceLedger,
) -> DifferentialCampaign<M> {
    DifferentialCampaign::new(
        machine,
        codec,
        Box::new(DeclineTactic::new()),
        Box::new(GenesisSelector::new()),
        Box::new(DefaultObservationCells::new()),
        led,
        coordinator(),
        cfg,
        seed,
    )
    .expect("new")
}

/// A standalone **seal** evidence batch for view-level tests: one `set`
/// register (`reg 1`) firing `value` at moment `at`, cut exactly there.
/// Returns the batch identity (digest of the canonical bytes) and the record.
pub(crate) fn seal_evidence(
    issue: u64,
    at: u64,
    value: u64,
) -> (EvidenceBatchId, CompletedRunEvidence) {
    let decl = sdk_events::encode_v2_declaration(&[DeclaredPoint {
        namespace: NS_STATE,
        local: 1,
        name: "r1".into(),
        classification: Classification::State,
        value_shape: Some(ValueShape::U64),
        base_op: Some(UpdateOp::Set),
        expectation: None,
    }])
    .expect("valid v2 declaration");
    let id = ((NS_STATE as u32) << 24) | 1;
    let mut firing = vec![0u8]; // op byte: Set
    firing.extend_from_slice(&value.to_le_bytes());
    let raw = vec![
        (sdk_events::Moment(0), CATALOG_EVENT_ID, decl),
        (sdk_events::Moment(at), id, firing),
    ];
    let n = sdk_events::decode_binary(&raw).expect("decodes");
    let ev = CompletedRunEvidence {
        rollout: RunId {
            issue,
            parent: Some(0),
        },
        role: EvidenceRole::Seal,
        terminal: StopReason::Quiescent { vtime: Moment(at) },
        env: Reproducer {
            blob_version: 1,
            bytes: issue.to_le_bytes().to_vec(),
        },
        cut: EvidenceCut {
            at: Moment(at),
            sdk_events: 1,
        },
        normalized: n,
        parent_cut: None,
        sealable_moments: Vec::new(),
    };
    let batch = EvidenceBatchId::digest(&ev.canonical_bytes());
    (batch, ev)
}

// ---------------------------------------------------------------------------
// The v1-verb test machine (task 155 / hm-5mx0)
// ---------------------------------------------------------------------------

/// One scripted SDK firing the [`VerbMachine`] emits at moment `at`.
#[derive(Clone)]
pub(crate) enum VerbFiring {
    /// A `set` on state register `reg` (unresolved base op on the v1 wire).
    State { reg: u32, value: u64 },
    /// An assertion firing at `NS_ASSERT` `local` with the given disposition
    /// ([`DISP_HIT`]/[`DISP_VIOLATION`]). Decodes with its declared verb's
    /// `AssertType`, so it reaches the occurrence/absence verdict folds.
    Assert { local: u32, disp: u8 },
}

/// One scripted SDK emission: a [`VerbFiring`] at moment `at`.
#[derive(Clone)]
pub(crate) struct VerbEmit {
    pub(crate) at: u64,
    pub(crate) firing: VerbFiring,
}

/// A run's script for a [`VerbMachine`]: its SDK emissions (in moment order) and
/// its terminal moment. An emission past `terminal` is only surfaced by a
/// marker-clamped run-forward seal, never by the open-loop rollout — the shape
/// that puts a firing in the advanced span `[rollout_terminal, seal_cut)`.
#[derive(Clone)]
pub(crate) struct VerbRun {
    pub(crate) emits: Vec<VerbEmit>,
    pub(crate) terminal: u64,
}

/// A pure, deterministic **v1-verb** machine: the [`ScriptedMachine`] shape
/// generalized so its SDK trajectory is arbitrary occurrence/assertion **and**
/// state firings, declared through a **v1 catalog** that carries each assertion's
/// verb ([`encode_v1_catalog`]). `ScriptedMachine` emits a v2 catalog (no verb →
/// `AssertType::None` → neither verdict fold fires) and state events only; this
/// machine closes that gap so a campaign step can drive an advanced-span
/// occurrence hit through the production `capture_seal_suffix` /
/// `decode_child_suffix` / [`DifferentialCampaign::step`] path (task 155 / hm-5mx0,
/// closing PR #155 F3c). `ScriptedMachine` is left untouched (its v2 shape is
/// load-bearing for the existing suites); this is a sibling built on its
/// Machine-trait surface.
///
/// Like `ScriptedMachine` it is **lineage-aware** (a snapshot captures the
/// emitted prefix; a `branch` restores it) and supports the marker-clamped
/// run-forward seal shape (tasks/136 + tasks/144): stage markers with
/// [`with_markers`](Self::with_markers) and pair a [`MarkerCodec`] reporting the
/// identical keys, and the seal advances to the drained marker, capturing any
/// firing in the advanced span.
pub(crate) struct VerbMachine {
    /// The fixed run script every `branch` replays (deterministic and
    /// seed-independent — the advanced span, not seed sensitivity, is under test).
    run: VerbRun,
    /// The declared v1 catalog points `(kind, local, name)` — constant across a
    /// campaign (every rollout and seal re-declares the identical schema).
    points: Vec<(u8, u32, String)>,
    seed: u64,
    emits: Vec<VerbEmit>,
    terminal: u64,
    cursor: usize,
    clock: u64,
    recorded: Reproducer,
    /// snap id -> (moment, included count, emitted prefix).
    snaps: BTreeMap<u64, (u64, u64, Vec<VerbEmit>)>,
    next_snap: u64,
    /// Staged-schedule `Moment`s relative to the branch origin (re-anchored at
    /// each `branch`, mirroring the adapter's blob-frame re-keying).
    markers: Vec<u64>,
    /// The absolute staged `Moment`s of the current branch (a `snapshot` while
    /// any lies ahead is refused `NotQuiescent`, the collapsed `SnapshotWhileArmed`).
    staged_abs: Vec<u64>,
}

impl VerbMachine {
    /// A machine declaring `points` (v1 catalog `(kind, local, name)`) whose
    /// every branch replays the fixed `run` script.
    pub(crate) fn new(points: Vec<(u8, u32, String)>, run: VerbRun) -> Self {
        Self {
            run,
            points,
            seed: 0,
            emits: Vec::new(),
            terminal: 0,
            cursor: 0,
            clock: 0,
            recorded: Reproducer {
                blob_version: 1,
                bytes: 0u64.to_le_bytes().to_vec(),
            },
            snaps: BTreeMap::new(),
            next_snap: 1,
            markers: Vec::new(),
            staged_abs: Vec::new(),
        }
    }

    /// Stage `markers` (relative to the branch origin) on every `branch` — pair
    /// with a [`MarkerCodec`] reporting the same keys (mirrors `ScriptedMachine`).
    pub(crate) fn with_markers(mut self, markers: Vec<u64>) -> Self {
        self.markers = markers;
        self
    }

    fn catalog(&self) -> Vec<u8> {
        let points: Vec<(u8, u32, &str)> = self
            .points
            .iter()
            .map(|(kind, local, name)| (*kind, *local, name.as_str()))
            .collect();
        encode_v1_catalog(&points)
    }

    fn seed_of(env: &Reproducer) -> u64 {
        ScriptedMachine::seed_of(env)
    }

    fn included_at(&self, moment: u64) -> u64 {
        self.emits.iter().filter(|e| e.at <= moment).count() as u64
    }
}

impl Machine for VerbMachine {
    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
        // Restore the snapshot's emitted prefix, then append the child's own
        // script at BRANCH-RELATIVE moments (the physical cut contract) — the
        // identical lineage logic as `ScriptedMachine`.
        let (moment, included, prefix) =
            self.snaps
                .get(&snap.0)
                .cloned()
                .unwrap_or((0, 0, Vec::new()));
        self.seed = Self::seed_of(env);
        let prog = self.run.clone();
        self.clock = moment;
        self.cursor = included as usize;
        self.emits = prefix;
        self.emits.extend(prog.emits.into_iter().map(|e| VerbEmit {
            at: moment + e.at,
            ..e
        }));
        self.terminal = moment + prog.terminal;
        self.recorded = env.clone();
        self.staged_abs = self.markers.iter().map(|&m| moment + m).collect();
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        let (moment, included, prefix) = self
            .snaps
            .get(&snap.0)
            .cloned()
            .ok_or(MachineError::UnknownSnapshot(snap.0))?;
        self.clock = moment;
        self.cursor = included as usize;
        self.emits = prefix;
        self.staged_abs.clear();
        Ok(())
    }

    fn run(
        &mut self,
        until: &StopConditions,
        _resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        // Byte-for-byte the `ScriptedMachine` run model (it advances on `at`
        // moments, blind to firing content): a deadline leg materializes a
        // replay to its sealable point; an open-loop rollout surfaces each
        // emit's sealable point up to the terminal, then terminates — an emit
        // past the terminal belongs only to a later marker-clamped run-forward.
        // That `at <= self.terminal` filter (the `None` arm below) is the
        // load-bearing fixture invariant the e2e gates pin via firing provenance
        // (PR158-F1): dropping it would leak the advanced-span firing into the
        // rollout evidence and vacate the gates' meaning.
        match until.deadline {
            Some(d) => {
                while self.cursor < self.emits.len() && self.emits[self.cursor].at < d.0 {
                    self.clock = self.emits[self.cursor].at;
                    self.cursor += 1;
                }
                if self.cursor < self.emits.len() && self.emits[self.cursor].at == d.0 {
                    self.clock = d.0;
                    self.cursor += 1;
                    Ok(StopReason::SnapshotPoint { vtime: Moment(d.0) })
                } else if d.0 < self.terminal {
                    self.clock = d.0;
                    Ok(StopReason::Deadline { vtime: Moment(d.0) })
                } else {
                    self.clock = self.terminal.max(d.0);
                    Ok(StopReason::Quiescent {
                        vtime: Moment(self.clock),
                    })
                }
            }
            None => {
                if self.cursor < self.emits.len() && self.emits[self.cursor].at <= self.terminal {
                    self.clock = self.emits[self.cursor].at;
                    self.cursor += 1;
                    Ok(StopReason::SnapshotPoint {
                        vtime: Moment(self.clock),
                    })
                } else {
                    self.clock = self.terminal;
                    Ok(StopReason::Quiescent {
                        vtime: Moment(self.terminal),
                    })
                }
            }
        }
    }

    fn snapshot(&mut self) -> Result<(SnapId, EvidenceCut), MachineError> {
        // A staged Moment still ahead refuses the seal (`SnapshotWhileArmed`,
        // collapsed at the adapter) — the marker-clamp discipline.
        if self.staged_abs.iter().any(|&m| m > self.clock) {
            return Err(MachineError::NotQuiescent);
        }
        let id = self.next_snap;
        self.next_snap += 1;
        let included = self.included_at(self.clock);
        let prefix: Vec<VerbEmit> = self.emits.iter().take(included as usize).cloned().collect();
        self.snaps.insert(id, (self.clock, included, prefix));
        // The catalog-inclusive stamp (`1 + included`) in the production frame,
        // exactly as `ScriptedMachine`: `sdk_events()` returns `[catalog] +
        // firings[..cursor]`, and `cursor == included` at a valid seal, so the
        // honest stamp equals the raw capture length (`capture_seal_suffix`'s
        // count invariant).
        Ok((
            SnapId(id),
            EvidenceCut {
                at: Moment(self.clock),
                sdk_events: 1 + included,
            },
        ))
    }

    fn drop_snap(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.snaps.remove(&snap.0);
        Ok(())
    }

    fn hash(&mut self) -> Result<[u8; 32], MachineError> {
        let mut h = [0u8; 32];
        h[..8].copy_from_slice(&self.seed.to_le_bytes());
        h[8..16].copy_from_slice(&self.clock.to_le_bytes());
        Ok(h)
    }

    fn coverage(&self) -> &[u8] {
        &[]
    }

    fn recorded_env(&self) -> Result<Reproducer, MachineError> {
        Ok(self.recorded.clone())
    }

    fn sdk_events(&mut self) -> Result<Vec<(u64, u32, Vec<u8>)>, MachineError> {
        // The v1 catalog then every firing emitted up to the cursor, encoded
        // through the shared [`assert_firing`]/[`state_firing`] fixtures.
        let mut out = vec![(0u64, CATALOG_EVENT_ID, self.catalog())];
        for e in self.emits.iter().take(self.cursor) {
            let (id, body) = match &e.firing {
                VerbFiring::State { reg, value } => state_firing(*reg, *value),
                VerbFiring::Assert { local, disp } => assert_firing(*local, *disp),
            };
            out.push((e.at, id, body));
        }
        Ok(out)
    }
}
