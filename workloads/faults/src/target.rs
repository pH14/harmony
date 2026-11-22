// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU16;

use control_proto::StopReason;
use fault_policy::{DecisionClass, Fault, ParkTarget, Span, StandingWindow, process_target};
use process_proto::EVERY_NODE;
use process_proto::registers as reg;
use searcher::search::assertion::{Accumulator, Observed};
use searcher::target::ExitKind;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

use crate::assertion::{AssertionKind, AssertionOutcome, Assertions, decode_json_event};
use crate::bundle::FaultVocabulary;
use crate::guest::GuestChoices;

pub const SUPERVISOR_TICK_NANOS: u64 = 10_000_000;
pub const SUPERVISOR_TICK_MICROS: u64 = SUPERVISOR_TICK_NANOS / 1_000;
const RESTART_DOWN_DIVISOR: u64 = 4;
const HOOK_HOLD_CHOICE_SHIFT: u32 = 62;
const NOMINAL_COVERAGE_QUANTUM: NonZeroU16 = NonZeroU16::new(1024).unwrap();
const NS_SHIFT: u32 = 24;
const JSON_EVENT_ID: u32 = 0;
const NS_ASSERT: u8 = 1;
const NS_STATE: u8 = 2;
const DISP_HIT: u8 = 0;
const DISP_VIOLATION: u8 = 1;
const STATE_SET: u8 = 0;
const STATE_MAX: u8 = 1;
const ASSERT_PAYLOAD_LEN: usize = 3;
const CATALOG_MAGIC: &[u8; 4] = b"SDKC";
const CATALOG_VERSION: u8 = 1;
const KIND_ALWAYS: u8 = 0;
const KIND_SOMETIMES: u8 = 1;
const KIND_REACHABLE: u8 = 2;
const KIND_UNREACHABLE: u8 = 3;
const KIND_STATE: u8 = 4;
const KIND_BUGGIFY: u8 = 5;
const STATE_PAYLOAD_LEN: usize = 9;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum FaultOperation {
    Wait(NonZeroU16),
    Kill(u16, NonZeroU16),
    KillAll(NonZeroU16),
    PowerLoss(u16, NonZeroU16),
    PowerLossAll(NonZeroU16),
    DiskError(u16, NonZeroU16),
    DiskStall(u16, NonZeroU16),
    DiskCorrupt(u16, NonZeroU16),
    EventKill {
        node: u16,
        rarity: u8,
        ticks: NonZeroU16,
    },
    EventPark {
        node: u16,
        edges: u32,
        hold_us: u32,
        ticks: NonZeroU16,
        #[serde(default, with = "park_target_serde")]
        target: Option<ParkTarget>,
    },
    Pause(u16, NonZeroU16),
    Restart(u16, NonZeroU16),
    Hook(u32, NonZeroU16),
    Clock {
        node: u16,
        offset_ms: i32,
        rate_ppm: i32,
        ticks: NonZeroU16,
    },
    ClockAll {
        offset_ms: i32,
        rate_ppm: i32,
        ticks: NonZeroU16,
    },
    EndFaults(NonZeroU16),
    PauseParked {
        node: u16,
        edges: u32,
        hold_us: u32,
        ticks: NonZeroU16,
        #[serde(default, with = "park_target_serde")]
        target: Option<ParkTarget>,
    },
}

impl FaultOperation {
    #[must_use]
    pub fn target_node(&self) -> Option<u16> {
        match *self {
            Self::Kill(node, _)
            | Self::Pause(node, _)
            | Self::Restart(node, _)
            | Self::PowerLoss(node, _)
            | Self::DiskError(node, _)
            | Self::DiskStall(node, _)
            | Self::DiskCorrupt(node, _)
            | Self::Clock { node, .. }
            | Self::PauseParked { node, .. } => Some(node),
            Self::Wait(_)
            | Self::KillAll(_)
            | Self::PowerLossAll(_)
            | Self::EventKill { .. }
            | Self::EventPark { .. }
            | Self::Hook(..)
            | Self::ClockAll { .. }
            | Self::EndFaults(_) => None,
        }
    }

    #[must_use]
    pub fn target_offer(&self, vocabulary: &FaultVocabulary) -> Vec<u16> {
        match self {
            Self::Kill(..)
            | Self::Pause(..)
            | Self::PauseParked { .. }
            | Self::Restart(..)
            | Self::Clock { .. } => (0..vocabulary.nodes()).collect(),
            Self::PowerLoss(..) | Self::DiskError(..) | Self::DiskStall(..) => {
                vocabulary.disks().to_vec()
            }
            Self::DiskCorrupt(..) => vocabulary.checksummed().to_vec(),
            Self::Wait(_)
            | Self::KillAll(_)
            | Self::PowerLossAll(_)
            | Self::EventKill { .. }
            | Self::EventPark { .. }
            | Self::Hook(..)
            | Self::ClockAll { .. }
            | Self::EndFaults(_) => Vec::new(),
        }
    }

    #[must_use]
    pub fn with_target_node(self, node: u16) -> Self {
        match self {
            Self::Kill(_, ticks) => Self::Kill(node, ticks),
            Self::Pause(_, ticks) => Self::Pause(node, ticks),
            Self::PauseParked {
                edges,
                hold_us,
                ticks,
                target,
                ..
            } => Self::PauseParked {
                node,
                edges,
                hold_us,
                ticks,
                target,
            },
            Self::Restart(_, ticks) => Self::Restart(node, ticks),
            Self::PowerLoss(_, ticks) => Self::PowerLoss(node, ticks),
            Self::DiskError(_, ticks) => Self::DiskError(node, ticks),
            Self::DiskStall(_, ticks) => Self::DiskStall(node, ticks),
            Self::DiskCorrupt(_, ticks) => Self::DiskCorrupt(node, ticks),
            Self::Clock {
                offset_ms,
                rate_ppm,
                ticks,
                ..
            } => Self::Clock {
                node,
                offset_ms,
                rate_ppm,
                ticks,
            },
            other @ (Self::Wait(_)
            | Self::KillAll(_)
            | Self::PowerLossAll(_)
            | Self::EventKill { .. }
            | Self::EventPark { .. }
            | Self::Hook(..)
            | Self::ClockAll { .. }
            | Self::EndFaults(_)) => other,
        }
    }

    #[must_use]
    pub fn ticks(&self) -> u64 {
        match *self {
            Self::Wait(ticks)
            | Self::EndFaults(ticks)
            | Self::Kill(_, ticks)
            | Self::KillAll(ticks)
            | Self::PowerLoss(_, ticks)
            | Self::PowerLossAll(ticks)
            | Self::DiskError(_, ticks)
            | Self::DiskStall(_, ticks)
            | Self::DiskCorrupt(_, ticks)
            | Self::EventKill { ticks, .. }
            | Self::EventPark { ticks, .. }
            | Self::PauseParked { ticks, .. }
            | Self::Pause(_, ticks)
            | Self::Restart(_, ticks)
            | Self::Hook(_, ticks)
            | Self::Clock { ticks, .. }
            | Self::ClockAll { ticks, .. } => u64::from(ticks.get()),
        }
    }

    #[must_use]
    pub fn with_ticks(self, ticks: NonZeroU16) -> Self {
        match self {
            Self::Wait(_) => Self::Wait(ticks),
            Self::EndFaults(_) => Self::EndFaults(ticks),
            Self::Kill(node, _) => Self::Kill(node, ticks),
            Self::KillAll(_) => Self::KillAll(ticks),
            Self::PowerLoss(node, _) => Self::PowerLoss(node, ticks),
            Self::PowerLossAll(_) => Self::PowerLossAll(ticks),
            Self::DiskError(node, _) => Self::DiskError(node, ticks),
            Self::DiskStall(node, _) => Self::DiskStall(node, ticks),
            Self::DiskCorrupt(node, _) => Self::DiskCorrupt(node, ticks),
            Self::EventKill { node, rarity, .. } => Self::EventKill {
                node,
                rarity,
                ticks,
            },
            Self::EventPark {
                node,
                edges,
                hold_us,
                target,
                ..
            } => Self::EventPark {
                node,
                edges,
                hold_us: fitted_hold_us(hold_us, ticks),
                ticks,
                target,
            },
            Self::PauseParked {
                node,
                edges,
                hold_us,
                target,
                ..
            } => Self::PauseParked {
                node,
                edges,
                hold_us: fitted_hold_us(hold_us, ticks),
                ticks,
                target,
            },
            Self::Pause(node, _) => Self::Pause(node, ticks),
            Self::Restart(node, _) => Self::Restart(node, ticks),
            Self::Hook(id, _) => Self::Hook(id, ticks),
            Self::Clock {
                node,
                offset_ms,
                rate_ppm,
                ..
            } => Self::Clock {
                node,
                offset_ms,
                rate_ppm,
                ticks,
            },
            Self::ClockAll {
                offset_ms,
                rate_ppm,
                ..
            } => Self::ClockAll {
                offset_ms,
                rate_ppm,
                ticks,
            },
        }
    }
}

fn fitted_hold_us(hold_us: u32, ticks: NonZeroU16) -> u32 {
    hold_us.min(
        u32::from(ticks.get())
            .saturating_mul(u32::try_from(SUPERVISOR_TICK_MICROS).unwrap_or(u32::MAX)),
    )
}

const OPERATION_KEY_WIDTH: usize = 32;

impl FaultOperation {
    #[must_use]
    pub fn key_bytes(&self) -> [u8; OPERATION_KEY_WIDTH] {
        let mut out = [0u8; OPERATION_KEY_WIDTH];
        let mut at = 0;
        let mut put = |field: &[u8]| {
            out[at..at + field.len()].copy_from_slice(field);
            at += field.len();
        };
        match *self {
            Self::Wait(ticks) => {
                put(&[0]);
                put(&ticks.get().to_le_bytes());
            }
            Self::Kill(node, ticks) => {
                put(&[1]);
                put(&node.to_le_bytes());
                put(&ticks.get().to_le_bytes());
            }
            Self::EventKill {
                node,
                rarity,
                ticks,
            } => {
                put(&[2]);
                put(&node.to_le_bytes());
                put(&[rarity]);
                put(&ticks.get().to_le_bytes());
            }
            Self::EventPark {
                node,
                edges,
                hold_us,
                ticks,
                target,
            } => {
                put(&[3]);
                put(&node.to_le_bytes());
                put(&edges.to_le_bytes());
                put(&hold_us.to_le_bytes());
                put(&ticks.get().to_le_bytes());
                if let Some(target) = target {
                    put(&[1]);
                    put(&target.start.to_le_bytes());
                    put(&target.end.to_le_bytes());
                }
            }
            Self::Pause(node, ticks) => {
                put(&[4]);
                put(&node.to_le_bytes());
                put(&ticks.get().to_le_bytes());
            }
            Self::Restart(node, ticks) => {
                put(&[5]);
                put(&node.to_le_bytes());
                put(&ticks.get().to_le_bytes());
            }
            Self::Hook(id, ticks) => {
                put(&[6]);
                put(&id.to_le_bytes());
                put(&ticks.get().to_le_bytes());
            }
            Self::KillAll(ticks) => {
                put(&[7]);
                put(&ticks.get().to_le_bytes());
            }
            Self::PowerLoss(node, ticks) => {
                put(&[8]);
                put(&node.to_le_bytes());
                put(&ticks.get().to_le_bytes());
            }
            Self::PowerLossAll(ticks) => {
                put(&[9]);
                put(&ticks.get().to_le_bytes());
            }
            Self::DiskError(node, ticks) => {
                put(&[10]);
                put(&node.to_le_bytes());
                put(&ticks.get().to_le_bytes());
            }
            Self::DiskStall(node, ticks) => {
                put(&[11]);
                put(&node.to_le_bytes());
                put(&ticks.get().to_le_bytes());
            }
            Self::DiskCorrupt(node, ticks) => {
                put(&[12]);
                put(&node.to_le_bytes());
                put(&ticks.get().to_le_bytes());
            }
            Self::Clock {
                node,
                offset_ms,
                rate_ppm,
                ticks,
            } => {
                put(&[13]);
                put(&node.to_le_bytes());
                put(&offset_ms.to_le_bytes());
                put(&rate_ppm.to_le_bytes());
                put(&ticks.get().to_le_bytes());
            }
            Self::ClockAll {
                offset_ms,
                rate_ppm,
                ticks,
            } => {
                put(&[14]);
                put(&offset_ms.to_le_bytes());
                put(&rate_ppm.to_le_bytes());
                put(&ticks.get().to_le_bytes());
            }
            Self::EndFaults(ticks) => {
                put(&[15]);
                put(&ticks.get().to_le_bytes());
            }
            Self::PauseParked {
                node,
                edges,
                hold_us,
                ticks,
                target,
            } => {
                put(&[16]);
                put(&node.to_le_bytes());
                put(&edges.to_le_bytes());
                put(&hold_us.to_le_bytes());
                put(&ticks.get().to_le_bytes());
                if let Some(target) = target {
                    put(&[1]);
                    put(&target.start.to_le_bytes());
                    put(&target.end.to_le_bytes());
                }
            }
        }
        out
    }
}

pub const ACTION_KEY_WIDTH: usize = OPERATION_KEY_WIDTH + 2 + 8;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
pub struct FaultAction {
    pub operation: FaultOperation,
    pub coverage_quantum: NonZeroU16,
    #[serde(default)]
    pub choice: u64,
    #[serde(default)]
    pub guest: Option<GuestChoices>,
}

impl Serialize for FaultAction {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let omit_guest = serializer.is_human_readable() && self.guest.is_none();
        let mut state =
            serializer.serialize_struct("FaultAction", if omit_guest { 3 } else { 4 })?;
        state.serialize_field("operation", &self.operation)?;
        state.serialize_field("coverage_quantum", &self.coverage_quantum)?;
        state.serialize_field("choice", &self.choice)?;
        if omit_guest {
            state.skip_field("guest")?;
        } else {
            state.serialize_field("guest", &self.guest)?;
        }
        state.end()
    }
}

impl FaultAction {
    pub fn new(operation: FaultOperation, coverage_quantum: NonZeroU16) -> Self {
        Self {
            operation,
            coverage_quantum,
            choice: 0,
            guest: None,
        }
    }

    #[must_use]
    pub fn with_choice(self, choice: u64) -> Self {
        Self { choice, ..self }
    }

    #[must_use]
    pub fn with_guest(self, guest: GuestChoices) -> Self {
        Self {
            guest: Some(guest),
            ..self
        }
    }

    #[must_use]
    pub fn entropy_seed(&self, base: u64) -> u64 {
        if self.choice == 0 {
            return base;
        }
        let mut z = (base ^ self.choice).wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn ticks(&self) -> u64 {
        self.operation.ticks()
    }

    pub fn with_ticks(self, ticks: NonZeroU16) -> Self {
        Self {
            operation: self.operation.with_ticks(ticks),
            ..self
        }
    }

    #[must_use]
    pub fn is_fault(&self) -> bool {
        !matches!(self.operation, FaultOperation::Wait(_))
    }

    #[must_use]
    pub fn as_wait(self) -> Self {
        let ticks = u16::try_from(action_ticks(&self))
            .ok()
            .and_then(NonZeroU16::new)
            .unwrap_or(NonZeroU16::MIN);
        Self {
            operation: FaultOperation::Wait(ticks),
            ..self
        }
    }

    pub fn key_bytes(&self) -> [u8; ACTION_KEY_WIDTH] {
        let mut bytes = [0; ACTION_KEY_WIDTH];
        bytes[..OPERATION_KEY_WIDTH].copy_from_slice(&self.operation.key_bytes());
        bytes[OPERATION_KEY_WIDTH..OPERATION_KEY_WIDTH + 2]
            .copy_from_slice(&self.coverage_quantum.get().to_le_bytes());
        bytes[OPERATION_KEY_WIDTH + 2..].copy_from_slice(&self.choice.to_le_bytes());
        bytes
    }
}

impl From<FaultOperation> for FaultAction {
    fn from(operation: FaultOperation) -> Self {
        Self::new(operation, NOMINAL_COVERAGE_QUANTUM)
    }
}

mod park_target_serde {
    use fault_policy::ParkTarget;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Deserialize, Serialize)]
    struct Range {
        start: u64,
        end: u64,
    }

    pub fn serialize<S: Serializer>(
        target: &Option<ParkTarget>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        target
            .map(|target| Range {
                start: target.start,
                end: target.end,
            })
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<ParkTarget>, D::Error> {
        Option::<Range>::deserialize(deserializer)?
            .map(|range| {
                ParkTarget::new(range.start, range.end).ok_or_else(|| {
                    serde::de::Error::custom("a park target must be a nonempty range")
                })
            })
            .transpose()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FaultEndpointCertificate {
    KvmWholeState([u8; 32]),
    KvmUnavailableAfterFailure,
    UmlWholeStateUnavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultSnapshot {
    pub history: crate::history::FaultHistory,
    pub observation: FaultObservations,
    pub failed: bool,
    pub endpoint: Option<FaultEndpointCertificate>,
    pub event_history: Option<(u64, [u8; 32])>,
    pub accumulator: Accumulator,
    #[serde(with = "crate::key::ObservedDef")]
    pub observed: Observed,
}

impl Default for FaultSnapshot {
    fn default() -> Self {
        Self {
            history: crate::history::FaultHistory::default(),
            observation: FaultObservations::default(),
            failed: false,
            endpoint: None,
            event_history: None,
            accumulator: Accumulator::default(),
            observed: crate::key::UNOBSERVED,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActionDelta {
    pub standing: Vec<StandingWindow>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActionWindows {
    pub root_seal: u64,
    pub hold_hooks: bool,
}

#[cfg(test)]
thread_local! {
    pub(crate) static TIMELINE_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl ActionWindows {
    fn end(self, start: u64, action: &FaultAction) -> Result<u64, String> {
        #[cfg(test)]
        TIMELINE_STEPS.with(|steps| steps.set(steps.get() + 1));
        start
            .checked_add(action_ticks(action) * SUPERVISOR_TICK_NANOS)
            .ok_or_else(|| "action timeline overflows guest time".to_owned())
    }

    pub fn iter<'a>(
        self,
        actions: impl IntoIterator<Item = &'a FaultAction>,
    ) -> impl Iterator<Item = Result<(u64, u64), String>> {
        actions
            .into_iter()
            .scan(Some(self.root_seal), move |start, action| {
                let from = (*start)?;
                let end = self.end(from, action);
                *start = end.as_ref().ok().copied();
                Some(end.map(|end| (from, end)))
            })
    }

    pub fn window(self, actions: &[FaultAction], index: usize) -> Result<(u64, u64), String> {
        let action = actions
            .get(index)
            .ok_or("action index is outside the input")?;
        let start = actions[..index]
            .iter()
            .try_fold(self.root_seal, |start, action| self.end(start, action))?;
        Ok((start, self.end(start, action)?))
    }

    #[must_use]
    pub fn delta(self, action: FaultAction, window: (u64, u64)) -> ActionDelta {
        action_delta(action, window, self.hold_hooks)
    }

    pub fn push_standing(
        self,
        standing: &mut Vec<StandingWindow>,
        action: FaultAction,
        window: (u64, u64),
    ) {
        if matches!(action.operation, FaultOperation::EndFaults(_)) {
            for open in standing.iter_mut().filter(|open| open.end > window.0) {
                open.end = window.0;
            }
            standing.retain(|open| open.start < open.end);
        }
        standing.extend(self.delta(action, window).standing);
    }
}

#[must_use]
pub fn action_ticks(action: &FaultAction) -> u64 {
    match action.operation {
        FaultOperation::PauseParked { ticks, .. } => u64::from(ticks.get()) * 2,
        _ => action.ticks(),
    }
}

fn restart_end(start: u64, horizon: u64) -> u64 {
    start.saturating_add((horizon / RESTART_DOWN_DIVISOR).max(SUPERVISOR_TICK_NANOS))
}

fn hook_hold_steps(choice: u64) -> u64 {
    1 << (choice >> HOOK_HOLD_CHOICE_SHIFT)
}

fn hook_end(start: u64, horizon: u64, choice: u64) -> u64 {
    start.saturating_add(horizon.saturating_mul(hook_hold_steps(choice)))
}

fn standing(target: Vec<u8>, window: (u64, u64)) -> StandingWindow {
    StandingWindow {
        class: DecisionClass::Process.as_u16(),
        target,
        start: window.0,
        end: window.1,
    }
}

fn event_park_delta(
    node: u16,
    edges: u32,
    hold_us: u32,
    target: Option<ParkTarget>,
    at_resume: bool,
    window: (u64, u64),
) -> ActionDelta {
    ActionDelta {
        standing: vec![standing(
            process_target(
                node,
                &Fault::ProcEventPark {
                    edges,
                    hold: Span(u64::from(hold_us).saturating_mul(1_000)),
                    target,
                    at_resume,
                },
            ),
            window,
        )],
    }
}

fn action_delta(action: FaultAction, window: (u64, u64), hold_hooks: bool) -> ActionDelta {
    let (start, end) = window;
    let horizon = end.saturating_sub(start);
    match action.operation {
        FaultOperation::Wait(_) | FaultOperation::EndFaults(_) => ActionDelta::default(),
        FaultOperation::EventKill { node, rarity, .. } => ActionDelta {
            standing: vec![standing(
                process_target(node, &Fault::ProcEventKill { rarity }),
                (start, u64::MAX),
            )],
        },
        FaultOperation::EventPark {
            node,
            edges,
            hold_us,
            target,
            ..
        } => event_park_delta(node, edges, hold_us, target, false, window),
        FaultOperation::PauseParked {
            node,
            edges,
            hold_us,
            target,
            ..
        } => {
            let resume = start.saturating_add(horizon / 2);
            let mut delta = event_park_delta(node, edges, hold_us, target, true, (resume, end));
            delta.standing.insert(
                0,
                standing(
                    process_target(node, &Fault::ProcPause(Span(resume - start))),
                    (start, resume),
                ),
            );
            delta
        }
        FaultOperation::Kill(node, _) => ActionDelta {
            standing: vec![standing(
                process_target(node, &Fault::ProcRestart),
                (start, end),
            )],
        },
        FaultOperation::KillAll(_) => ActionDelta {
            standing: vec![standing(
                process_target(EVERY_NODE, &Fault::ProcRestart),
                (start, end),
            )],
        },
        FaultOperation::PowerLoss(node, _) => ActionDelta {
            standing: vec![
                standing(process_target(node, &Fault::ProcRestart), (start, end)),
                standing(
                    process_target(
                        node,
                        &Fault::ProcPowerLoss {
                            choice: action.choice,
                        },
                    ),
                    (start, end),
                ),
            ],
        },
        FaultOperation::PowerLossAll(_) => ActionDelta {
            standing: vec![
                standing(
                    process_target(EVERY_NODE, &Fault::ProcRestart),
                    (start, end),
                ),
                standing(
                    process_target(
                        EVERY_NODE,
                        &Fault::ProcPowerLoss {
                            choice: action.choice,
                        },
                    ),
                    (start, end),
                ),
            ],
        },
        FaultOperation::DiskError(node, _) => ActionDelta {
            standing: vec![standing(
                process_target(node, &Fault::ProcDiskError),
                (start, end),
            )],
        },
        FaultOperation::DiskStall(node, _) => ActionDelta {
            standing: vec![standing(
                process_target(node, &Fault::ProcDiskStall(Span(horizon))),
                (start, end),
            )],
        },
        FaultOperation::DiskCorrupt(node, _) => ActionDelta {
            standing: vec![standing(
                process_target(
                    node,
                    &Fault::ProcDiskCorrupt {
                        choice: action.choice,
                    },
                ),
                (start, restart_end(start, horizon)),
            )],
        },
        FaultOperation::Pause(node, _) => ActionDelta {
            standing: vec![standing(
                process_target(node, &Fault::ProcPause(Span(horizon))),
                (start, end),
            )],
        },
        FaultOperation::Restart(node, _) => ActionDelta {
            standing: vec![standing(
                process_target(node, &Fault::ProcRestart),
                (start, restart_end(start, horizon)),
            )],
        },
        FaultOperation::Hook(id, _) => ActionDelta {
            standing: vec![standing(
                process_target(0, &Fault::RunHook(id)),
                if hold_hooks {
                    (start, hook_end(start, horizon, action.choice))
                } else {
                    (start, end)
                },
            )],
        },
        FaultOperation::Clock {
            node,
            offset_ms,
            rate_ppm,
            ..
        } => ActionDelta {
            standing: vec![standing(
                process_target(node, &clock_fault(offset_ms, rate_ppm, 0)),
                (start, end),
            )],
        },
        FaultOperation::ClockAll {
            offset_ms,
            rate_ppm,
            ..
        } => ActionDelta {
            standing: vec![standing(
                process_target(EVERY_NODE, &clock_fault(offset_ms, rate_ppm, action.choice)),
                (start, end),
            )],
        },
    }
}

fn clock_fault(offset_ms: i32, rate_ppm: i32, spread: u64) -> Fault {
    Fault::ProcClock {
        offset: i64::from(offset_ms) * 1_000_000,
        rate_ppm,
        spread,
    }
}

pub fn action_deltas(
    windows: ActionWindows,
    actions: &[FaultAction],
) -> Result<Vec<ActionDelta>, String> {
    actions
        .iter()
        .zip(windows.iter(actions))
        .map(|(action, window)| window.map(|window| windows.delta(*action, window)))
        .collect()
}

pub fn standing_windows(
    windows: ActionWindows,
    inherited: &[StandingWindow],
    actions: &[FaultAction],
) -> Result<Vec<StandingWindow>, String> {
    let mut standing = inherited.to_vec();
    for (action, window) in actions.iter().zip(windows.iter(actions)) {
        windows.push_standing(&mut standing, *action, window?);
    }
    Ok(standing)
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckEvidence {
    pub disturbance_generation: u64,
    pub run: u64,
    pub start_generation: u64,
    pub end_generation: u64,
    pub points: Vec<String>,
    #[serde(default)]
    pub undecided_keys: u64,
    pub pending_faults: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SdkCapture {
    pub registers: BTreeMap<u32, u64>,
    pub assertions: Assertions,
    pub setup_complete: bool,
    pub completed_check: Option<CompletedCheck>,
    #[serde(default)]
    pub undecided_keys: u64,
    pub parks: Vec<ParkLanding>,
    pub park_reads: Vec<ParkRead>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParkLanding {
    pub moment: u64,
    pub site: u64,
    pub edges: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParkRead {
    pub moment: u64,
    pub site: u64,
    pub edges: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompletedCheck {
    pub run: u64,
    pub start_generation: u64,
    pub end_generation: u64,
    pub points: Vec<String>,
}

impl SdkCapture {
    fn note_completed_check(
        &mut self,
        run: u64,
        check_passes: &BTreeMap<u64, BTreeSet<String>>,
        check_undecided: &BTreeMap<u64, u64>,
    ) {
        let register = |id: u32| self.registers.get(&id).copied().unwrap_or_default();
        let pid = register(reg::COMPLETED_CHECK_PID);
        self.undecided_keys = check_undecided.get(&pid).copied().unwrap_or_default();
        let Some(points) = check_passes.get(&pid).filter(|points| !points.is_empty()) else {
            return;
        };
        self.completed_check = Some(CompletedCheck {
            run,
            start_generation: register(reg::COMPLETED_CHECK_START_GENERATION),
            end_generation: register(reg::COMPLETED_CHECK_END_GENERATION),
            points: points.iter().cloned().collect(),
        });
    }

    pub fn check_infrastructure_status(&self) -> Result<(), String> {
        if self
            .registers
            .get(&reg::INFRASTRUCTURE_ERROR)
            .copied()
            .unwrap_or_default()
            != 0
        {
            return Err("fault agent reported an infrastructure failure".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct SdkDecoderState {
    capture: SdkCapture,
    check_passes: BTreeMap<u64, BTreeSet<String>>,
    #[serde(default)]
    check_undecided: BTreeMap<u64, u64>,
    last_moment: u64,
    offset: u32,
    action_after: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SdkDecoder {
    state: std::sync::Arc<SdkDecoderState>,
}

impl Serialize for SdkDecoder {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.state.as_ref().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SdkDecoder {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        SdkDecoderState::deserialize(deserializer).map(|state| Self {
            state: std::sync::Arc::new(state),
        })
    }
}

impl SdkDecoder {
    pub fn offset(&self) -> u32 {
        self.state.offset
    }
    pub fn capture(&self) -> &SdkCapture {
        &self.state.capture
    }
    pub fn moment(&self) -> u64 {
        self.state.last_moment
    }
    pub fn action_after(&self) -> Option<u64> {
        self.state.action_after
    }
    pub fn begin_action(&mut self, after: u64) {
        if self.state.action_after == Some(after) {
            return;
        }
        let state = std::sync::Arc::make_mut(&mut self.state);
        state.action_after = Some(after);
        state.capture.parks.retain(|park| park.moment > after);
        state.capture.park_reads.retain(|read| read.moment > after);
    }
    pub fn retained_allocation(&self) -> (usize, usize) {
        fn tree_entry<K, V>() -> usize {
            16 * std::mem::size_of::<(K, V)>() + 16 * std::mem::size_of::<usize>() + 64
        }
        let capture = self.capture();
        let mut bytes = std::mem::size_of::<SdkDecoderState>() + 2 * std::mem::size_of::<usize>();
        bytes += capture.registers.len() * tree_entry::<u32, u64>();
        bytes += capture
            .assertions
            .0
            .iter()
            .map(|(id, outcome)| {
                tree_entry::<String, AssertionOutcome>()
                    + id.capacity()
                    + outcome.message.capacity()
                    + outcome.location.capacity()
            })
            .sum::<usize>();
        bytes += self
            .state
            .check_passes
            .values()
            .map(|points| {
                tree_entry::<u64, BTreeSet<String>>()
                    + points
                        .iter()
                        .map(|point| tree_entry::<String, ()>() + point.capacity())
                        .sum::<usize>()
            })
            .sum::<usize>();
        bytes += self.state.check_undecided.len() * tree_entry::<u64, u64>();
        bytes += capture.parks.capacity() * std::mem::size_of::<ParkLanding>();
        bytes += capture.park_reads.capacity() * std::mem::size_of::<ParkRead>();
        if let Some(check) = &capture.completed_check {
            bytes += check.points.capacity() * std::mem::size_of::<String>();
            bytes += check.points.iter().map(String::capacity).sum::<usize>();
        }
        (std::sync::Arc::as_ptr(&self.state) as usize, bytes)
    }
    pub fn append(&mut self, events: &[(u64, u32, Vec<u8>)]) -> Result<usize, String> {
        if events.is_empty() {
            return Ok(0);
        }
        let count = u32::try_from(events.len()).map_err(|_| "SDK event count overflow")?;
        let offset = self
            .offset()
            .checked_add(count)
            .ok_or("SDK event offset overflow")?;
        let mut staged = self.clone();
        let state = std::sync::Arc::make_mut(&mut staged.state);
        let capture = &mut state.capture;
        let check_passes = &mut state.check_passes;
        let check_undecided = &mut state.check_undecided;
        let mut processed = 0_usize;
        for (moment, event_id, bytes) in events {
            processed += 1;
            if *event_id == JSON_EVENT_ID && bytes.trim_ascii_start().first() == Some(&b'{') {
                if let Some(event) = decode_json_event(bytes) {
                    capture.setup_complete |= event.setup_complete;
                    if let (Some(keys), Some(pid)) = (event.undecided_keys, event.pid) {
                        check_undecided.insert(pid, keys);
                    }
                    if let Some(park) = event.park {
                        capture.parks.push(ParkLanding {
                            moment: *moment,
                            site: park.site,
                            edges: park.edges,
                        });
                    }
                    if let Some(read) = event.park_read {
                        capture.park_reads.push(ParkRead {
                            moment: *moment,
                            site: read.site,
                            edges: read.edges,
                        });
                    }
                    if let Some((id, outcome)) = event.assertion {
                        if let Some(pid) = event.pid
                            && outcome.feeds_the_key()
                        {
                            check_passes.entry(pid).or_default().insert(id.clone());
                        }
                        capture.assertions.record(id, outcome);
                    }
                }
                continue;
            }
            if *event_id == JSON_EVENT_ID && declare_sdk_catalog(bytes, &mut capture.assertions)? {
                continue;
            }
            let namespace = (event_id >> NS_SHIFT) as u8;
            let local = event_id & ((1 << NS_SHIFT) - 1);
            match namespace {
                NS_ASSERT if bytes.len() == ASSERT_PAYLOAD_LEN => {
                    record_sdk_assertion(&mut capture.assertions, local, bytes[0])?;
                }
                NS_STATE if bytes.len() == STATE_PAYLOAD_LEN => {
                    let value = u64::from_le_bytes(
                        bytes[1..STATE_PAYLOAD_LEN]
                            .try_into()
                            .map_err(|_| "SDK state payload is truncated")?,
                    );
                    match bytes[0] {
                        STATE_SET => {
                            let previous = capture.registers.insert(local, value);
                            if local == reg::CHECKS_STARTED && previous != Some(value) {
                                check_passes.clear();
                                check_undecided.clear();
                            }
                            if local == reg::COMPLETED_CHECK_RUN {
                                capture.note_completed_check(value, check_passes, check_undecided);
                            }
                        }
                        STATE_MAX => {
                            capture
                                .registers
                                .entry(local)
                                .and_modify(|current| *current = (*current).max(value))
                                .or_insert(value);
                        }
                        _ => return Err("SDK state event has an unknown operation".to_owned()),
                    }
                }
                _ => {}
            }
        }

        if let Some(after) = state.action_after {
            capture.parks.retain(|park| park.moment > after);
            capture.park_reads.retain(|read| read.moment > after);
        }
        state.last_moment = events.last().map_or(state.last_moment, |event| event.0);
        state.offset = offset;
        *self = staged;
        Ok(processed)
    }
}

#[cfg(feature = "consonance")]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ObservationCheckpoint {
    version: u16,
    receipt: [u8; 32],
    decoder: SdkDecoder,
}

#[cfg(feature = "consonance")]
impl ObservationCheckpoint {
    pub(crate) fn capture(
        receipt: &crate::history::HistoryReceipt,
        decoder: SdkDecoder,
    ) -> Result<Self, String> {
        let checkpoint = Self {
            version: 1,
            receipt: receipt.root(),
            decoder,
        };
        checkpoint.validate_binding(receipt)?;
        Ok(checkpoint)
    }
    pub(crate) fn decoder(&self) -> &SdkDecoder {
        &self.decoder
    }
    pub(crate) fn validate_binding(
        &self,
        receipt: &crate::history::HistoryReceipt,
    ) -> Result<(), String> {
        let after = (receipt.depth() != 0).then(|| receipt.previous_moment());
        let observation = FaultObservations::new(
            self.decoder.moment(),
            self.decoder.capture(),
            FaultStop::Deadline,
        );
        if self.version != 1
            || self.receipt != receipt.root()
            || self.decoder.action_after() != after
            || self.decoder.moment() != receipt.moment()
            || !receipt.matches_observation(&observation)?
        {
            return Err("observation checkpoint differs from captured history".to_owned());
        }
        self.decoder.capture().check_infrastructure_status()
    }
}

fn declare_sdk_catalog(bytes: &[u8], assertions: &mut Assertions) -> Result<bool, String> {
    let Some(rest) = bytes.strip_prefix(CATALOG_MAGIC) else {
        return Ok(false);
    };
    let truncated = || "SDK catalog is truncated".to_owned();
    let (&version, rest) = rest.split_first().ok_or_else(truncated)?;
    if version != CATALOG_VERSION {
        return Err(format!("SDK catalog has unsupported version {version}"));
    }
    let (count, mut rest) = rest.split_at_checked(4).ok_or_else(truncated)?;
    let count = u32::from_le_bytes(count.try_into().map_err(|_| truncated())?);
    for _ in 0..count {
        let (head, tail) = rest.split_at_checked(7).ok_or_else(truncated)?;
        let id = u32::from_le_bytes(head[1..5].try_into().map_err(|_| truncated())?);
        let name_len = usize::from(u16::from_le_bytes([head[5], head[6]]));
        let (name, tail) = tail.split_at_checked(name_len).ok_or_else(truncated)?;
        rest = tail;
        let kind = match head[0] {
            KIND_ALWAYS => AssertionKind::Always,
            KIND_SOMETIMES => AssertionKind::Sometimes,
            KIND_REACHABLE => AssertionKind::Reachable,
            KIND_UNREACHABLE => AssertionKind::Unreachable,
            KIND_STATE | KIND_BUGGIFY => continue,
            other => return Err(format!("SDK catalog has unknown point kind {other}")),
        };
        assertions.record(
            id.to_string(),
            AssertionOutcome {
                kind,
                message: String::from_utf8_lossy(name).into_owned(),
                location: String::new(),
                passed: false,
                failed: false,
                holds: None,
            },
        );
    }
    if !rest.is_empty() {
        return Err("SDK catalog has trailing bytes".to_owned());
    }
    Ok(true)
}

fn record_sdk_assertion(
    assertions: &mut Assertions,
    local: u32,
    disposition: u8,
) -> Result<(), String> {
    let id = local.to_string();
    let declared = assertions.0.get(&id).map(|outcome| outcome.kind);
    let (kind, passed) = match disposition {
        DISP_HIT => (declared.unwrap_or(AssertionKind::Sometimes), true),
        DISP_VIOLATION => (declared.unwrap_or(AssertionKind::Always), false),
        _ => return Err("SDK assertion event has an unknown disposition".to_owned()),
    };
    assertions.record(
        id.clone(),
        AssertionOutcome {
            kind,
            message: id,
            location: String::new(),
            passed,
            failed: !passed,
            holds: (kind == AssertionKind::Sometimes).then_some(passed),
        },
    );
    Ok(())
}

pub fn decode_sdk_events(events: &[(u64, u32, Vec<u8>)]) -> Result<SdkCapture, String> {
    let mut decoder = SdkDecoder::default();
    decoder.append(events)?;
    Ok(decoder.capture().clone())
}

#[cfg(test)]
fn decode_sdk_events_reference(events: &[(u64, u32, Vec<u8>)]) -> Result<SdkCapture, String> {
    let mut capture = SdkCapture::default();
    let mut check_passes: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
    let mut check_undecided: BTreeMap<u64, u64> = BTreeMap::new();
    for (moment, event_id, bytes) in events {
        if *event_id == JSON_EVENT_ID && bytes.trim_ascii_start().first() == Some(&b'{') {
            if let Some(event) = decode_json_event(bytes) {
                capture.setup_complete |= event.setup_complete;
                if let (Some(keys), Some(pid)) = (event.undecided_keys, event.pid) {
                    check_undecided.insert(pid, keys);
                }
                if let Some(park) = event.park {
                    capture.parks.push(ParkLanding {
                        moment: *moment,
                        site: park.site,
                        edges: park.edges,
                    });
                }
                if let Some(read) = event.park_read {
                    capture.park_reads.push(ParkRead {
                        moment: *moment,
                        site: read.site,
                        edges: read.edges,
                    });
                }
                if let Some((id, outcome)) = event.assertion {
                    if let Some(pid) = event.pid
                        && outcome.feeds_the_key()
                    {
                        check_passes.entry(pid).or_default().insert(id.clone());
                    }
                    capture.assertions.record(id, outcome);
                }
            }
            continue;
        }
        if *event_id == JSON_EVENT_ID && declare_sdk_catalog(bytes, &mut capture.assertions)? {
            continue;
        }
        let namespace = (event_id >> NS_SHIFT) as u8;
        let local = event_id & ((1 << NS_SHIFT) - 1);
        match namespace {
            NS_ASSERT if bytes.len() == ASSERT_PAYLOAD_LEN => {
                record_sdk_assertion(&mut capture.assertions, local, bytes[0])?;
            }
            NS_STATE if bytes.len() == STATE_PAYLOAD_LEN => {
                let value = u64::from_le_bytes(
                    bytes[1..STATE_PAYLOAD_LEN]
                        .try_into()
                        .map_err(|_| "SDK state payload is truncated")?,
                );
                match bytes[0] {
                    STATE_SET => {
                        let previous = capture.registers.insert(local, value);
                        if local == reg::CHECKS_STARTED && previous != Some(value) {
                            check_passes.clear();
                            check_undecided.clear();
                        }
                        if local == reg::COMPLETED_CHECK_RUN {
                            capture.note_completed_check(value, &check_passes, &check_undecided);
                        }
                    }
                    STATE_MAX => {
                        capture
                            .registers
                            .entry(local)
                            .and_modify(|current| *current = (*current).max(value))
                            .or_insert(value);
                    }
                    _ => return Err("SDK state event has an unknown operation".to_owned()),
                }
            }
            _ => {}
        }
    }
    Ok(capture)
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum FaultStop {
    #[default]
    Deadline,
    Quiescent,
    Assertion {
        point: u32,
    },
    Crash,
    Unexpected,
}

impl FaultStop {
    #[must_use]
    pub fn from_stop_reason(reason: &StopReason) -> Self {
        match reason {
            StopReason::Deadline { .. } => Self::Deadline,
            StopReason::Quiescent { .. } => Self::Quiescent,
            StopReason::Assertion { ev, .. } => Self::Assertion { point: ev.id },
            StopReason::Crash { .. } => Self::Crash,
            _ => Self::Unexpected,
        }
    }

    #[must_use]
    pub fn is_bug(self) -> bool {
        matches!(self, Self::Assertion { .. } | Self::Crash)
    }

    #[must_use]
    pub fn is_continuable(self) -> bool {
        matches!(self, Self::Deadline)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultObservations {
    pub moment: u64,
    pub ticks: u64,
    pub alive: u64,
    pub hooks_started: u64,
    pub hooks_finished: u64,
    pub unexpected_deaths: u64,
    pub restarts: u64,
    pub event_kill_fires: u64,
    pub event_kill_site: u64,
    pub event_ready: u64,
    pub event_park_fires: u64,
    pub event_park_held: u64,
    pub event_kill_armed: u64,
    pub edge_crossings: u64,
    pub edge_digest: u64,
    pub workload_started: u64,
    pub workload_finished: u64,
    pub checks_started: u64,
    pub checks_finished: u64,
    pub check: Option<CheckEvidence>,
    pub assertions: Assertions,
    pub stop: FaultStop,
    #[serde(default)]
    pub watchdog_cutoff: bool,
    #[serde(default)]
    pub infrastructure_failure: bool,
    #[serde(default)]
    pub parks: Vec<ParkLanding>,
    #[serde(default)]
    pub park_reads: Vec<ParkRead>,
    #[serde(default)]
    pub fault_free_checks_started: u64,
    #[serde(default)]
    pub fault_free_checks_finished: u64,
    #[serde(default)]
    pub faults_in_force: bool,
    #[serde(default)]
    pub recovery_pending: bool,
    #[serde(default)]
    pub fault_free_check_passed: u64,
    #[serde(default)]
    pub stage_fall: Option<u8>,
}

impl FaultObservations {
    #[must_use]
    pub fn new(moment: u64, capture: &SdkCapture, stop: FaultStop) -> Self {
        let value = |id: u32| capture.registers.get(&id).copied().unwrap_or_default();
        Self {
            moment,
            ticks: value(reg::TICKS),
            alive: value(reg::ALIVE),
            hooks_started: value(reg::HOOKS_STARTED),
            hooks_finished: value(reg::HOOKS_FINISHED),
            unexpected_deaths: value(reg::UNEXPECTED_DEATHS),
            restarts: value(reg::RESTARTS),
            event_kill_fires: value(reg::EVENT_KILL_FIRES),
            event_kill_site: value(reg::EVENT_KILL_SITE),
            event_ready: value(reg::EVENT_READY),
            event_park_fires: value(reg::EVENT_PARK_FIRES),
            event_park_held: value(reg::EVENT_PARK_HELD),
            event_kill_armed: value(reg::EVENT_KILL_ARMED),
            edge_crossings: value(reg::EDGE_CROSSINGS),
            edge_digest: value(reg::EDGE_DIGEST),
            workload_started: value(reg::WORKLOAD_STARTED),
            workload_finished: value(reg::WORKLOAD_FINISHED),
            checks_started: value(reg::CHECKS_STARTED),
            checks_finished: value(reg::CHECKS_FINISHED),
            check: (value(reg::CHECK_ENABLED) != 0).then(|| {
                let completed = capture.completed_check.clone().unwrap_or_default();
                CheckEvidence {
                    disturbance_generation: value(reg::DISTURBANCE_GENERATION),
                    run: completed.run,
                    start_generation: completed.start_generation,
                    end_generation: completed.end_generation,
                    points: completed.points,
                    undecided_keys: capture.undecided_keys,
                    pending_faults: value(reg::PENDING_FAULTS),
                }
            }),
            assertions: capture.assertions.clone(),
            stop,
            watchdog_cutoff: false,
            infrastructure_failure: false,
            parks: capture.parks.clone(),
            park_reads: capture.park_reads.clone(),
            fault_free_checks_started: value(reg::FAULT_FREE_CHECKS_STARTED),
            fault_free_checks_finished: value(reg::FAULT_FREE_CHECKS_FINISHED),
            faults_in_force: value(reg::FAULTS_IN_FORCE) != 0,
            recovery_pending: value(reg::RECOVERY_PENDING) != 0,
            fault_free_check_passed: value(reg::FAULT_FREE_CHECK_PASSED),
            stage_fall: None,
        }
    }

    #[must_use]
    pub fn sometimes(&self) -> BTreeSet<String> {
        self.assertions
            .key_ids()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    #[must_use]
    pub fn violations(&self) -> BTreeSet<String> {
        self.assertions.violations()
    }

    #[must_use]
    pub fn is_bug(&self) -> bool {
        !self.watchdog_cutoff
            && !self.infrastructure_failure
            && (self.stop.is_bug() || !self.violations().is_empty())
    }

    #[must_use]
    pub fn exit_kind(&self) -> ExitKind {
        if self.stop == FaultStop::Crash {
            ExitKind::Crash
        } else {
            ExitKind::Ok
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use control_proto::Moment;
    use fault_policy::decode_process_target;

    const ROOT: u64 = 1_000;
    const WINDOWS: ActionWindows = ActionWindows {
        root_seal: ROOT,
        hold_hooks: false,
    };
    const HELD: ActionWindows = ActionWindows {
        root_seal: ROOT,
        hold_hooks: true,
    };
    const TICK: u64 = SUPERVISOR_TICK_NANOS;

    fn ticks(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).unwrap()
    }

    fn kills(count: usize) -> Vec<FaultAction> {
        vec![FaultOperation::Kill(0, ticks(50)).into(); count]
    }

    fn assert_event(point: u32, disposition: u8) -> (u64, u32, Vec<u8>) {
        (
            0,
            ((NS_ASSERT as u32) << NS_SHIFT) | point,
            vec![disposition, 0, 0],
        )
    }

    fn state_event(register: u32, op: u8, value: u64) -> (u64, u32, Vec<u8>) {
        let mut bytes = vec![op];
        bytes.extend_from_slice(&value.to_le_bytes());
        (0, ((NS_STATE as u32) << NS_SHIFT) | register, bytes)
    }

    fn cursor_events() -> Vec<(u64, u32, Vec<u8>)> {
        let mut events = vec![
            state_event(reg::CHECK_ENABLED, STATE_SET, 1),
            state_event(reg::COMPLETED_CHECK_PID, STATE_SET, 40),
            state_event(reg::CHECKS_STARTED, STATE_SET, 1),
            json_pass(40, "first"),
            (4, 0, br#"{"harmony_park":{"site":7,"edges":8}}"#.to_vec()),
            (
                5,
                0,
                br#"{"harmony_park_read":{"site":9,"edges":10}}"#.to_vec(),
            ),
            state_event(reg::CHECKS_STARTED, STATE_SET, 1),
            state_event(reg::COMPLETED_CHECK_RUN, STATE_SET, 7),
            assert_event(3, DISP_HIT),
            state_event(reg::TICKS, STATE_MAX, 9),
            state_event(reg::TICKS, STATE_MAX, 2),
            state_event(reg::CHECKS_STARTED, STATE_SET, 2),
            state_event(reg::COMPLETED_CHECK_RUN, STATE_SET, 8),
            json_pass(40, "second"),
            state_event(reg::COMPLETED_CHECK_RUN, STATE_SET, 9),
            (99, 0, b"{malformed".to_vec()),
            (2, u32::MAX, vec![1, 2, 3]),
        ];
        events[3].0 = 3;
        events
    }

    #[test]
    fn cursor_decoder_matches_independent_full_reference_at_chunk_boundaries() {
        let events = cursor_events();
        for chunk_size in 1..=events.len() {
            let mut decoder = SdkDecoder::default();
            let mut end = 0;
            for chunk in events.chunks(chunk_size) {
                assert_eq!(decoder.append(chunk).unwrap(), chunk.len());
                end += chunk.len();
                assert_eq!(
                    decoder.capture(),
                    &decode_sdk_events_reference(&events[..end]).unwrap()
                );
                assert_eq!(decoder.offset() as usize, end);
                assert_eq!(decoder.moment(), events[end - 1].0);
            }
            assert_eq!(decoder.append(&[]).unwrap(), 0);
            assert_eq!(decoder.moment(), 2);
        }
        let short = &events[..8];
        for boundaries in 0..(1_usize << (short.len() - 1)) {
            let mut decoder = SdkDecoder::default();
            let mut start = 0;
            for end in 1..=short.len() {
                if end == short.len() || boundaries & (1 << (end - 1)) != 0 {
                    decoder.append(&short[start..end]).unwrap();
                    assert_eq!(
                        decoder.capture(),
                        &decode_sdk_events_reference(&short[..end]).unwrap()
                    );
                    start = end;
                }
            }
        }
    }

    #[test]
    fn cursor_decode_errors_preserve_all_visible_and_hidden_state() {
        let events = cursor_events();
        let mut decoder = SdkDecoder::default();
        decoder.append(&events[..8]).unwrap();
        decoder.begin_action(3);
        for malformed in [assert_event(1, 9), state_event(reg::CHECKS_STARTED, 9, 2)] {
            let before = decoder.clone();
            let bytes = postcard::to_allocvec(&decoder).unwrap();
            let batch = vec![
                state_event(reg::CHECKS_STARTED, STATE_SET, 77),
                json_pass(40, "stale"),
                events[4].clone(),
                malformed,
            ];
            assert!(decoder.append(&batch).is_err());
            assert_eq!(decoder, before);
            assert_eq!(postcard::to_allocvec(&decoder).unwrap(), bytes);
        }
        let mut resumed: SdkDecoder =
            postcard::from_bytes(&postcard::to_allocvec(&decoder).unwrap()).unwrap();
        resumed.append(&completion(40, 11, 3)).unwrap();
        assert_eq!(
            resumed.capture().completed_check.as_ref().unwrap().points,
            vec!["first"]
        );
    }

    #[test]
    fn cursor_action_projection_preserves_repeated_evidence_and_drops_old_parks() {
        let events = cursor_events();
        let mut decoder = SdkDecoder::default();
        decoder.append(&events[..8]).unwrap();
        decoder.begin_action(4);
        let mut expected = decode_sdk_events_reference(&events[..8]).unwrap();
        expected.parks.retain(|park| park.moment > 4);
        expected.park_reads.retain(|read| read.moment > 4);
        assert_eq!(decoder.capture(), &expected);
        let before = decoder.clone();
        decoder.append(&[]).unwrap();
        assert_eq!(decoder, before);
        decoder.append(&events[8..]).unwrap();
        expected = decode_sdk_events_reference(&events).unwrap();
        expected.parks.retain(|park| park.moment > 4);
        expected.park_reads.retain(|read| read.moment > 4);
        assert_eq!(decoder.capture(), &expected);
        decoder.begin_action(5);
        assert!(decoder.capture().parks.is_empty());
        assert!(decoder.capture().park_reads.is_empty());
    }

    #[test]
    fn windows_tile_the_axis_by_each_action_duration() {
        assert_eq!(
            WINDOWS.window(&kills(4), 0).unwrap(),
            (ROOT, ROOT + 50 * TICK)
        );
        let (start, end) = WINDOWS.window(&kills(4), 3).unwrap();
        assert_eq!(start, ROOT + 150 * TICK);
        assert_eq!(end, start + 50 * TICK);
        let mixed = [
            FaultOperation::Kill(0, ticks(3)),
            FaultOperation::Hook(1, ticks(7)),
            FaultOperation::Pause(0, ticks(2)),
        ]
        .map(FaultAction::from);
        assert_eq!(
            WINDOWS.window(&mixed, 2).unwrap(),
            (ROOT + 10 * TICK, ROOT + 12 * TICK)
        );
        let parked = [
            FaultOperation::PauseParked {
                node: 0,
                edges: 1,
                hold_us: 10_000,
                ticks: ticks(3),
                target: None,
            },
            FaultOperation::Kill(0, ticks(3)),
        ]
        .map(FaultAction::from);
        assert_eq!(WINDOWS.window(&parked, 0).unwrap(), (ROOT, ROOT + 6 * TICK));
        assert_eq!(
            WINDOWS.window(&parked, 1).unwrap(),
            (ROOT + 6 * TICK, ROOT + 9 * TICK)
        );
    }

    #[test]
    fn coverage_quantum_is_keyed_serialized_and_preserved_when_duration_changes() {
        let operation = FaultOperation::Wait(ticks(4));
        let action = FaultAction::new(operation, ticks(64));
        assert_ne!(
            action.key_bytes(),
            FaultAction::new(operation, ticks(1)).key_bytes()
        );
        assert_eq!(action.with_ticks(ticks(9)).coverage_quantum, ticks(64));
        let encoded = serde_json::to_string(&action).unwrap();
        assert_eq!(
            serde_json::from_str::<FaultAction>(&encoded).unwrap(),
            action
        );
        assert!(
            serde_json::from_str::<FaultAction>(r#"{"operation":{"Wait":4},"coverage_quantum":0}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<FaultAction>(r#"{"operation":{"Wait":4}}"#).is_err());
    }

    #[test]
    fn every_action_takes_a_recorded_duration() {
        let actions = [
            FaultOperation::Wait(ticks(9)),
            FaultOperation::Kill(1, ticks(9)),
            FaultOperation::EventKill {
                node: 1,
                rarity: 3,
                ticks: ticks(9),
            },
            FaultOperation::EventPark {
                node: 1,
                edges: 5,
                hold_us: 1,
                ticks: ticks(9),
                target: ParkTarget::new(8, 16),
            },
            FaultOperation::Pause(1, ticks(9)),
            FaultOperation::Restart(1, ticks(9)),
            FaultOperation::Hook(4, ticks(9)),
        ]
        .map(FaultAction::from);
        for action in actions {
            let adapted = action.with_ticks(ticks(12));
            assert_eq!(adapted.ticks(), 12);
            assert_eq!(action_ticks(&adapted), 12);
        }
        let parked = FaultAction::from(FaultOperation::PauseParked {
            node: 1,
            edges: 1,
            hold_us: 1,
            ticks: ticks(9),
            target: None,
        })
        .with_ticks(ticks(12));
        assert_eq!(parked.ticks(), 12);
        assert_eq!(action_ticks(&parked), 24);
        assert_eq!(parked.as_wait().operation, FaultOperation::Wait(ticks(24)));
    }

    #[test]
    fn a_shorter_window_shortens_an_event_park_hold_to_fit() {
        let park = FaultOperation::EventPark {
            node: 0,
            edges: 1,
            hold_us: 90_000,
            ticks: ticks(9),
            target: ParkTarget::new(8, 16),
        };
        assert_eq!(
            park.with_ticks(ticks(4)),
            FaultOperation::EventPark {
                node: 0,
                edges: 1,
                hold_us: 40_000,
                ticks: ticks(4),
                target: ParkTarget::new(8, 16),
            }
        );
        assert_eq!(
            park.with_ticks(ticks(12)),
            FaultOperation::EventPark {
                node: 0,
                edges: 1,
                hold_us: 90_000,
                ticks: ticks(12),
                target: ParkTarget::new(8, 16),
            }
        );
    }

    #[test]
    fn cumulative_windows_visit_each_duration_once_and_match_indexed_windows() {
        let actions: Vec<_> = (1..=4096)
            .map(|index| FaultAction::from(FaultOperation::Wait(ticks(index))))
            .collect();
        let visits = std::cell::Cell::new(0);
        TIMELINE_STEPS.with(|steps| steps.set(0));
        let rows = WINDOWS
            .iter(actions.iter().inspect(|_| visits.set(visits.get() + 1)))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(visits.get(), actions.len());
        assert_eq!(TIMELINE_STEPS.with(std::cell::Cell::get), actions.len());
        assert_eq!(rows.len(), actions.len());
        for (index, row) in rows.into_iter().enumerate() {
            let before = index as u64 * (index as u64 + 1) / 2;
            assert_eq!(
                row,
                (
                    ROOT + before * TICK,
                    ROOT + (before + index as u64 + 1) * TICK
                )
            );
            assert_eq!(row, WINDOWS.window(&actions, index).unwrap());
        }
    }

    #[test]
    fn cumulative_windows_check_each_end_and_allow_an_empty_maximum_root() {
        let action = FaultAction::from(FaultOperation::Wait(NonZeroU16::MIN));
        let windows = ActionWindows {
            root_seal: u64::MAX - TICK,
            hold_hooks: false,
        };
        let mut rows = windows.iter([&action, &action, &action]);
        assert_eq!(rows.next().unwrap().unwrap(), (u64::MAX - TICK, u64::MAX));
        assert_eq!(
            rows.next().unwrap().unwrap_err(),
            "action timeline overflows guest time"
        );
        assert!(rows.next().is_none());
        assert!(
            ActionWindows {
                root_seal: u64::MAX,
                hold_hooks: false,
            }
            .iter(&[])
            .next()
            .is_none()
        );
        assert!(
            ActionWindows {
                root_seal: u64::MAX,
                hold_hooks: false,
            }
            .iter([&action])
            .next()
            .unwrap()
            .is_err()
        );
    }

    #[test]
    fn an_overflowing_timeline_is_rejected() {
        let windows = ActionWindows {
            root_seal: u64::MAX - 1,
            hold_hooks: false,
        };
        assert!(windows.window(&kills(1), 0).is_err());
        assert!(WINDOWS.window(&[], 0).is_err());
        assert!(serde_json::from_str::<FaultAction>(r#"{"Wait":0}"#).is_err());
        assert!(serde_json::from_str::<FaultAction>(r#"{"Wait":65536}"#).is_err());
        assert!(serde_json::from_str::<FaultAction>(r#"{"Kill":[0,0]}"#).is_err());
    }

    #[test]
    fn short_and_long_waits_shift_later_faults_by_the_recorded_duration() {
        let actions = [
            FaultOperation::Wait(ticks(8)),
            FaultOperation::Kill(0, ticks(50)),
            FaultOperation::Wait(ticks(8192)),
            FaultOperation::Hook(1, ticks(50)),
        ]
        .map(FaultAction::from);
        assert_eq!(WINDOWS.window(&actions, 1).unwrap().0, ROOT + 8 * TICK);
        assert_eq!(WINDOWS.window(&actions, 3).unwrap().0, ROOT + 8250 * TICK);
        let encoded = serde_json::to_string(&actions).unwrap();
        let replay: Vec<FaultAction> = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            action_deltas(WINDOWS, &actions),
            action_deltas(WINDOWS, &replay)
        );
    }

    #[test]
    fn an_event_park_stands_for_its_window_with_its_own_hold() {
        let action = FaultOperation::EventPark {
            node: 2,
            edges: 4_096,
            hold_us: 2_000_000,
            ticks: ticks(500),
            target: None,
        };
        let window = WINDOWS.window(&[action.into()], 0).unwrap();
        let fault = WINDOWS
            .delta(action.into(), window)
            .standing
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(fault.start, ROOT);
        assert_eq!(fault.end, ROOT + 500 * TICK);
        assert_eq!(
            decode_process_target(&fault.target),
            Some((
                2,
                Fault::ProcEventPark {
                    edges: 4_096,
                    hold: Span(2_000_000_000),
                    target: None,
                    at_resume: false,
                }
            ))
        );
        let aimed = FaultOperation::EventPark {
            node: 2,
            edges: 4_096,
            hold_us: 2_000_000,
            ticks: ticks(500),
            target: ParkTarget::new(0x1000, 0x1004),
        };
        let fault = WINDOWS
            .delta(aimed.into(), window)
            .standing
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(
            decode_process_target(&fault.target),
            Some((
                2,
                Fault::ProcEventPark {
                    edges: 4_096,
                    hold: Span(2_000_000_000),
                    target: ParkTarget::new(0x1000, 0x1004),
                    at_resume: false,
                }
            ))
        );
        let parked = FaultAction::from(FaultOperation::PauseParked {
            node: 2,
            edges: 1,
            hold_us: 2_000_000,
            ticks: ticks(250),
            target: ParkTarget::new(0x1000, 0x1001),
        });
        assert_eq!(WINDOWS.window(&[parked], 0).unwrap(), window);
        let standing = action_delta(parked, window, false).standing;
        assert_eq!(
            standing
                .iter()
                .map(|fault| (fault.start, fault.end, decode_process_target(&fault.target)))
                .collect::<Vec<_>>(),
            [
                (
                    ROOT,
                    ROOT + 250 * TICK,
                    Some((2, Fault::ProcPause(Span(250 * TICK))))
                ),
                (
                    ROOT + 250 * TICK,
                    ROOT + 500 * TICK,
                    Some((
                        2,
                        Fault::ProcEventPark {
                            edges: 1,
                            hold: Span(2_000_000_000),
                            target: ParkTarget::new(0x1000, 0x1001),
                            at_resume: true,
                        }
                    ))
                ),
            ]
        );
    }

    #[test]
    fn a_parked_pause_is_its_own_action_in_keys_text_and_binary() {
        let target = ParkTarget::new(8, 16);
        let park = FaultOperation::EventPark {
            node: 1,
            edges: 3,
            hold_us: 90_000,
            ticks: ticks(9),
            target,
        };
        let parked = FaultOperation::PauseParked {
            node: 1,
            edges: 3,
            hold_us: 90_000,
            ticks: ticks(9),
            target,
        };
        assert_ne!(park.key_bytes(), parked.key_bytes());
        assert_ne!(
            FaultOperation::Pause(1, ticks(9)).key_bytes(),
            parked.key_bytes()
        );
        assert_eq!(parked.key_bytes()[0], 16);
        assert_eq!(
            parked.with_ticks(ticks(4)),
            FaultOperation::PauseParked {
                node: 1,
                edges: 3,
                hold_us: 40_000,
                ticks: ticks(4),
                target,
            }
        );
        assert_eq!(parked.target_node(), Some(1));
        assert_eq!(
            parked.with_target_node(2),
            FaultOperation::PauseParked {
                node: 2,
                edges: 3,
                hold_us: 90_000,
                ticks: ticks(9),
                target,
            }
        );
        let text = r#"{"operation":{"PauseParked":{"node":1,"edges":3,"hold_us":90000,"ticks":9,"target":{"start":8,"end":16}}},"coverage_quantum":1,"choice":0}"#;
        let action: FaultAction = serde_json::from_str(text).unwrap();
        assert_eq!(action.operation, parked);
        assert_eq!(serde_json::to_string(&action).unwrap(), text);
        assert!(
            serde_json::from_str::<FaultAction>(&text.replace("PauseParked", "EventParkFromArm"))
                .is_err()
        );
        let bytes = postcard::to_allocvec(&(action, 0x55_u8)).unwrap();
        assert_eq!(bytes[0], 16);
        assert_eq!(
            postcard::from_bytes::<(FaultAction, u8)>(&bytes).unwrap(),
            (action, 0x55)
        );
        assert_eq!(
            postcard::to_allocvec(&FaultAction::from(park)).unwrap()[0],
            9
        );
    }

    #[test]
    fn a_choice_changes_the_action_key_and_entropy_seed() {
        let action = FaultAction::from(FaultOperation::Wait(ticks(4)));
        assert_eq!(action.entropy_seed(11), 11);
        let first = action.with_choice(1);
        let second = action.with_choice(2);
        assert_ne!(first.entropy_seed(11), 11);
        assert_ne!(first.entropy_seed(11), second.entropy_seed(11));
        assert_eq!(
            first.entropy_seed(11),
            action.with_choice(1).entropy_seed(11)
        );
        assert_ne!(first.key_bytes(), second.key_bytes());
        assert_eq!(first.with_ticks(ticks(9)).choice, 1);
    }

    #[test]
    fn guest_choices_are_omitted_from_text_only_when_absent_and_always_encoded_in_binary() {
        let plain = FaultAction::from(FaultOperation::Wait(ticks(4))).with_choice(3);
        let text = serde_json::to_value(plain).unwrap();
        assert!(text.get("guest").is_none());
        assert_eq!(serde_json::from_value::<FaultAction>(text).unwrap(), plain);
        let guest = plain.with_guest(crate::guest::GuestChoices {
            contrary: 7,
            hold: 9,
            seed: 11,
            untried: Some(1),
        });
        let text = serde_json::to_value(guest).unwrap();
        assert_eq!(text["guest"]["seed"], 11);
        assert_eq!(serde_json::from_value::<FaultAction>(text).unwrap(), guest);
        for action in [plain, guest] {
            let bytes = postcard::to_allocvec(&(action, 0x55_u8)).unwrap();
            assert_eq!(
                postcard::from_bytes::<(FaultAction, u8)>(&bytes).unwrap(),
                (action, 0x55)
            );
        }
        assert_ne!(
            postcard::to_allocvec(&plain).unwrap(),
            postcard::to_allocvec(&guest).unwrap()
        );
    }

    #[test]
    fn an_event_park_target_is_a_nonempty_range_in_json() {
        let untargeted = r#"{"operation":{"EventPark":{"node":0,"edges":1,"hold_us":5,"ticks":1,"target":null}},"coverage_quantum":1,"choice":7}"#;
        let action: FaultAction = serde_json::from_str(untargeted).unwrap();
        assert_eq!(serde_json::to_string(&action).unwrap(), untargeted);
        let targeted = r#"{"operation":{"EventPark":{"node":0,"edges":1,"hold_us":5,"ticks":1,"target":{"start":8,"end":16}}},"coverage_quantum":1,"choice":0}"#;
        let action: FaultAction = serde_json::from_str(targeted).unwrap();
        assert!(matches!(
            action.operation,
            FaultOperation::EventPark { target: Some(target), .. } if target == ParkTarget::new(8, 16).unwrap()
        ));
        assert_eq!(serde_json::to_string(&action).unwrap(), targeted);
        let empty = r#"{"operation":{"EventPark":{"node":0,"edges":1,"hold_us":5,"ticks":1,"target":{"start":8,"end":8}}},"coverage_quantum":1}"#;
        assert!(serde_json::from_str::<FaultAction>(empty).is_err());
    }

    #[test]
    fn event_park_snapshots_round_trip_in_binary_checkpoints() {
        use searcher::search::campaign::{SnapshotCheckpoint, SnapshotCheckpointEntry};
        use sha2::{Digest, Sha256};

        for target in [None, ParkTarget::new(8, 16)] {
            let snapshot = FaultSnapshot {
                history: crate::history::FaultHistory::fixture(
                    &[FaultAction::new(
                        FaultOperation::EventPark {
                            node: 1,
                            edges: 4096,
                            hold_us: 1000,
                            ticks: NonZeroU16::new(10).unwrap(),
                            target,
                        },
                        NonZeroU16::new(256).unwrap(),
                    )],
                    &[Some([17; 32])],
                    Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
                    &(FaultObservations::default()),
                ),
                ..FaultSnapshot::default()
            };
            let bytes = postcard::to_allocvec(&snapshot).unwrap();
            assert_eq!(
                postcard::from_bytes::<FaultSnapshot>(&bytes).unwrap(),
                snapshot
            );
            let checkpoint = SnapshotCheckpoint {
                format: "fault-test".to_owned(),
                entries: vec![SnapshotCheckpointEntry {
                    id: 1,
                    snapshot: snapshot.clone(),
                }],
            };
            let checkpoint_bytes = checkpoint.to_bytes().unwrap();
            let restored =
                SnapshotCheckpoint::<FaultSnapshot>::from_bytes(&checkpoint_bytes, "fault-test")
                    .unwrap();
            assert_eq!(
                Sha256::digest(&bytes),
                Sha256::digest(postcard::to_allocvec(&restored.entries[0].snapshot).unwrap())
            );
        }
    }

    #[test]
    fn wait_installs_nothing() {
        assert_eq!(
            WINDOWS.delta(
                FaultOperation::Wait(NonZeroU16::MIN).into(),
                WINDOWS.window(&kills(4), 0).unwrap()
            ),
            ActionDelta::default()
        );
        assert!(
            standing_windows(
                WINDOWS,
                &[],
                &[
                    FaultOperation::Wait(NonZeroU16::MIN).into(),
                    FaultOperation::Wait(NonZeroU16::MIN).into()
                ]
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn kill_holds_its_node_down_for_its_window_and_brings_it_back() {
        let window = WINDOWS.window(&kills(4), 1).unwrap();
        let delta = WINDOWS.delta(FaultOperation::Kill(2, ticks(50)).into(), window);
        assert_eq!(delta.standing.len(), 1);
        let fault = &delta.standing[0];
        assert_eq!(fault.class, DecisionClass::Process.as_u16());
        assert_eq!(
            decode_process_target(&fault.target),
            Some((2, Fault::ProcRestart))
        );
        assert_eq!((fault.start, fault.end), window);
    }

    #[test]
    fn a_whole_cluster_kill_holds_every_node_down_for_its_window() {
        let window = WINDOWS.window(&kills(4), 1).unwrap();
        let delta = WINDOWS.delta(FaultOperation::KillAll(ticks(50)).into(), window);
        assert_eq!(delta.standing.len(), 1);
        let fault = &delta.standing[0];
        assert_eq!(
            decode_process_target(&fault.target),
            Some((EVERY_NODE, Fault::ProcRestart))
        );
        assert_eq!((fault.start, fault.end), window);
        assert_ne!(
            FaultOperation::KillAll(ticks(50)).key_bytes(),
            FaultOperation::Kill(0, ticks(50)).key_bytes()
        );
    }

    #[test]
    fn a_power_loss_carries_its_choice_and_brings_the_node_back_at_window_end() {
        let window = WINDOWS.window(&kills(4), 1).unwrap();
        for (operation, node) in [
            (FaultOperation::PowerLoss(2, ticks(50)), 2),
            (FaultOperation::PowerLossAll(ticks(50)), EVERY_NODE),
        ] {
            let action = FaultAction::from(operation).with_choice(77);
            let delta = WINDOWS.delta(action, window);
            assert_eq!(
                delta
                    .standing
                    .iter()
                    .map(|fault| (decode_process_target(&fault.target), fault.start, fault.end))
                    .collect::<Vec<_>>(),
                [
                    (Some((node, Fault::ProcRestart)), window.0, window.1),
                    (
                        Some((node, Fault::ProcPowerLoss { choice: 77 })),
                        window.0,
                        window.1
                    ),
                ]
            );
        }
        assert_ne!(
            FaultOperation::PowerLoss(0, ticks(50)).key_bytes(),
            FaultOperation::Kill(0, ticks(50)).key_bytes()
        );
        assert_ne!(
            FaultOperation::PowerLossAll(ticks(50)).key_bytes(),
            FaultOperation::KillAll(ticks(50)).key_bytes()
        );
    }

    #[test]
    fn disk_faults_reach_the_supervisor_with_their_windows() {
        let window = WINDOWS.window(&kills(4), 1).unwrap();
        let horizon = window.1 - window.0;
        for (operation, fault, end) in [
            (
                FaultOperation::DiskError(1, ticks(50)),
                Fault::ProcDiskError,
                window.1,
            ),
            (
                FaultOperation::DiskStall(1, ticks(50)),
                Fault::ProcDiskStall(Span(horizon)),
                window.1,
            ),
            (
                FaultOperation::DiskCorrupt(1, ticks(50)),
                Fault::ProcDiskCorrupt { choice: 77 },
                window.0 + horizon / RESTART_DOWN_DIVISOR,
            ),
        ] {
            let action = FaultAction::from(operation).with_choice(77);
            let standing = WINDOWS
                .delta(action, window)
                .standing
                .into_iter()
                .next()
                .expect("a disk fault installs a standing fault");
            assert_eq!(decode_process_target(&standing.target), Some((1, fault)));
            assert_eq!((standing.start, standing.end), (window.0, end));
            assert_eq!(operation.with_ticks(ticks(9)).ticks(), 9);
        }
        let keys = [
            FaultOperation::DiskError(0, ticks(50)),
            FaultOperation::DiskStall(0, ticks(50)),
            FaultOperation::DiskCorrupt(0, ticks(50)),
            FaultOperation::PowerLoss(0, ticks(50)),
            FaultOperation::Restart(0, ticks(50)),
        ]
        .map(|operation| operation.key_bytes());
        assert_eq!(keys.iter().collect::<BTreeSet<_>>().len(), keys.len());
    }

    #[test]
    fn a_clock_shift_covers_its_window_and_spreads_only_across_every_node() {
        let window = WINDOWS.window(&kills(4), 1).unwrap();
        for (operation, node, spread) in [
            (
                FaultOperation::Clock {
                    node: 2,
                    offset_ms: -1_500,
                    rate_ppm: 250_000,
                    ticks: ticks(50),
                },
                2,
                0,
            ),
            (
                FaultOperation::ClockAll {
                    offset_ms: -1_500,
                    rate_ppm: 250_000,
                    ticks: ticks(50),
                },
                EVERY_NODE,
                77,
            ),
        ] {
            let action = FaultAction::from(operation).with_choice(77);
            let fault = WINDOWS
                .delta(action, window)
                .standing
                .into_iter()
                .next()
                .expect("a clock shift installs a standing fault");
            assert_eq!(
                decode_process_target(&fault.target),
                Some((
                    node,
                    Fault::ProcClock {
                        offset: -1_500_000_000,
                        rate_ppm: 250_000,
                        spread,
                    }
                ))
            );
            assert_eq!((fault.start, fault.end), window);
            assert_eq!(action.ticks(), 50);
            assert_eq!(action.with_ticks(ticks(9)).ticks(), 9);
        }
        let shift = |offset_ms, rate_ppm| FaultOperation::Clock {
            node: 0,
            offset_ms,
            rate_ppm,
            ticks: ticks(50),
        };
        let keys = [
            shift(1, 0).key_bytes(),
            shift(-1, 0).key_bytes(),
            shift(0, 1).key_bytes(),
            FaultOperation::ClockAll {
                offset_ms: 1,
                rate_ppm: 0,
                ticks: ticks(50),
            }
            .key_bytes(),
            FaultOperation::Kill(0, ticks(50)).key_bytes(),
        ];
        assert_eq!(keys.iter().collect::<BTreeSet<_>>().len(), keys.len());
    }

    #[test]
    fn pause_holds_its_node_for_its_recorded_duration() {
        for duration in [1_u16, 7, 1_024] {
            let actions = [FaultAction::from(FaultOperation::Pause(1, ticks(duration)))];
            let (start, end) = WINDOWS.window(&actions, 0).unwrap();
            let fault = WINDOWS
                .delta(actions[0], (start, end))
                .standing
                .into_iter()
                .next()
                .expect("pause installs a standing fault");
            assert_eq!(
                decode_process_target(&fault.target),
                Some((1, Fault::ProcPause(Span(u64::from(duration) * TICK))))
            );
            assert_eq!((fault.start, fault.end), (start, end));
        }
    }

    #[test]
    fn restart_brings_the_node_back_inside_its_window() {
        for duration in [1_u16, 4, 50, 1_024] {
            let actions = [FaultAction::from(FaultOperation::Restart(
                0,
                ticks(duration),
            ))];
            let (start, end) = WINDOWS.window(&actions, 0).unwrap();
            let fault = WINDOWS
                .delta(actions[0], (start, end))
                .standing
                .into_iter()
                .next()
                .expect("restart installs a standing fault");
            assert_eq!(
                decode_process_target(&fault.target),
                Some((0, Fault::ProcRestart))
            );
            assert_eq!(
                fault.end,
                start + (u64::from(duration) * TICK / 4).max(TICK)
            );
            assert!(fault.end <= end);
        }
    }

    #[test]
    fn hook_targets_the_supervisor_rather_than_a_node() {
        let fault = WINDOWS
            .delta(
                FaultOperation::Hook(9, ticks(50)).into(),
                WINDOWS.window(&kills(4), 0).unwrap(),
            )
            .standing
            .into_iter()
            .next()
            .expect("hook installs a standing fault");
        assert_eq!(
            decode_process_target(&fault.target),
            Some((0, Fault::RunHook(9)))
        );
    }

    fn hook_with_hold(id: u32, duration: u16, hold_bits: u64) -> FaultAction {
        FaultAction::from(FaultOperation::Hook(id, ticks(duration)))
            .with_choice((hold_bits << HOOK_HOLD_CHOICE_SHIFT) | 0x1234)
    }

    fn spans_of(windows: ActionWindows, actions: &[FaultAction]) -> Vec<(u32, u64, u64)> {
        standing_windows(windows, &[], actions)
            .unwrap()
            .into_iter()
            .filter_map(|fault| match decode_process_target(&fault.target) {
                Some((0, Fault::RunHook(id))) => Some((id, fault.start, fault.end)),
                _ => None,
            })
            .collect()
    }

    fn hook_spans(actions: &[FaultAction]) -> Vec<(u32, u64, u64)> {
        spans_of(HELD, actions)
    }

    #[test]
    fn without_the_hold_a_hook_window_is_its_action_whatever_its_choice() {
        let wait = FaultAction::from(FaultOperation::Wait(ticks(50)));
        for bits in 0..4 {
            let mut actions = vec![hook_with_hold(1, 50, bits)];
            actions.extend([wait; 8]);
            assert_eq!(spans_of(WINDOWS, &actions), [(1, ROOT, ROOT + 50 * TICK)]);
            assert_eq!(
                standing_windows(WINDOWS, &[], &actions),
                standing_windows(WINDOWS, &[], &[hook_with_hold(1, 50, 0)])
            );
        }
    }

    #[test]
    fn a_cut_hook_stays_in_force_over_the_waits_after_it() {
        let wait = FaultAction::from(FaultOperation::Wait(ticks(50)));
        for (bits, steps) in [(0_u64, 1_u64), (1, 2), (2, 4), (3, 8)] {
            let mut actions = vec![hook_with_hold(1, 50, bits)];
            actions.extend([wait; 8]);
            let covered = usize::try_from(steps).unwrap() - 1;
            let (start, _) = WINDOWS.window(&actions, 0).unwrap();
            let (_, last_covered) = WINDOWS.window(&actions, covered).unwrap();
            assert_eq!(hook_spans(&actions), [(1, start, last_covered)]);
            assert_eq!(last_covered, start + steps * 50 * TICK);
            assert_eq!(WINDOWS.window(&actions, 1).unwrap().0, start + 50 * TICK);
        }
    }

    #[test]
    fn the_longest_cut_lasts_eight_of_the_longest_actions() {
        let spans = hook_spans(&[hook_with_hold(1, 1024, 3)]);
        assert_eq!(spans.len(), 1);
        let (_, start, end) = spans[0];
        assert_eq!(end - start, 8 * 1024 * TICK);
        assert_eq!(end - start, 81_920_000_000);
    }

    #[test]
    fn a_heal_drawn_inside_a_held_cut_opens_at_its_own_step() {
        let actions = [
            hook_with_hold(1, 50, 3),
            FaultOperation::Wait(ticks(50)).into(),
            hook_with_hold(4, 50, 0),
        ];
        let heal = WINDOWS.window(&actions, 2).unwrap();
        assert_eq!(
            hook_spans(&actions),
            [(1, ROOT, ROOT + 8 * 50 * TICK), (4, heal.0, heal.1)]
        );
        assert!(ROOT < heal.0 && heal.1 < ROOT + 8 * 50 * TICK);
    }

    #[test]
    fn consecutive_held_cuts_overlap() {
        let actions = [hook_with_hold(1, 50, 1), hook_with_hold(2, 50, 1)];
        assert_eq!(
            hook_spans(&actions),
            [
                (1, ROOT, ROOT + 100 * TICK),
                (2, ROOT + 50 * TICK, ROOT + 150 * TICK),
            ]
        );
    }

    #[test]
    fn a_recorded_hook_replays_with_its_hold() {
        let actions = [
            hook_with_hold(1, 64, 2),
            FaultOperation::Kill(0, ticks(64)).into(),
            hook_with_hold(3, 64, 1),
        ];
        let recorded =
            crate::package::parse_recorded_input(&serde_json::to_string(&actions).unwrap())
                .unwrap()
                .actions;
        assert_eq!(recorded, actions);
        assert_eq!(
            standing_windows(HELD, &[], &recorded),
            standing_windows(HELD, &[], &actions)
        );
        assert_eq!(hook_spans(&recorded)[0], (1, ROOT, ROOT + 4 * 64 * TICK));
        let mut other = actions;
        other[0] = hook_with_hold(1, 64, 0);
        assert_ne!(hook_spans(&actions), hook_spans(&other));
        assert_eq!(
            fault_policy::decode_windows(
                &fault_policy::encode_windows(&standing_windows(HELD, &[], &actions).unwrap())
                    .unwrap()
            )
            .unwrap(),
            standing_windows(HELD, &[], &actions).unwrap()
        );
    }

    #[test]
    fn an_input_installs_one_standing_fault_per_faulting_action() {
        let actions = [
            FaultOperation::Hook(1, ticks(50)),
            FaultOperation::Wait(NonZeroU16::MIN),
            FaultOperation::Kill(0, ticks(50)),
        ]
        .map(FaultAction::from);
        let faults = standing_windows(WINDOWS, &[], &actions).unwrap();
        assert_eq!(faults.len(), 2);
        assert_eq!(
            (faults[0].start, faults[0].end),
            WINDOWS.window(&actions, 0).unwrap()
        );
        assert_eq!(
            (faults[1].start, faults[1].end),
            WINDOWS.window(&actions, 2).unwrap()
        );
        assert!(
            faults
                .iter()
                .all(|fault| fault.class == DecisionClass::Process.as_u16())
        );
    }

    #[test]
    fn ending_faults_closes_every_window_still_standing_at_its_start() {
        let inherited = standing_windows(
            ActionWindows {
                root_seal: 0,
                hold_hooks: false,
            },
            &[],
            &[FaultOperation::EventKill {
                node: 2,
                rarity: 9,
                ticks: ticks(4),
            }
            .into()],
        )
        .unwrap();
        let actions = [
            FaultOperation::EventKill {
                node: 0,
                rarity: 3,
                ticks: ticks(50),
            },
            FaultOperation::Pause(1, ticks(50)),
            FaultOperation::EndFaults(ticks(16)),
            FaultOperation::Kill(1, ticks(50)),
        ]
        .map(FaultAction::from);
        let end = WINDOWS.window(&actions, 2).unwrap().0;
        let open = standing_windows(WINDOWS, &inherited, &actions[..2]).unwrap();
        assert_eq!(
            open.iter().filter(|window| window.end == u64::MAX).count(),
            2
        );
        let ended = standing_windows(WINDOWS, &inherited, &actions).unwrap();
        assert_eq!(ended.len(), 4);
        assert_eq!(ended[0].end, end);
        assert_eq!(ended[1].end, end);
        assert_eq!(
            (ended[2].start, ended[2].end),
            WINDOWS.window(&actions, 1).unwrap()
        );
        assert_eq!(
            (ended[3].start, ended[3].end),
            WINDOWS.window(&actions, 3).unwrap()
        );
        assert_eq!(
            FaultOperation::EndFaults(ticks(16)).ticks(),
            16,
            "ending faults advances the timeline like a wait"
        );
    }

    #[test]
    fn the_window_list_round_trips_through_the_shared_codec() {
        let actions = [
            FaultOperation::Kill(1, ticks(50)),
            FaultOperation::Hook(2, ticks(50)),
        ]
        .map(FaultAction::from);
        let windows = standing_windows(WINDOWS, &[], &actions).unwrap();
        let bytes = fault_policy::encode_windows(&windows).expect("encode");
        assert_eq!(
            fault_policy::decode_windows(&bytes).expect("decode"),
            windows
        );
    }

    #[test]
    fn the_window_list_is_a_function_of_the_actions_and_the_tiling() {
        let actions = [
            FaultOperation::Restart(3, ticks(50)),
            FaultOperation::Wait(NonZeroU16::MIN),
        ]
        .map(FaultAction::from);
        assert_eq!(
            standing_windows(WINDOWS, &[], &actions),
            standing_windows(WINDOWS, &[], &actions)
        );
        let moved = ActionWindows {
            root_seal: 2_000,
            hold_hooks: false,
        };
        assert_ne!(
            standing_windows(WINDOWS, &[], &actions),
            standing_windows(moved, &[], &actions)
        );
        let shorter = [
            FaultOperation::Restart(3, ticks(10)),
            FaultOperation::Wait(NonZeroU16::MIN),
        ]
        .map(FaultAction::from);
        assert_ne!(
            standing_windows(WINDOWS, &[], &actions),
            standing_windows(WINDOWS, &[], &shorter)
        );
    }

    #[test]
    fn a_reported_runtime_failure_is_not_bug_evidence() {
        let capture =
            decode_sdk_events(&[state_event(reg::INFRASTRUCTURE_ERROR, STATE_SET, 1)]).unwrap();
        assert!(capture.assertions.violations().is_empty());
        assert!(capture.check_infrastructure_status().is_err());
        assert!(SdkCapture::default().check_infrastructure_status().is_ok());
    }

    #[test]
    fn sometimes_hits_survive_decoding() {
        let capture = decode_sdk_events(&[
            assert_event(3, DISP_HIT),
            assert_event(3, DISP_HIT),
            assert_event(70, DISP_HIT),
            assert_event(5, DISP_VIOLATION),
            state_event(reg::HOOKS_FINISHED, STATE_SET, 4),
        ])
        .expect("decode");
        assert_eq!(capture.assertions.key_ids(), BTreeSet::from(["3", "70"]));
        assert_eq!(
            capture.assertions.violations(),
            BTreeSet::from(["5".to_owned()])
        );
        assert_eq!(capture.registers.get(&reg::HOOKS_FINISHED), Some(&4));
    }

    #[test]
    fn state_registers_take_the_last_set_and_the_greatest_max() {
        let capture = decode_sdk_events(&[
            state_event(reg::TICKS, STATE_SET, 9),
            state_event(reg::TICKS, STATE_SET, 3),
            state_event(reg::RESTARTS, STATE_MAX, 2),
            state_event(reg::RESTARTS, STATE_MAX, 7),
            state_event(reg::RESTARTS, STATE_MAX, 4),
        ])
        .expect("decode");
        assert_eq!(capture.registers.get(&reg::TICKS), Some(&3));
        assert_eq!(capture.registers.get(&reg::RESTARTS), Some(&7));
    }

    fn catalog_event(points: &[(u8, u32, &str)]) -> (u64, u32, Vec<u8>) {
        let mut bytes = CATALOG_MAGIC.to_vec();
        bytes.push(CATALOG_VERSION);
        bytes.extend_from_slice(&u32::try_from(points.len()).unwrap().to_le_bytes());
        for (kind, id, name) in points {
            bytes.push(*kind);
            bytes.extend_from_slice(&id.to_le_bytes());
            bytes.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
            bytes.extend_from_slice(name.as_bytes());
        }
        (0, JSON_EVENT_ID, bytes)
    }

    #[test]
    fn catalog_declared_binary_assertions_keep_their_kind_and_report_unmet_ones() {
        let catalog = catalog_event(&[
            (KIND_SOMETIMES, 1, "quiet"),
            (KIND_SOMETIMES, 2, "fired"),
            (KIND_REACHABLE, 3, "reached"),
            (KIND_UNREACHABLE, 4, "forbidden"),
            (KIND_REACHABLE, 5, "missed"),
            (KIND_BUGGIFY, 6, "coin"),
            (KIND_STATE, 7, "counter"),
        ]);
        let capture = decode_sdk_events(&[
            catalog,
            assert_event(2, DISP_HIT),
            assert_event(3, DISP_HIT),
            assert_event(4, DISP_VIOLATION),
        ])
        .expect("decode");
        let assertions = &capture.assertions;
        assert_eq!(
            assertions.never_satisfied(),
            BTreeSet::from(["1".to_owned(), "5".to_owned()])
        );
        assert_eq!(assertions.0["1"].message, "quiet");
        assert_eq!(assertions.0["3"].kind, AssertionKind::Reachable);
        assert_eq!(assertions.0["4"].kind, AssertionKind::Unreachable);
        assert_eq!(assertions.violations(), BTreeSet::from(["4".to_owned()]));
        assert!(!assertions.0.contains_key("6") && !assertions.0.contains_key("7"));
        assert_eq!(assertions.key_ids(), BTreeSet::from(["2", "3"]));
    }

    #[test]
    fn an_undeclared_binary_assertion_takes_its_kind_from_the_disposition() {
        let capture =
            decode_sdk_events(&[assert_event(8, DISP_HIT), assert_event(9, DISP_VIOLATION)])
                .expect("decode");
        assert_eq!(capture.assertions.0["8"].kind, AssertionKind::Sometimes);
        assert_eq!(capture.assertions.0["9"].kind, AssertionKind::Always);
    }

    #[test]
    fn a_malformed_catalog_is_loud() {
        let (moment, id, mut bytes) = catalog_event(&[(KIND_SOMETIMES, 1, "x")]);
        bytes.pop();
        assert!(decode_sdk_events(&[(moment, id, bytes)]).is_err());
        assert!(decode_sdk_events(&[catalog_event(&[(9, 1, "x")])]).is_err());
    }

    #[test]
    fn a_malformed_event_is_ignored_and_a_bad_operation_is_loud() {
        let short = decode_sdk_events(&[
            (0, (u32::from(NS_ASSERT) << NS_SHIFT) | 1, vec![0]),
            (0, (u32::from(NS_STATE) << NS_SHIFT) | 1, vec![0, 1]),
            (0, 0, vec![1, 2, 3]),
        ])
        .expect("decode");
        assert_eq!(short, SdkCapture::default());
        assert!(decode_sdk_events(&[state_event(1, 9, 0)]).is_err());
        assert!(decode_sdk_events(&[assert_event(1, 9)]).is_err());
    }

    #[test]
    fn observations_project_the_registers_and_the_sometimes_set() {
        let capture = decode_sdk_events(&[
            assert_event(0, DISP_HIT),
            assert_event(63, DISP_HIT),
            assert_event(64, DISP_HIT),
            state_event(reg::ALIVE, STATE_SET, 0b101),
            state_event(reg::HOOKS_FINISHED, STATE_SET, 2),
            state_event(reg::EVENT_PARK_HELD, STATE_SET, 0b100),
            state_event(reg::EVENT_KILL_ARMED, STATE_SET, 0b001),
        ])
        .expect("decode");
        let observations = FaultObservations::new(77, &capture, FaultStop::Deadline);
        assert_eq!(observations.moment, 77);
        assert_eq!(observations.alive, 0b101);
        assert_eq!(observations.hooks_finished, 2);
        assert_eq!(observations.event_park_held, 0b100);
        assert_eq!(observations.event_kill_armed, 0b001);
        assert_eq!(
            observations.sometimes(),
            BTreeSet::from(["0".to_owned(), "63".to_owned(), "64".to_owned()])
        );
        assert!(!observations.is_bug());
        assert_eq!(observations.exit_kind(), ExitKind::Ok);
    }

    fn json_pass(pid: u64, id: &str) -> (u64, u32, Vec<u8>) {
        (
            0,
            JSON_EVENT_ID,
            format!(
                r#"{{"harmony_attribution":{{"rip":"0x1","pid":{pid},"comm_hex":"61"}},"antithesis_assert":{{"hit":true,"must_hit":true,"assert_type":"reachability","message":"{id}","condition":true,"id":"{id}"}}}}"#
            )
            .into_bytes(),
        )
    }

    fn completion(pid: u64, run: u64, generation: u64) -> Vec<(u64, u32, Vec<u8>)> {
        vec![
            state_event(reg::COMPLETED_CHECK_PID, STATE_SET, pid),
            state_event(reg::COMPLETED_CHECK_START_GENERATION, STATE_SET, generation),
            state_event(reg::COMPLETED_CHECK_END_GENERATION, STATE_SET, generation),
            state_event(reg::COMPLETED_CHECK_RUN, STATE_SET, run),
        ]
    }

    #[test]
    fn a_supervisor_node_exit_record_is_a_violation() {
        let capture = decode_sdk_events(&[(
            5,
            JSON_EVENT_ID,
            br#"{"antithesis_assert":{"assert_type":"always","display_type":"AlwaysOrUnreachable","id":"workload node ends only by a fault the search injected","message":"workload node ends only by a fault the search injected","hit":true,"must_hit":false,"condition":false,"location":{"file":"harmony-supervisor","function":"node exit","class":"","begin_line":0,"begin_column":0},"details":{"node":1,"exit":"signal 11"}}}
"#
            .to_vec(),
        )])
        .unwrap();
        assert_eq!(
            capture.assertions.violations(),
            BTreeSet::from(["workload node ends only by a fault the search injected".to_owned()])
        );
    }

    #[test]
    fn park_reports_become_landings_and_reads_with_their_moment() {
        let capture = decode_sdk_events(&[
            (
                41,
                JSON_EVENT_ID,
                br#"{"harmony_attribution":{"rip":"0x1","pid":7,"comm_hex":"61"},"harmony_park":{"site":913,"edges":4096}}
"#
                .to_vec(),
            ),
            (
                52,
                JSON_EVENT_ID,
                br#"{"harmony_attribution":{"rip":"0x1","pid":7,"comm_hex":"61"},"harmony_park_read":{"site":913,"edges":3}}
"#
                .to_vec(),
            ),
        ])
        .unwrap();
        assert_eq!(
            capture.parks,
            vec![ParkLanding {
                moment: 41,
                site: 913,
                edges: 4096,
            }]
        );
        assert_eq!(
            capture.park_reads,
            vec![ParkRead {
                moment: 52,
                site: 913,
                edges: 3,
            }]
        );
        assert!(capture.assertions.0.is_empty());
    }

    #[test]
    fn completed_check_provenance_is_the_checks_own_passes() {
        let mut events = vec![
            json_pass(9, "elsewhere"),
            state_event(reg::CHECK_ENABLED, STATE_SET, 1),
            state_event(reg::CHECKS_STARTED, STATE_SET, 1),
            json_pass(40, "checked"),
            json_pass(41, "hook"),
            state_event(reg::CHECKS_STARTED, STATE_SET, 1),
        ];
        events.extend(completion(40, 7, 2));
        events.extend([
            state_event(reg::DISTURBANCE_GENERATION, STATE_SET, 3),
            state_event(reg::PENDING_FAULTS, STATE_SET, 1),
        ]);
        let capture = decode_sdk_events(&events).unwrap();
        let observation = FaultObservations::new(77, &capture, FaultStop::Deadline);
        assert!(observation.sometimes().contains("elsewhere"));
        assert_eq!(
            observation.check,
            Some(CheckEvidence {
                disturbance_generation: 3,
                run: 7,
                start_generation: 2,
                end_generation: 2,
                points: vec!["checked".to_owned()],
                undecided_keys: 0,
                pending_faults: 1,
            })
        );
        assert!(FaultObservations::default().check.is_none());
    }

    fn json_undecided(pid: u64, keys: u64) -> (u64, u32, Vec<u8>) {
        (
            0,
            JSON_EVENT_ID,
            format!(
                r#"{{"harmony_attribution":{{"rip":"0x1","pid":{pid},"comm_hex":"61"}},"harmony_check":{{"undecided_keys":{keys}}}}}"#
            )
            .into_bytes(),
        )
    }

    #[test]
    fn a_completed_check_carries_its_own_undecided_keys() {
        let mut events = vec![
            state_event(reg::CHECK_ENABLED, STATE_SET, 1),
            state_event(reg::CHECKS_STARTED, STATE_SET, 1),
            json_pass(40, "checked"),
            json_undecided(40, 3),
            json_undecided(41, 9),
        ];
        events.extend(completion(40, 1, 1));
        let check = |events: &[(u64, u32, Vec<u8>)]| {
            let capture = decode_sdk_events(events).unwrap();
            assert_eq!(capture, decode_sdk_events_reference(events).unwrap());
            FaultObservations::new(1, &capture, FaultStop::Deadline)
                .check
                .unwrap()
        };
        assert_eq!(check(&events).undecided_keys, 3);
        events.extend([
            state_event(reg::CHECKS_STARTED, STATE_SET, 2),
            json_pass(40, "checked"),
        ]);
        events.extend(completion(40, 2, 1));
        let next = check(&events);
        assert_eq!((next.run, next.undecided_keys), (2, 0));
    }

    #[test]
    fn a_fully_undecided_check_still_reports_its_undecided_keys() {
        let mut events = vec![
            state_event(reg::CHECK_ENABLED, STATE_SET, 1),
            state_event(reg::CHECKS_STARTED, STATE_SET, 1),
            json_pass(40, "checked"),
        ];
        events.extend(completion(40, 1, 1));
        events.extend([
            state_event(reg::CHECKS_STARTED, STATE_SET, 2),
            json_undecided(40, 3),
        ]);
        events.extend(completion(40, 2, 2));
        let capture = decode_sdk_events(&events).unwrap();
        assert_eq!(capture, decode_sdk_events_reference(&events).unwrap());
        let check = FaultObservations::new(1, &capture, FaultStop::Deadline)
            .check
            .unwrap();
        assert_eq!((check.run, check.undecided_keys), (1, 3));
    }

    #[test]
    fn a_check_without_passes_keeps_the_previous_evidence() {
        let mut events = vec![
            state_event(reg::CHECK_ENABLED, STATE_SET, 1),
            state_event(reg::CHECKS_STARTED, STATE_SET, 1),
            json_pass(40, "checked"),
        ];
        events.extend(completion(40, 1, 1));
        events.extend([
            state_event(reg::CHECKS_STARTED, STATE_SET, 2),
            json_pass(9, "elsewhere"),
        ]);
        events.extend(completion(40, 2, 2));
        let capture = decode_sdk_events(&events).unwrap();
        let check = FaultObservations::new(1, &capture, FaultStop::Deadline)
            .check
            .unwrap();
        assert_eq!((check.run, check.end_generation), (1, 1));
        assert_eq!(check.points, ["checked"]);
        events.extend([
            state_event(reg::CHECKS_STARTED, STATE_SET, 3),
            json_pass(40, "checked"),
        ]);
        events.extend(completion(40, 3, 2));
        let capture = decode_sdk_events(&events).unwrap();
        let check = FaultObservations::new(1, &capture, FaultStop::Deadline)
            .check
            .unwrap();
        assert_eq!((check.run, check.end_generation), (3, 2));
    }

    #[test]
    fn assertion_and_crash_stops_are_bugs_and_end_the_branch() {
        let assertion = FaultStop::from_stop_reason(&StopReason::Assertion {
            vtime: Moment(5),
            ev: control_proto::EventRef {
                id: 2,
                data: Vec::new(),
            },
        });
        assert_eq!(assertion, FaultStop::Assertion { point: 2 });
        assert!(assertion.is_bug());
        assert!(!assertion.is_continuable());
        let crash = FaultStop::from_stop_reason(&StopReason::Crash {
            vtime: Moment(5),
            info: control_proto::CrashInfo {
                kind: control_proto::CrashKind::Panic,
                detail: Vec::new(),
            },
        });
        assert!(crash.is_bug());
        assert_eq!(
            FaultObservations::new(0, &SdkCapture::default(), crash).exit_kind(),
            ExitKind::Crash
        );
        assert!(
            FaultStop::from_stop_reason(&StopReason::Deadline { vtime: Moment(1) })
                .is_continuable()
        );
        assert!(!FaultStop::from_stop_reason(&StopReason::Quiescent { vtime: Moment(1) }).is_bug());
    }

    #[test]
    fn json_assertions_arriving_on_event_zero_are_decoded() {
        let json = |text: &str| (0, JSON_EVENT_ID, text.as_bytes().to_vec());
        let capture = decode_sdk_events(&[
            json(r#"{"antithesis_assert":{"id":"reached","assert_type":"reachability","must_hit":true,"hit":true,"condition":true}}"#),
            json(r#"{"antithesis_assert":{"id":"kept","assert_type":"always","must_hit":true,"hit":true,"condition":false}}"#),
            json(r#"{"antithesis_setup":{"status":"complete"}}"#),
            json("{not json}"),
            catalog_event(&[]),
        ])
        .expect("decode");
        assert!(capture.setup_complete);
        assert_eq!(capture.assertions.key_ids(), BTreeSet::from(["reached"]));
        let observation = FaultObservations::new(0, &capture, FaultStop::Deadline);
        assert_eq!(
            observation.violations(),
            BTreeSet::from(["kept".to_owned()])
        );
        assert!(observation.is_bug());
    }

    #[test]
    fn a_violation_event_alone_is_a_bug() {
        let capture = decode_sdk_events(&[assert_event(4, DISP_VIOLATION)]).expect("decode");
        let observations = FaultObservations::new(0, &capture, FaultStop::Deadline);
        assert!(observations.is_bug());
    }
}
