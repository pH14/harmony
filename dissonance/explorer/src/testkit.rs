// SPDX-License-Identifier: AGPL-3.0-or-later
//! Crate-internal test kit for the Differential campaign tests: a pure,
//! deterministic scripted [`Machine`] that emits reducible SDK state, a trivial
//! reproducer codec, and the campaign/ledger/coordinator builders shared by the
//! `campaign` and `retention` test modules.

use std::collections::BTreeMap;
use std::rc::Rc;

use revision_coordinator::{CampaignConfigId, Coordinator, EvidenceBatchId, MemLedger};
use sdk_events::{Classification, DeclaredPoint, NS_STATE, UpdateOp, ValueShape};

use crate::campaign::{CATALOG_EVENT_ID, CampaignConfig, DifferentialCampaign, Ingress};
use crate::defaults::{DeclineTactic, GenesisSelector};
use crate::error::MachineError;
use crate::evidence::{CompletedRunEvidence, DefaultObservationCells, EvidenceRole, RunId};
use crate::ledger::EvidenceLedger;
use crate::retention::RetentionProfile;
use crate::seam::{EnvCodec, Machine};
use crate::spine::{EvidenceCut, Moment};
use crate::{Answer, Reproducer, SnapId, StopConditions, StopReason};

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
        }
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
        Ok(())
    }

    fn run(
        &mut self,
        until: &StopConditions,
        _resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
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
                } else {
                    self.clock = self.terminal.max(d.0);
                    Ok(StopReason::Quiescent {
                        vtime: Moment(self.clock),
                    })
                }
            }
            // Rollout: surface each emission's sealable point, then terminate.
            None => {
                if self.cursor < self.emits.len() {
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
        let id = self.next_snap;
        self.next_snap += 1;
        let included = self.included_at(self.clock);
        let prefix: Vec<Emit> = self.emits.iter().take(included as usize).cloned().collect();
        self.snaps.insert(id, (self.clock, included, prefix));
        Ok((
            SnapId(id),
            EvidenceCut {
                at: Moment(self.clock),
                sdk_events: included,
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
    DifferentialCampaign::new(
        machine,
        Box::new(ToyCodec),
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
