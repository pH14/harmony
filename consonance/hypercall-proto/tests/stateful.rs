// SPDX-License-Identifier: AGPL-3.0-or-later
//! Model-based (stateful) property test for [`hypercall_proto::Dispatcher`].
//!
//! `proptest-state-machine` generates a precondition-satisfying sequence of
//! operations — service registration, `dispatch` (well-formed and deliberately
//! malformed frames), `save_state`, and `restore_state` — and drives them against
//! both the real [`Dispatcher`] and an independent reference model.
//!
//! The reference re-implements, from scratch, the dispatcher's routing/framing
//! rules and each reference service's logical behavior. Two invariants are
//! asserted: after every `dispatch` the produced response frame must equal the
//! model-predicted frame byte-for-byte, and after every transition the
//! dispatcher's `save_state` blob must equal the model's. Because `restore_state`
//! rewinds the model in lockstep, a restore that silently diverged would be caught
//! by the next dispatch or the next save-blob comparison.
//!
//! Service ids map to fixed service types (Console=1, Entropy=2, Block=3,
//! Event=4); registration always (re)creates that id's canonical service, which
//! keeps `save_state`/`restore_state` registration-shape invariants clean.
//!
//! Ordered collections are used freely here: the determinism rules constrain
//! library code, not the test oracle.
#![cfg(feature = "host")]

use std::collections::{BTreeMap, BTreeSet};

use hypercall_proto::{
    ConsoleSink, Dispatcher, EventSink, MAX_PAYLOAD, MemBlockDevice, SeededEntropy, ServiceId,
};
use proptest::prelude::*;
use proptest::strategy::Union;
use proptest::test_runner::Config;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest, prop_state_machine};

// Wire/protocol constants mirrored from the library (private there).
const MAGIC: u32 = 0x3150_4348;
const HEADER_LEN: usize = 24;
const KIND_REQUEST: u16 = 1;
const KIND_RESPONSE: u16 = 2;
const SECTOR_SIZE: usize = 512;

// Status codes.
const ST_OK: u16 = 0;
const ST_BAD_REQUEST: u16 = 1;
const ST_UNKNOWN_SERVICE: u16 = 2;
const ST_UNKNOWN_OPCODE: u16 = 3;
const ST_OUT_OF_RANGE: u16 = 4;

// Entropy stream constants (mirrored from the library).
const ENTROPY_FALLBACK_SEED: u64 = 0x9E37_79B9_7F4A_7C15;
const ENTROPY_MUL: u64 = 0x2545_F491_4F6C_DD1D;

// Canonical service ids.
const ID_CONSOLE: u16 = 1;
const ID_ENTROPY: u16 = 2;
const ID_BLOCK: u16 = 3;
const ID_EVENT: u16 = 4;

fn normalize_seed(seed: u64) -> u64 {
    if seed == 0 {
        ENTROPY_FALLBACK_SEED
    } else {
        seed
    }
}

/// Deterministic block-device contents for a given size, mirrored by model and SUT.
fn block_data(sectors: u8) -> Vec<u8> {
    (0..sectors as usize * SECTOR_SIZE)
        .map(|i| i as u8)
        .collect()
}

// ---------------------------------------------------------------------------
// Reference service models
// ---------------------------------------------------------------------------

/// Independent reference model of one registered service.
#[derive(Clone, Debug)]
enum SvcModel {
    Console(Vec<u8>),
    Entropy(u64),
    Block(Vec<u8>),
    Event(Vec<(u32, Vec<u8>)>),
}

fn rd_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
fn rd_u64(b: &[u8], off: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(a)
}

impl SvcModel {
    /// Predict `(status, response_payload)` for one request, mutating model state
    /// exactly as the reference service would.
    fn handle(&mut self, opcode: u16, payload: &[u8]) -> (u16, Vec<u8>) {
        match self {
            SvcModel::Console(bytes) => {
                if opcode != 1 {
                    return (ST_UNKNOWN_OPCODE, Vec::new());
                }
                bytes.extend_from_slice(payload);
                (ST_OK, Vec::new())
            }
            SvcModel::Entropy(state) => {
                if opcode != 1 {
                    return (ST_UNKNOWN_OPCODE, Vec::new());
                }
                if payload.len() != 4 {
                    return (ST_BAD_REQUEST, Vec::new());
                }
                let n = rd_u32(payload, 0) as usize;
                if !(1..=MAX_PAYLOAD).contains(&n) {
                    return (ST_BAD_REQUEST, Vec::new());
                }
                let mut out = vec![0u8; n];
                let mut offset = 0;
                while offset < n {
                    *state ^= *state >> 12;
                    *state ^= *state << 25;
                    *state ^= *state >> 27;
                    let word = state.wrapping_mul(ENTROPY_MUL).to_le_bytes();
                    let take = core::cmp::min(8, n - offset);
                    out[offset..offset + take].copy_from_slice(&word[..take]);
                    offset += take;
                }
                (ST_OK, out)
            }
            SvcModel::Block(data) => match opcode {
                1 => {
                    if !payload.is_empty() {
                        return (ST_BAD_REQUEST, Vec::new());
                    }
                    let count = (data.len() / SECTOR_SIZE) as u64;
                    (ST_OK, count.to_le_bytes().to_vec())
                }
                2 => {
                    if payload.len() != 12 {
                        return (ST_BAD_REQUEST, Vec::new());
                    }
                    let lba = rd_u64(payload, 0);
                    let sectors = rd_u32(payload, 8);
                    if !(1..=7).contains(&sectors) {
                        return (ST_BAD_REQUEST, Vec::new());
                    }
                    let Ok(start_sector) = usize::try_from(lba) else {
                        return (ST_OUT_OF_RANGE, Vec::new());
                    };
                    let Some(start) = start_sector.checked_mul(SECTOR_SIZE) else {
                        return (ST_OUT_OF_RANGE, Vec::new());
                    };
                    let len = sectors as usize * SECTOR_SIZE;
                    let Some(end) = start.checked_add(len) else {
                        return (ST_OUT_OF_RANGE, Vec::new());
                    };
                    if end > data.len() {
                        return (ST_OUT_OF_RANGE, Vec::new());
                    }
                    (ST_OK, data[start..end].to_vec())
                }
                _ => (ST_UNKNOWN_OPCODE, Vec::new()),
            },
            SvcModel::Event(events) => {
                if opcode != 1 {
                    return (ST_UNKNOWN_OPCODE, Vec::new());
                }
                if payload.len() < 4 {
                    return (ST_BAD_REQUEST, Vec::new());
                }
                let id = rd_u32(payload, 0);
                events.push((id, payload[4..].to_vec()));
                (ST_OK, Vec::new())
            }
        }
    }

    fn save(&self) -> Vec<u8> {
        match self {
            SvcModel::Console(bytes) => bytes.clone(),
            SvcModel::Entropy(state) => state.to_le_bytes().to_vec(),
            SvcModel::Block(_) => Vec::new(),
            SvcModel::Event(events) => {
                let mut out = Vec::new();
                out.extend_from_slice(&(events.len() as u32).to_le_bytes());
                for (id, data) in events {
                    out.extend_from_slice(&id.to_le_bytes());
                    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
                    out.extend_from_slice(data);
                }
                out
            }
        }
    }

    /// Restore exactly what `save` captured. Block state is not serialized, so its
    /// restore is a no-op — mirroring the library. We only ever feed blobs the
    /// matching service produced, so parsing is infallible here.
    fn restore(&mut self, state: &[u8]) {
        match self {
            SvcModel::Console(bytes) => {
                bytes.clear();
                bytes.extend_from_slice(state);
            }
            SvcModel::Entropy(s) => *s = rd_u64(state, 0),
            SvcModel::Block(_) => {}
            SvcModel::Event(events) => {
                let count = rd_u32(state, 0) as usize;
                let mut offset = 4;
                let mut out = Vec::new();
                for _ in 0..count {
                    let id = rd_u32(state, offset);
                    offset += 4;
                    let len = rd_u32(state, offset) as usize;
                    offset += 4;
                    out.push((id, state[offset..offset + len].to_vec()));
                    offset += len;
                }
                *events = out;
            }
        }
    }
}

/// The dispatcher's `save_state` format over the registered (ascending-id) set.
fn model_save(registered: &BTreeMap<u16, SvcModel>) -> Vec<u8> {
    let mut out = Vec::new();
    for (id, svc) in registered {
        let state = svc.save();
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&(state.len() as u32).to_le_bytes());
        out.extend_from_slice(&state);
    }
    out
}

/// Mirror of `Dispatcher::restore_state` over a blob this set produced.
fn model_restore(registered: &mut BTreeMap<u16, SvcModel>, blob: &[u8]) {
    let mut offset = 0;
    for svc in registered.values_mut() {
        offset += 2; // id (already known to match)
        let len = rd_u32(blob, offset) as usize;
        offset += 4;
        svc.restore(&blob[offset..offset + len]);
        offset += len;
    }
}

// ---------------------------------------------------------------------------
// Frame helpers
// ---------------------------------------------------------------------------

fn make_frame(
    magic: u32,
    kind: u16,
    service: u16,
    opcode: u16,
    status: u16,
    seq: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
    buf.extend_from_slice(&magic.to_le_bytes());
    buf.extend_from_slice(&kind.to_le_bytes());
    buf.extend_from_slice(&service.to_le_bytes());
    buf.extend_from_slice(&opcode.to_le_bytes());
    buf.extend_from_slice(&status.to_le_bytes());
    buf.extend_from_slice(&seq.to_le_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // reserved
    buf.extend_from_slice(payload);
    buf
}

fn make_response(service: u16, opcode: u16, seq: u32, status: u16, payload: &[u8]) -> Vec<u8> {
    make_frame(MAGIC, KIND_RESPONSE, service, opcode, status, seq, payload)
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Malform {
    None,
    BadMagic,
    WrongKind,
}

#[derive(Clone, Debug)]
enum Transition {
    /// (Re)register a service. `which` picks the canonical service type.
    Register {
        which: u8,
        seed: u64,
        sectors: u8,
    },
    Dispatch {
        service: u16,
        opcode: u16,
        seq: u32,
        payload: Vec<u8>,
        malform: Malform,
    },
    Save,
    Restore {
        which: usize,
    },
}

#[derive(Clone, Debug)]
struct RefState {
    registered: BTreeMap<u16, SvcModel>,
    /// (registration id-set, save blob) for each `Save`, by index.
    saves: Vec<(BTreeSet<u16>, Vec<u8>)>,
    /// Frame the last `Dispatch` is predicted to produce.
    last_response: Vec<u8>,
}

impl RefState {
    fn id_set(&self) -> BTreeSet<u16> {
        self.registered.keys().copied().collect()
    }
}

fn register_parts(which: u8, seed: u64, sectors: u8) -> (u16, SvcModel) {
    match which % 4 {
        0 => (ID_CONSOLE, SvcModel::Console(Vec::new())),
        1 => (ID_ENTROPY, SvcModel::Entropy(normalize_seed(seed))),
        2 => (ID_BLOCK, SvcModel::Block(block_data(sectors))),
        _ => (ID_EVENT, SvcModel::Event(Vec::new())),
    }
}

struct ProtoRef;

impl ReferenceStateMachine for ProtoRef {
    type State = RefState;
    type Transition = Transition;

    fn init_state() -> BoxedStrategy<RefState> {
        Just(RefState {
            registered: BTreeMap::new(),
            saves: Vec::new(),
            last_response: Vec::new(),
        })
        .boxed()
    }

    fn transitions(state: &RefState) -> BoxedStrategy<Transition> {
        let register = (0u8..4, any::<u64>(), 0u8..=4)
            .prop_map(|(which, seed, sectors)| Transition::Register {
                which,
                seed,
                sectors,
            })
            .boxed();

        let disp = |service: BoxedStrategy<u16>,
                    opcode: BoxedStrategy<u16>,
                    payload: BoxedStrategy<Vec<u8>>,
                    malform: BoxedStrategy<Malform>| {
            (service, opcode, any::<u32>(), payload, malform)
                .prop_map(
                    |(service, opcode, seq, payload, malform)| Transition::Dispatch {
                        service,
                        opcode,
                        seq,
                        payload,
                        malform,
                    },
                )
                .boxed()
        };

        let well_formed = prop_oneof![
            // console write
            disp(
                Just(ID_CONSOLE).boxed(),
                Just(1u16).boxed(),
                prop::collection::vec(any::<u8>(), 0..40).boxed(),
                Just(Malform::None).boxed()
            ),
            // entropy read (valid n)
            disp(
                Just(ID_ENTROPY).boxed(),
                Just(1u16).boxed(),
                (1u32..=64).prop_map(|n| n.to_le_bytes().to_vec()).boxed(),
                Just(Malform::None).boxed()
            ),
            // block capacity
            disp(
                Just(ID_BLOCK).boxed(),
                Just(1u16).boxed(),
                Just(Vec::new()).boxed(),
                Just(Malform::None).boxed()
            ),
            // block read
            disp(
                Just(ID_BLOCK).boxed(),
                Just(2u16).boxed(),
                (0u64..6, 1u32..=7)
                    .prop_map(|(lba, sectors)| {
                        let mut p = Vec::with_capacity(12);
                        p.extend_from_slice(&lba.to_le_bytes());
                        p.extend_from_slice(&sectors.to_le_bytes());
                        p
                    })
                    .boxed(),
                Just(Malform::None).boxed()
            ),
            // event emit
            disp(
                Just(ID_EVENT).boxed(),
                Just(1u16).boxed(),
                prop::collection::vec(any::<u8>(), 4..24).boxed(),
                Just(Malform::None).boxed()
            ),
        ];

        let fuzz = disp(
            prop::sample::select(vec![0u16, 1, 2, 3, 4, 5]).boxed(),
            (0u16..4).boxed(),
            prop::collection::vec(any::<u8>(), 0..16).boxed(),
            prop_oneof![
                3 => Just(Malform::None),
                1 => Just(Malform::BadMagic),
                1 => Just(Malform::WrongKind),
            ]
            .boxed(),
        );

        let dispatch = prop_oneof![3 => well_formed, 1 => fuzz].boxed();

        let mut choices: Vec<(u32, BoxedStrategy<Transition>)> = vec![
            (3, register),
            (5, dispatch),
            (2, Just(Transition::Save).boxed()),
        ];

        let cur = state.id_set();
        let valid_restores: Vec<usize> = state
            .saves
            .iter()
            .enumerate()
            .filter(|(_, (set, _))| *set == cur)
            .map(|(i, _)| i)
            .collect();
        if !valid_restores.is_empty() {
            choices.push((
                2,
                prop::sample::select(valid_restores)
                    .prop_map(|which| Transition::Restore { which })
                    .boxed(),
            ));
        }

        Union::new_weighted(choices).boxed()
    }

    fn apply(mut state: RefState, transition: &Transition) -> RefState {
        match transition {
            Transition::Register {
                which,
                seed,
                sectors,
            } => {
                let (id, svc) = register_parts(*which, *seed, *sectors);
                state.registered.insert(id, svc);
            }
            Transition::Dispatch {
                service,
                opcode,
                seq,
                payload,
                malform,
            } => {
                state.last_response = match malform {
                    Malform::BadMagic => make_response(0, 0, 0, ST_BAD_REQUEST, &[]),
                    Malform::WrongKind => {
                        make_response(*service, *opcode, *seq, ST_BAD_REQUEST, &[])
                    }
                    Malform::None => {
                        if let Some(svc) = state.registered.get_mut(service) {
                            let (status, resp) = svc.handle(*opcode, payload);
                            make_response(*service, *opcode, *seq, status, &resp)
                        } else {
                            make_response(*service, *opcode, *seq, ST_UNKNOWN_SERVICE, &[])
                        }
                    }
                };
            }
            Transition::Save => {
                let set = state.id_set();
                let blob = model_save(&state.registered);
                state.saves.push((set, blob));
            }
            Transition::Restore { which } => {
                let blob = state.saves[*which].1.clone();
                model_restore(&mut state.registered, &blob);
            }
        }
        state
    }

    fn preconditions(state: &RefState, transition: &Transition) -> bool {
        match transition {
            Transition::Restore { which } => state
                .saves
                .get(*which)
                .is_some_and(|(set, _)| *set == state.id_set()),
            _ => true,
        }
    }
}

struct ProtoSut {
    dispatcher: Dispatcher,
    /// Save blobs by index, aligned with the model's `saves`.
    blobs: Vec<Vec<u8>>,
}

struct ProtoMachine;

impl StateMachineTest for ProtoMachine {
    type SystemUnderTest = ProtoSut;
    type Reference = ProtoRef;

    fn init_test(_ref_state: &RefState) -> ProtoSut {
        ProtoSut {
            dispatcher: Dispatcher::new(),
            blobs: Vec::new(),
        }
    }

    fn apply(mut sut: ProtoSut, ref_state: &RefState, transition: Transition) -> ProtoSut {
        match transition {
            Transition::Register {
                which,
                seed,
                sectors,
            } => match which % 4 {
                0 => sut
                    .dispatcher
                    .register(ServiceId::Console, Box::new(ConsoleSink::new())),
                1 => sut
                    .dispatcher
                    .register(ServiceId::Entropy, Box::new(SeededEntropy::new(seed))),
                2 => sut.dispatcher.register(
                    ServiceId::Block,
                    Box::new(MemBlockDevice::new(block_data(sectors)).unwrap()),
                ),
                _ => sut
                    .dispatcher
                    .register(ServiceId::Event, Box::new(EventSink::new())),
            },
            Transition::Dispatch {
                service,
                opcode,
                seq,
                payload,
                malform,
            } => {
                let (magic, kind) = match malform {
                    Malform::None => (MAGIC, KIND_REQUEST),
                    Malform::BadMagic => (0, KIND_REQUEST),
                    Malform::WrongKind => (MAGIC, KIND_RESPONSE),
                };
                let req = make_frame(magic, kind, service, opcode, 0, seq, &payload);
                let mut resp = vec![0u8; 4096];
                let len = sut.dispatcher.dispatch(&req, &mut resp);
                assert_eq!(
                    &resp[..len],
                    &ref_state.last_response[..],
                    "dispatch frame diverged (service {service} opcode {opcode} malform {malform:?})"
                );
            }
            Transition::Save => {
                let blob = sut.dispatcher.save_state();
                assert_eq!(
                    blob,
                    ref_state.saves.last().unwrap().1,
                    "save_state blob diverged"
                );
                sut.blobs.push(blob);
            }
            Transition::Restore { which } => {
                sut.dispatcher
                    .restore_state(&sut.blobs[which])
                    .expect("restore of a matching-shape blob must succeed");
            }
        }
        sut
    }

    fn check_invariants(sut: &ProtoSut, ref_state: &RefState) {
        // The strongest standing invariant: the dispatcher's serialized state must
        // equal the model's after every transition.
        assert_eq!(
            sut.dispatcher.save_state(),
            model_save(&ref_state.registered),
            "save_state blob diverged from model"
        );
    }
}

prop_state_machine! {
    #![proptest_config(Config { cases: 256, ..Config::default() })]

    /// Drive 1..40 operations against the dispatcher and the reference model.
    #[test]
    fn dispatcher_matches_model(sequential 1..40 => ProtoMachine);
}
