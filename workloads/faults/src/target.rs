// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet};

use control_proto::StopReason;
use fault_policy::{DecisionClass, Fault, HostFault, Span, StandingWindow, process_target};
use searcher::target::ExitKind;
use serde::{Deserialize, Serialize};

pub const DEFAULT_HORIZON_NANOS: u64 = 500_000_000;
pub const AGENT_TICK_NANOS: u64 = 10_000_000;
const RESTART_DOWN_DIVISOR: u64 = 4;
pub const MAX_FAULT_ACTIONS: usize = 256;

pub mod reg {
    pub const TICKS: u32 = 1;
    pub const ALIVE: u32 = 2;
    pub const HOOKS_STARTED: u32 = 3;
    pub const HOOKS_FINISHED: u32 = 4;
    pub const SOMETIMES: u32 = 5;
    pub const UNEXPECTED_DEATHS: u32 = 6;
    pub const RESTARTS: u32 = 7;
    pub const PARKED: u32 = 8;
    pub const EVENT_KILL_FIRES: u32 = 9;
    pub const EVENT_KILL_SITE: u32 = 10;
    pub const EVENT_PARK_FIRES: u32 = 11;
    pub const WORKLOAD_STARTED: u32 = 12;
    pub const WORKLOAD_FINISHED: u32 = 13;
    pub const CHECKS_STARTED: u32 = 14;
    pub const CHECKS_FINISHED: u32 = 15;
    pub const INFRASTRUCTURE_ERROR: u32 = 16;
    pub const EVENT_READY: u32 = 17;
    pub const DISTURBANCE_GENERATION: u32 = 18;
    pub const CHECK_ENABLED: u32 = 19;
    pub const COMPLETED_CHECK_RUN: u32 = 20;
    pub const COMPLETED_CHECK_START_GENERATION: u32 = 21;
    pub const COMPLETED_CHECK_END_GENERATION: u32 = 22;
    pub const COMPLETED_CHECK_POINTS: u32 = 23;
    pub const PENDING_FAULTS: u32 = 24;
}

const NS_SHIFT: u32 = 24;
const NS_ASSERT: u8 = 1;
const NS_STATE: u8 = 2;
const DISP_HIT: u8 = 0;
const DISP_VIOLATION: u8 = 1;
const STATE_SET: u8 = 0;
const STATE_MAX: u8 = 1;
const ASSERT_PAYLOAD_LEN: usize = 3;
const STATE_PAYLOAD_LEN: usize = 9;

pub const SOMETIMES_KEY_BITS: u32 = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum FaultAction {
    Wait(std::num::NonZeroU16),
    Kill(u16),
    EventKill {
        node: u16,
        rarity: u8,
    },
    EventPark {
        node: u16,
        rarity: u8,
        hold_us: u32,
    },
    Pause(u16, u32),
    Restart(u16),
    Hook(u32),
    Interrupt(u32),
    Park {
        node: u16,
        addr: u64,
        hits: u32,
        hold_us: u32,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultSnapshot {
    pub actions: Vec<FaultAction>,
    pub observation: FaultObservations,
    pub failed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedPerturb {
    pub fault: Vec<u8>,
    pub at: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActionDelta {
    pub standing: Option<StandingWindow>,
    pub perturb: Option<StagedPerturb>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActionWindows {
    pub root_seal: u64,
    pub horizon_nanos: u64,
}

impl ActionWindows {
    fn end(self, start: u64, action: &FaultAction) -> Result<u64, String> {
        let duration = match action {
            FaultAction::Wait(ticks) => u64::from(ticks.get()) * AGENT_TICK_NANOS,
            _ => self.horizon_nanos,
        };
        if duration == 0 {
            return Err("action window must have positive duration".to_owned());
        }
        start
            .checked_add(duration)
            .ok_or_else(|| "action timeline overflows guest time".to_owned())
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
}

#[must_use]
pub fn action_ticks(action: &FaultAction) -> u64 {
    match action {
        FaultAction::Wait(ticks) => u64::from(ticks.get()),
        _ => DEFAULT_HORIZON_NANOS / AGENT_TICK_NANOS,
    }
}

fn standing(target: Vec<u8>, window: (u64, u64)) -> StandingWindow {
    StandingWindow {
        class: DecisionClass::Process.as_u16(),
        target,
        start: window.0,
        end: window.1,
    }
}

#[must_use]
pub fn action_delta(action: FaultAction, window: (u64, u64)) -> ActionDelta {
    let (start, end) = window;
    let horizon = end.saturating_sub(start);
    match action {
        FaultAction::Wait(_) => ActionDelta::default(),
        FaultAction::EventKill { node, rarity } => ActionDelta {
            standing: Some(standing(
                process_target(node, &Fault::ProcEventKill { rarity }),
                (start, u64::MAX),
            )),
            perturb: None,
        },
        FaultAction::EventPark {
            node,
            rarity,
            hold_us,
        } => ActionDelta {
            standing: Some(standing(
                process_target(
                    node,
                    &Fault::ProcEventPark {
                        rarity,
                        hold: Span(u64::from(hold_us) * 1_000),
                    },
                ),
                (start, end),
            )),
            perturb: None,
        },
        FaultAction::Kill(node) => ActionDelta {
            standing: Some(standing(
                process_target(node, &Fault::ProcKill),
                (start, end),
            )),
            perturb: None,
        },
        FaultAction::Pause(node, ticks) => {
            let held = u64::from(ticks)
                .saturating_mul(AGENT_TICK_NANOS)
                .min(horizon.saturating_sub(AGENT_TICK_NANOS))
                .max(AGENT_TICK_NANOS);
            ActionDelta {
                standing: Some(standing(
                    process_target(node, &Fault::ProcPause(Span(held))),
                    (start, start.saturating_add(held)),
                )),
                perturb: None,
            }
        }
        FaultAction::Restart(node) => ActionDelta {
            standing: Some(standing(
                process_target(node, &Fault::ProcRestart),
                (start, start.saturating_add(horizon / RESTART_DOWN_DIVISOR)),
            )),
            perturb: None,
        },
        FaultAction::Hook(id) => ActionDelta {
            standing: Some(standing(
                process_target(0, &Fault::RunHook(id)),
                (start, end),
            )),
            perturb: None,
        },
        FaultAction::Park {
            node,
            addr,
            hits,
            hold_us,
        } => ActionDelta {
            standing: Some(standing(
                process_target(
                    node,
                    &Fault::ProcPark {
                        addr,
                        hits,
                        hold: Span(u64::from(hold_us).saturating_mul(1_000)),
                    },
                ),
                (start, end),
            )),
            perturb: None,
        },
        FaultAction::Interrupt(vector) => ActionDelta {
            standing: None,
            perturb: Some(StagedPerturb {
                fault: HostFault::InjectInterrupt { vector }.encode(),
                at: start,
            }),
        },
    }
}

pub fn action_deltas(
    windows: ActionWindows,
    actions: &[FaultAction],
) -> Result<Vec<ActionDelta>, String> {
    let mut start = windows.root_seal;
    actions
        .iter()
        .map(|action| {
            let end = windows.end(start, action)?;
            let delta = action_delta(*action, (start, end));
            start = end;
            Ok(delta)
        })
        .collect()
}

pub fn standing_windows(
    windows: ActionWindows,
    actions: &[FaultAction],
) -> Result<Vec<StandingWindow>, String> {
    Ok(action_deltas(windows, actions)?
        .into_iter()
        .filter_map(|delta| delta.standing)
        .collect())
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckEvidence {
    pub disturbance_generation: u64,
    pub run: u64,
    pub start_generation: u64,
    pub end_generation: u64,
    pub points: Vec<u32>,
    pub pending_faults: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SdkCapture {
    pub registers: BTreeMap<u32, u64>,
    pub sometimes: BTreeSet<u32>,
    pub violations: BTreeSet<u32>,
}

impl SdkCapture {
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

pub fn decode_sdk_events(events: &[(u64, u32, Vec<u8>)]) -> Result<SdkCapture, String> {
    let mut capture = SdkCapture::default();
    for (_, event_id, bytes) in events {
        let namespace = (event_id >> NS_SHIFT) as u8;
        let local = event_id & ((1 << NS_SHIFT) - 1);
        match namespace {
            NS_ASSERT if bytes.len() == ASSERT_PAYLOAD_LEN => match bytes[0] {
                DISP_HIT => {
                    capture.sometimes.insert(local);
                }
                DISP_VIOLATION => {
                    capture.violations.insert(local);
                }
                _ => return Err("SDK assertion event has an unknown disposition".to_owned()),
            },
            NS_STATE if bytes.len() == STATE_PAYLOAD_LEN => {
                let value = u64::from_le_bytes(
                    bytes[1..STATE_PAYLOAD_LEN]
                        .try_into()
                        .map_err(|_| "SDK state payload is truncated")?,
                );
                match bytes[0] {
                    STATE_SET => {
                        capture.registers.insert(local, value);
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
    pub parked: u64,
    pub event_kill_fires: u64,
    pub event_kill_site: u64,
    pub event_ready: u64,
    pub event_park_fires: u64,
    pub workload_started: u64,
    pub workload_finished: u64,
    pub checks_started: u64,
    pub checks_finished: u64,
    pub check: Option<CheckEvidence>,
    pub sometimes_register: u64,
    pub sometimes: BTreeSet<u32>,
    pub violations: BTreeSet<u32>,
    pub stop: FaultStop,
    #[serde(default)]
    pub watchdog_cutoff: bool,
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
            parked: value(reg::PARKED),
            event_kill_fires: value(reg::EVENT_KILL_FIRES),
            event_kill_site: value(reg::EVENT_KILL_SITE),
            event_ready: value(reg::EVENT_READY),
            event_park_fires: value(reg::EVENT_PARK_FIRES),
            workload_started: value(reg::WORKLOAD_STARTED),
            workload_finished: value(reg::WORKLOAD_FINISHED),
            checks_started: value(reg::CHECKS_STARTED),
            checks_finished: value(reg::CHECKS_FINISHED),
            check: (value(reg::CHECK_ENABLED) != 0).then(|| CheckEvidence {
                disturbance_generation: value(reg::DISTURBANCE_GENERATION),
                run: value(reg::COMPLETED_CHECK_RUN),
                start_generation: value(reg::COMPLETED_CHECK_START_GENERATION),
                end_generation: value(reg::COMPLETED_CHECK_END_GENERATION),
                points: (0..48)
                    .filter(|point| value(reg::COMPLETED_CHECK_POINTS) & (1_u64 << point) != 0)
                    .collect(),
                pending_faults: value(reg::PENDING_FAULTS),
            }),
            sometimes_register: value(reg::SOMETIMES),
            sometimes: capture.sometimes.clone(),
            violations: capture.violations.clone(),
            stop,
            watchdog_cutoff: false,
        }
    }

    #[must_use]
    pub fn sometimes_bitmap(&self) -> u64 {
        self.sometimes
            .iter()
            .filter(|id| **id < SOMETIMES_KEY_BITS)
            .fold(0_u64, |bits, id| bits | (1_u64 << id))
    }

    #[must_use]
    pub fn is_bug(&self) -> bool {
        self.stop.is_bug() || !self.violations.is_empty()
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
        horizon_nanos: DEFAULT_HORIZON_NANOS,
    };

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

    #[test]
    fn windows_tile_the_axis_from_the_root_seal() {
        assert_eq!(
            WINDOWS.window(&[FaultAction::Kill(0); 4], 0).unwrap(),
            (1_000, 1_000 + DEFAULT_HORIZON_NANOS)
        );
        let (start, end) = WINDOWS.window(&[FaultAction::Kill(0); 4], 3).unwrap();
        assert_eq!(start, 1_000 + 3 * DEFAULT_HORIZON_NANOS);
        assert_eq!(end, start + DEFAULT_HORIZON_NANOS);
        assert_eq!(
            WINDOWS.window(&[FaultAction::Kill(0); 4], 3).unwrap().1,
            end
        );
        let short = ActionWindows {
            horizon_nanos: 100_000_000,
            ..WINDOWS
        };
        assert_eq!(
            short.window(&[FaultAction::Kill(0); 4], 3).unwrap(),
            (1_000 + 300_000_000, 1_000 + 400_000_000)
        );
    }

    #[test]
    fn an_overflowing_timeline_is_rejected() {
        let windows = ActionWindows {
            root_seal: u64::MAX - 1,
            ..WINDOWS
        };
        assert!(windows.window(&[FaultAction::Kill(0)], 0).is_err());
        assert!(WINDOWS.window(&[], 0).is_err());
        assert!(serde_json::from_str::<FaultAction>(r#"{"Wait":0}"#).is_err());
        assert!(serde_json::from_str::<FaultAction>(r#"{"Wait":65536}"#).is_err());
    }

    #[test]
    fn short_and_long_waits_shift_later_faults_by_the_recorded_duration() {
        let actions = [
            FaultAction::Wait(std::num::NonZeroU16::new(8).unwrap()),
            FaultAction::Kill(0),
            FaultAction::Wait(std::num::NonZeroU16::new(8192).unwrap()),
            FaultAction::Hook(1),
        ];
        assert_eq!(
            WINDOWS.window(&actions, 1).unwrap().0,
            ROOT + 8 * AGENT_TICK_NANOS
        );
        assert_eq!(
            WINDOWS.window(&actions, 3).unwrap().0,
            ROOT + 8200 * AGENT_TICK_NANOS + DEFAULT_HORIZON_NANOS
        );
        let encoded = serde_json::to_string(&actions).unwrap();
        let replay: Vec<FaultAction> = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            action_deltas(WINDOWS, &actions),
            action_deltas(WINDOWS, &replay)
        );
    }

    #[test]
    fn wait_installs_nothing() {
        assert_eq!(
            action_delta(
                FaultAction::Wait(std::num::NonZeroU16::MIN),
                WINDOWS.window(&[FaultAction::Kill(0); 4], 0).unwrap()
            ),
            ActionDelta::default()
        );
        assert!(
            standing_windows(
                WINDOWS,
                &[
                    FaultAction::Wait(std::num::NonZeroU16::MIN),
                    FaultAction::Wait(std::num::NonZeroU16::MIN)
                ]
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn kill_holds_the_whole_horizon_for_its_node() {
        let delta = action_delta(
            FaultAction::Kill(2),
            WINDOWS.window(&[FaultAction::Kill(0); 4], 1).unwrap(),
        );
        let fault = delta.standing.expect("kill installs a standing fault");
        assert_eq!(fault.class, DecisionClass::Process.as_u16());
        assert_eq!(
            decode_process_target(&fault.target),
            Some((2, Fault::ProcKill))
        );
        assert_eq!(
            (fault.start, fault.end),
            WINDOWS.window(&[FaultAction::Kill(0); 4], 1).unwrap()
        );
        assert!(delta.perturb.is_none());
    }

    #[test]
    fn pause_lifts_inside_its_own_horizon() {
        let short = ActionWindows {
            horizon_nanos: 5 * AGENT_TICK_NANOS,
            ..WINDOWS
        };
        for windows in [WINDOWS, short] {
            for ticks in [0_u32, 1, 7, u32::MAX] {
                let (start, end) = windows.window(&[FaultAction::Kill(0); 4], 0).unwrap();
                let delta = action_delta(FaultAction::Pause(1, ticks), (start, end));
                let fault = delta.standing.expect("pause installs a standing fault");
                let (node, decoded) = decode_process_target(&fault.target).expect("decode");
                assert_eq!(node, 1);
                let held = match decoded {
                    Fault::ProcPause(Span(held)) => held,
                    other => panic!("pause encoded as {other:?}"),
                };
                assert_eq!(fault.start, start);
                assert_eq!(fault.end, start + held);
                assert!(fault.end < end, "a pause must lift before the horizon");
                assert!(held >= AGENT_TICK_NANOS, "a pause must span a whole tick");
            }
        }
    }

    #[test]
    fn restart_leaves_its_window_inside_the_horizon() {
        for horizon_nanos in [DEFAULT_HORIZON_NANOS, 100_000_000] {
            let windows = ActionWindows {
                horizon_nanos,
                ..WINDOWS
            };
            let (start, end) = windows.window(&[FaultAction::Kill(0); 4], 0).unwrap();
            let fault = action_delta(FaultAction::Restart(0), (start, end))
                .standing
                .expect("restart installs a standing fault");
            assert_eq!(
                decode_process_target(&fault.target),
                Some((0, Fault::ProcRestart))
            );
            assert_eq!(fault.end, start + horizon_nanos / 4);
            assert!(fault.end < end);
        }
    }

    #[test]
    fn hook_targets_the_agent_rather_than_a_node() {
        let fault = action_delta(
            FaultAction::Hook(9),
            WINDOWS.window(&[FaultAction::Kill(0); 4], 0).unwrap(),
        )
        .standing
        .expect("hook installs a standing fault");
        assert_eq!(
            decode_process_target(&fault.target),
            Some((0, Fault::RunHook(9)))
        );
    }

    #[test]
    fn park_carries_its_place_hit_and_hold() {
        let fault = action_delta(
            FaultAction::Park {
                node: 1,
                addr: 0x4b_0e86,
                hits: 28,
                hold_us: 2_000,
            },
            WINDOWS.window(&[FaultAction::Kill(0); 4], 0).unwrap(),
        )
        .standing
        .expect("park installs a standing fault");
        assert_eq!(
            decode_process_target(&fault.target),
            Some((
                1,
                Fault::ProcPark {
                    addr: 0x4b_0e86,
                    hits: 28,
                    hold: Span(2_000_000),
                }
            ))
        );
    }

    #[test]
    fn interrupt_stages_a_host_fault_and_no_standing_fault() {
        let (start, _) = WINDOWS.window(&[FaultAction::Kill(0); 4], 2).unwrap();
        let delta = action_delta(
            FaultAction::Interrupt(0x30),
            WINDOWS.window(&[FaultAction::Kill(0); 4], 2).unwrap(),
        );
        assert!(delta.standing.is_none());
        let perturb = delta.perturb.expect("interrupt stages a host fault");
        assert_eq!(perturb.at, start);
        assert_eq!(
            HostFault::decode(&perturb.fault),
            Ok(HostFault::InjectInterrupt { vector: 0x30 })
        );
    }

    #[test]
    fn an_input_installs_one_standing_fault_per_faulting_action() {
        let actions = [
            FaultAction::Hook(1),
            FaultAction::Wait(std::num::NonZeroU16::MIN),
            FaultAction::Interrupt(32),
            FaultAction::Kill(0),
        ];
        let faults = standing_windows(WINDOWS, &actions).unwrap();
        assert_eq!(faults.len(), 2);
        assert_eq!(
            (faults[0].start, faults[0].end),
            WINDOWS.window(&[FaultAction::Kill(0); 4], 0).unwrap()
        );
        assert_eq!(
            (faults[1].start, faults[1].end),
            WINDOWS.window(&actions, 3).unwrap()
        );
        assert!(
            faults
                .iter()
                .all(|fault| fault.class == DecisionClass::Process.as_u16())
        );
    }

    #[test]
    fn the_window_list_round_trips_through_the_shared_codec() {
        let actions = [FaultAction::Kill(1), FaultAction::Hook(2)];
        let windows = standing_windows(WINDOWS, &actions).unwrap();
        let bytes = fault_policy::encode_windows(&windows).expect("encode");
        assert_eq!(
            fault_policy::decode_windows(&bytes).expect("decode"),
            windows
        );
    }

    #[test]
    fn the_window_list_is_a_function_of_the_actions_and_the_tiling() {
        let actions = [
            FaultAction::Restart(3),
            FaultAction::Wait(std::num::NonZeroU16::MIN),
        ];
        assert_eq!(
            standing_windows(WINDOWS, &actions),
            standing_windows(WINDOWS, &actions)
        );
        let moved = ActionWindows {
            root_seal: 2_000,
            ..WINDOWS
        };
        let shorter = ActionWindows {
            horizon_nanos: 100_000_000,
            ..WINDOWS
        };
        assert_ne!(
            standing_windows(WINDOWS, &actions),
            standing_windows(moved, &actions)
        );
        assert_ne!(
            standing_windows(WINDOWS, &actions),
            standing_windows(shorter, &actions)
        );
    }

    #[test]
    fn a_reported_runtime_failure_is_not_bug_evidence() {
        let capture =
            decode_sdk_events(&[state_event(reg::INFRASTRUCTURE_ERROR, STATE_SET, 1)]).unwrap();
        assert!(capture.violations.is_empty());
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
        assert_eq!(capture.sometimes, BTreeSet::from([3, 70]));
        assert_eq!(capture.violations, BTreeSet::from([5]));
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
            state_event(reg::SOMETIMES, STATE_SET, 0b11),
        ])
        .expect("decode");
        let observations = FaultObservations::new(77, &capture, FaultStop::Deadline);
        assert_eq!(observations.moment, 77);
        assert_eq!(observations.alive, 0b101);
        assert_eq!(observations.hooks_finished, 2);
        assert_eq!(observations.sometimes_register, 0b11);
        assert_eq!(observations.sometimes, BTreeSet::from([0, 63, 64]));
        assert_eq!(
            observations.sometimes_bitmap(),
            (1_u64 << 63) | 1,
            "a site past the key width stays in the observation only"
        );
        assert!(!observations.is_bug());
        assert_eq!(observations.exit_kind(), ExitKind::Ok);
    }

    #[test]
    fn completed_check_provenance_is_distinct_from_cumulative_hits() {
        let capture = decode_sdk_events(&[
            assert_event(11, DISP_HIT),
            state_event(reg::CHECK_ENABLED, STATE_SET, 1),
            state_event(reg::COMPLETED_CHECK_RUN, STATE_SET, 7),
            state_event(reg::COMPLETED_CHECK_START_GENERATION, STATE_SET, 2),
            state_event(reg::COMPLETED_CHECK_END_GENERATION, STATE_SET, 2),
            state_event(reg::COMPLETED_CHECK_POINTS, STATE_SET, 1 << 11),
            state_event(reg::DISTURBANCE_GENERATION, STATE_SET, 3),
            state_event(reg::PENDING_FAULTS, STATE_SET, 1),
        ])
        .unwrap();
        let observation = FaultObservations::new(77, &capture, FaultStop::Deadline);
        assert!(observation.sometimes.contains(&11));
        assert_eq!(
            observation.check,
            Some(CheckEvidence {
                disturbance_generation: 3,
                run: 7,
                start_generation: 2,
                end_generation: 2,
                points: vec![11],
                pending_faults: 1,
            })
        );
        assert!(FaultObservations::default().check.is_none());
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
    fn a_violation_event_alone_is_a_bug() {
        let capture = decode_sdk_events(&[assert_event(4, DISP_VIOLATION)]).expect("decode");
        let observations = FaultObservations::new(0, &capture, FaultStop::Deadline);
        assert!(observations.is_bug());
    }
}
