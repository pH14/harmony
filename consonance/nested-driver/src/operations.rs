// SPDX-License-Identifier: AGPL-3.0-or-later

use control_proto::{Reply, Reproducer, Request, SnapId};
use environment::{channel::Effect, input_spec::InputSpec};
use std::fmt;
use vmm_backend::{Backend, X86};
use vmm_core::{
    control::{ControlServer, RestoreMode, server_caps},
    vendor::x86::contract_vclock_config,
    vmm::{Vmm, VmmError, VtimeWiring},
};

pub const ENTROPY_BYTES: usize = 28;
pub const SNAPSHOT_LIMIT: usize = 8;
pub const ORACLE_ASSERTION: u32 = 1;
pub const RESTORE_ASSERTION: u32 = 2;
pub const OPERATION_ASSERTION: u32 = 3;
pub const REPLAY_ASSERTION: u32 = 4;
pub const PAIR_BASE: u32 = 100;
pub const LONG_PROGRAM: &[u8] = &[
    0x33, 0x1e, 0x00, 0x70, 0x43, 0x01, 0x1e, 0x00, 0x30, 0x31, 0x1e, 0x00, 0x40, 0x03, 0x36, 0x00,
    0x50, 0x89, 0x36, 0x00, 0x50, 0x33, 0x3e, 0x00, 0x60, 0x01, 0xdf, 0x01, 0x3e, 0x00, 0x60, 0x83,
    0x06, 0x00, 0x80, 0x01, 0x03, 0x36, 0x00, 0x80, 0x31, 0x36, 0x00, 0x50, 0xba, 0xf8, 0x03, 0xb0,
    0x5a, 0xee, 0xeb, 0xcc,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Operation {
    Run,
    Snapshot,
    Restore,
    Fork,
    Drop,
    ExportImport,
}

impl Operation {
    pub const ALL: [Self; 6] = [
        Self::Run,
        Self::Snapshot,
        Self::Restore,
        Self::Fork,
        Self::Drop,
        Self::ExportImport,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Snapshot => "snapshot",
            Self::Restore => "restore",
            Self::Fork => "fork",
            Self::Drop => "drop",
            Self::ExportImport => "export_import",
        }
    }

    pub fn pair(self, next: Self) -> u32 {
        PAIR_BASE + self as u32 * 6 + next as u32
    }
}

const PAIR_NAMES: [[&str; 6]; 6] = [
    [
        "nested.pair.run.run",
        "nested.pair.run.snapshot",
        "nested.pair.run.restore",
        "nested.pair.run.fork",
        "nested.pair.run.drop",
        "nested.pair.run.export_import",
    ],
    [
        "nested.pair.snapshot.run",
        "nested.pair.snapshot.snapshot",
        "nested.pair.snapshot.restore",
        "nested.pair.snapshot.fork",
        "nested.pair.snapshot.drop",
        "nested.pair.snapshot.export_import",
    ],
    [
        "nested.pair.restore.run",
        "nested.pair.restore.snapshot",
        "nested.pair.restore.restore",
        "nested.pair.restore.fork",
        "nested.pair.restore.drop",
        "nested.pair.restore.export_import",
    ],
    [
        "nested.pair.fork.run",
        "nested.pair.fork.snapshot",
        "nested.pair.fork.restore",
        "nested.pair.fork.fork",
        "nested.pair.fork.drop",
        "nested.pair.fork.export_import",
    ],
    [
        "nested.pair.drop.run",
        "nested.pair.drop.snapshot",
        "nested.pair.drop.restore",
        "nested.pair.drop.fork",
        "nested.pair.drop.drop",
        "nested.pair.drop.export_import",
    ],
    [
        "nested.pair.export_import.run",
        "nested.pair.export_import.snapshot",
        "nested.pair.export_import.restore",
        "nested.pair.export_import.fork",
        "nested.pair.export_import.drop",
        "nested.pair.export_import.export_import",
    ],
];

pub fn catalog() -> Vec<harmony_sdk::Point> {
    use harmony_sdk::Point;
    let mut catalog = vec![
        Point::always(ORACLE_ASSERTION, "nested.l2.oracle"),
        Point::always(RESTORE_ASSERTION, "nested.restore.readback"),
        Point::always(OPERATION_ASSERTION, "nested.operation.success"),
        Point::always(REPLAY_ASSERTION, "nested.replay.output"),
        Point::state(1, "nested.creations"),
        Point::state(2, "nested.imports"),
        Point::state(3, "nested.steps"),
        Point::state(4, "nested.bytes.0"),
        Point::state(5, "nested.bytes.1"),
        Point::state(6, "nested.bytes.2"),
        Point::state(7, "nested.snapshots"),
        Point::state(8, "nested.fork_depth"),
        Point::state(9, "nested.operations"),
        Point::state(10, "nested.operation"),
        Point::state(11, "nested.pairs"),
    ];
    for previous in Operation::ALL {
        for next in Operation::ALL {
            catalog.push(Point::sometimes(
                previous.pair(next),
                PAIR_NAMES[previous as usize][next as usize],
            ));
        }
    }
    catalog
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Choice {
    pub operation: Operation,
    pub snapshot: usize,
    pub inputs: Vec<u16>,
    pub branch_seed: u64,
}

pub fn choose(bytes: &[u8; ENTROPY_BYTES], live: usize) -> Result<Choice, Failure> {
    if !(1..=SNAPSHOT_LIMIT).contains(&live) {
        return Err(Failure::new(
            OPERATION_ASSERTION,
            "invalid live snapshot count",
        ));
    }
    let selected = Operation::ALL[usize::from(bytes[0]) % Operation::ALL.len()];
    let operation = match (selected, live) {
        (Operation::Drop, 1) => Operation::Snapshot,
        (Operation::Snapshot, SNAPSHOT_LIMIT) => Operation::Drop,
        _ => selected,
    };
    let steps = usize::from(bytes[2] % 8) + 1;
    Ok(Choice {
        operation,
        snapshot: usize::from(bytes[1]) % live,
        inputs: bytes[4..]
            .chunks_exact(2)
            .take(steps)
            .map(|word| u16::from_le_bytes(word.try_into().unwrap()))
            .collect(),
        branch_seed: u64::from_le_bytes(bytes[20..28].try_into().unwrap()),
    })
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LongOracle {
    pub base: crate::Oracle,
    pub rounds: u16,
    pub input: u16,
}

impl LongOracle {
    pub fn advance(&mut self, input: u16) {
        self.input = input;
        self.base.registers[0] ^= input;
        self.base.advance();
        self.rounds = self.rounds.wrapping_add(1);
        self.base.registers[1] = self.base.registers[1].wrapping_add(self.rounds);
        self.base.memory[2] ^= self.base.registers[1];
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut bytes = self.base.bytes();
        bytes.extend_from_slice(&self.rounds.to_le_bytes());
        bytes.extend_from_slice(&self.input.to_le_bytes());
        bytes
    }
}

pub fn compose_long<B: Backend<A = X86>>(backend: B) -> Result<Vmm<B>, VmmError> {
    let mut vmm = crate::compose_program(backend, LONG_PROGRAM)?;
    vmm.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7)?);
    vmm.wire_snapshot_hashing();
    Ok(vmm)
}

fn observe<B: Backend<A = X86>>(vmm: &mut Vmm<B>) -> Result<LongOracle, VmmError> {
    let base = crate::observe(vmm)?;
    let memory = vmm.guest_memory();
    Ok(LongOracle {
        base,
        rounds: u16::from_le_bytes(memory[0x8000..0x8002].try_into().unwrap()),
        input: u16::from_le_bytes(memory[0x7000..0x7002].try_into().unwrap()),
    })
}

#[derive(Debug)]
pub struct Failure {
    pub assertion: u32,
    pub detail: String,
}

impl Failure {
    fn new(assertion: u32, detail: impl Into<String>) -> Self {
        Self {
            assertion,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "inner assertion {}: {}", self.assertion, self.detail)
    }
}

impl std::error::Error for Failure {}

fn checked<T>(result: Result<T, impl fmt::Display>) -> Result<T, Failure> {
    result.map_err(|error| Failure::new(OPERATION_ASSERTION, error.to_string()))
}

#[derive(Clone, Copy)]
struct Cut {
    id: SnapId,
    oracle: LongOracle,
    depth: u64,
    steps: u64,
}

pub struct Engine<B: Backend<A = X86>> {
    server: ControlServer<B>,
    snapshots: Vec<Cut>,
    pub oracle: LongOracle,
    pub depth: u64,
    pub steps: u64,
    pub imports: u64,
}

impl<B: Backend<A = X86>> Engine<B> {
    pub fn new(vmm: Vmm<B>) -> Result<Self, Failure> {
        let mut server = ControlServer::new(
            vmm,
            Box::new(|| {
                Err(VmmError::ContractViolation(
                    "inner restore requested VM recreation".into(),
                ))
            }),
        );
        server.set_restore_mode(RestoreMode::InPlace);
        checked(checked(server.handle(&Request::Hello(server_caps())))?)?;
        let mut engine = Self {
            server,
            snapshots: Vec::new(),
            oracle: LongOracle::default(),
            depth: 0,
            steps: 0,
            imports: 0,
        };
        let root = engine.capture()?;
        engine.snapshots.push(root);
        Ok(engine)
    }

    pub fn live_snapshots(&self) -> usize {
        self.snapshots.len()
    }

    fn request(&mut self, request: Request) -> Result<Reply, Failure> {
        checked(checked(self.server.handle(&request))?)
    }

    fn vmm(&mut self) -> Result<&mut Vmm<B>, Failure> {
        self.server
            .vmm_mut()
            .ok_or_else(|| Failure::new(OPERATION_ASSERTION, "missing inner VMM"))
    }

    fn capture(&mut self) -> Result<Cut, Failure> {
        let actual = checked(observe(self.vmm()?))?;
        if actual != self.oracle {
            return Err(Failure::new(
                ORACLE_ASSERTION,
                format!("capture {actual:?} != {:?}", self.oracle),
            ));
        }
        let id = match self.request(Request::Snapshot)? {
            Reply::Snapshot { id, .. } => id,
            other => {
                return Err(Failure::new(
                    OPERATION_ASSERTION,
                    format!("snapshot reply {other:?}"),
                ));
            }
        };
        Ok(Cut {
            id,
            oracle: self.oracle,
            depth: self.depth,
            steps: self.steps,
        })
    }

    fn check_restored(&mut self, cut: Cut) -> Result<(), Failure> {
        let actual = checked(observe(self.vmm()?))?;
        if actual != cut.oracle {
            return Err(Failure::new(
                RESTORE_ASSERTION,
                format!("restored {actual:?} != {:?}", cut.oracle),
            ));
        }
        let mut expected = cut.oracle;
        expected.advance(expected.input);
        checked(crate::step(self.vmm()?))?;
        let actual = checked(observe(self.vmm()?))?;
        if actual != expected {
            return Err(Failure::new(
                RESTORE_ASSERTION,
                format!("L2 readback {actual:?} != {expected:?}"),
            ));
        }
        self.request(Request::Replay(cut.id))?;
        let actual = checked(observe(self.vmm()?))?;
        if actual != cut.oracle {
            return Err(Failure::new(
                RESTORE_ASSERTION,
                format!("restored after L2 readback {actual:?} != {:?}", cut.oracle),
            ));
        }
        self.oracle = cut.oracle;
        self.depth = cut.depth;
        self.steps = cut.steps;
        Ok(())
    }

    fn restore(&mut self, cut: Cut) -> Result<(), Failure> {
        self.request(Request::Replay(cut.id))?;
        self.check_restored(cut)
    }

    fn run_inputs(&mut self, inputs: &[u16], check: bool) -> Result<Vec<LongOracle>, Failure> {
        let mut outputs = Vec::new();
        for &input in inputs {
            let vmm = self.vmm()?;
            checked(vmm.apply_effect(&Effect::WriteMemory {
                gpa: 0x7000,
                bytes: input.to_le_bytes().to_vec(),
            }))?;
            checked(crate::step(vmm))?;
            self.oracle.advance(input);
            let actual = checked(observe(self.vmm()?))?;
            if check && actual != self.oracle {
                return Err(Failure::new(
                    ORACLE_ASSERTION,
                    format!("L2 output {actual:?} != {:?}", self.oracle),
                ));
            }
            self.steps += 1;
            outputs.push(actual);
        }
        Ok(outputs)
    }

    fn retain(&mut self, cut: Cut, victim: usize) -> Result<(), Failure> {
        if self.snapshots.len() == SNAPSHOT_LIMIT {
            let old = self.snapshots.remove(victim);
            self.request(Request::Drop(old.id))?;
        }
        self.snapshots.push(cut);
        Ok(())
    }

    pub fn execute(&mut self, choice: &Choice) -> Result<(), Failure> {
        let cut = *self
            .snapshots
            .get(choice.snapshot)
            .ok_or_else(|| Failure::new(OPERATION_ASSERTION, "unknown selected snapshot"))?;
        match choice.operation {
            Operation::Run => {
                let start = self.capture()?;
                let first = self.run_inputs(&choice.inputs, true)?;
                self.restore(start)?;
                let second = self.run_inputs(&choice.inputs, false)?;
                if first != second {
                    return Err(Failure::new(
                        REPLAY_ASSERTION,
                        format!("replay {second:?} != {first:?}"),
                    ));
                }
                self.request(Request::Drop(start.id))?;
            }
            Operation::Snapshot => {
                let current = self.capture()?;
                self.retain(current, choice.snapshot)?;
            }
            Operation::Restore => self.restore(cut)?,
            Operation::Fork => {
                self.request(Request::Branch {
                    snap: cut.id,
                    env: Reproducer {
                        blob_version: InputSpec::BLOB_VERSION,
                        bytes: InputSpec::seeded(choice.branch_seed).encode(),
                    },
                })?;
                self.check_restored(cut)?;
                self.request(Request::Branch {
                    snap: cut.id,
                    env: Reproducer {
                        blob_version: InputSpec::BLOB_VERSION,
                        bytes: InputSpec::seeded(choice.branch_seed).encode(),
                    },
                })?;
                self.depth = cut.depth + 1;
                let child = self.capture()?;
                self.retain(child, choice.snapshot)?;
            }
            Operation::Drop => {
                if self.snapshots.len() == 1 {
                    return Err(Failure::new(
                        OPERATION_ASSERTION,
                        "cannot drop the last snapshot",
                    ));
                }
                self.snapshots.remove(choice.snapshot);
                self.request(Request::Drop(cut.id))?;
            }
            Operation::ExportImport => {
                let mut bytes = Vec::new();
                checked(self.server.export_portable_snapshot(cut.id, &mut bytes))?;
                let imported = checked(self.server.import_portable_snapshot(bytes.as_slice()))?;
                self.imports += 1;
                let imported = Cut {
                    id: imported.id,
                    ..cut
                };
                self.restore(imported)?;
                self.retain(imported, choice.snapshot)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_choice_is_total_bounded_and_covers_every_valid_operation() {
        let mut reached = [false; 6];
        for live in 1..=SNAPSHOT_LIMIT {
            for byte in 0..=255 {
                let bytes = [byte; ENTROPY_BYTES];
                let choice = choose(&bytes, live).unwrap();
                assert_eq!(choice, choose(&bytes, live).unwrap());
                assert!(choice.snapshot < live);
                assert!((1..=8).contains(&choice.inputs.len()));
                assert!(!(live == 1 && choice.operation == Operation::Drop));
                assert!(!(live == SNAPSHOT_LIMIT && choice.operation == Operation::Snapshot));
                reached[choice.operation as usize] = true;
            }
        }
        assert_eq!(reached, [true; 6]);
        let mut bytes = [0; ENTROPY_BYTES];
        bytes[20..28].copy_from_slice(&7u64.to_le_bytes());
        let choice = choose(&bytes, 1).unwrap();
        assert_eq!(choice.branch_seed, 7);
        assert!(choice.inputs.iter().all(|&input| input == 0));
        assert!(choose(&[0; ENTROPY_BYTES], 0).is_err());
        assert!(choose(&[0; ENTROPY_BYTES], SNAPSHOT_LIMIT + 1).is_err());
    }

    #[test]
    fn ordered_operation_pairs_have_thirty_six_distinct_coordinates() {
        let points: std::collections::BTreeSet<_> = Operation::ALL
            .into_iter()
            .flat_map(|previous| Operation::ALL.map(|next| previous.pair(next)))
            .collect();
        assert_eq!(points, (PAIR_BASE..PAIR_BASE + 36).collect());
    }

    #[test]
    fn longer_guest_reads_previous_words_registers_counter_and_input() {
        let mut oracle = LongOracle::default();
        oracle.advance(0x55);
        assert_eq!(oracle.base.registers, [0x1262, 0x5679, 0x8274]);
        assert_eq!(oracle.base.memory, [0x2373, 0x3040, 1, 0xc6b8]);
        assert_eq!(oracle.rounds, 1);
        assert_eq!(oracle.input, 0x55);
        let mut changed = LongOracle {
            rounds: 1,
            ..Default::default()
        };
        changed.advance(0x55);
        assert_ne!(changed, oracle);
        let mut changed = LongOracle::default();
        changed.base.memory[3] ^= 1;
        changed.advance(0x55);
        assert_ne!(changed, oracle);
        let mut changed = LongOracle::default();
        changed.advance(0x54);
        assert_ne!(changed, oracle);
    }
}
