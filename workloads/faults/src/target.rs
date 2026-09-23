// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU16;

use control_proto::StopReason;
use fault_policy::{DecisionClass, Fault, HostFault, Span, StandingWindow, process_target};
use process_proto::registers as reg;
use searcher::target::ExitKind;
use serde::{Deserialize, Serialize};

use crate::assertion::{AssertionKind, AssertionOutcome, Assertions, decode_json_event};

pub const SUPERVISOR_TICK_NANOS: u64 = 10_000_000;
pub const SUPERVISOR_TICK_MICROS: u64 = SUPERVISOR_TICK_NANOS / 1_000;
const RESTART_DOWN_DIVISOR: u64 = 4;
pub const MAX_FAULT_ACTIONS: usize = 256;

const NS_SHIFT: u32 = 24;
const JSON_EVENT_ID: u32 = 0;
const NS_ASSERT: u8 = 1;
const NS_STATE: u8 = 2;
const DISP_HIT: u8 = 0;
const DISP_VIOLATION: u8 = 1;
const STATE_SET: u8 = 0;
const STATE_MAX: u8 = 1;
const ASSERT_PAYLOAD_LEN: usize = 3;
const STATE_PAYLOAD_LEN: usize = 9;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum FaultAction {
    Wait(NonZeroU16),
    Kill(u16, NonZeroU16),
    EventKill {
        node: u16,
        rarity: u8,
        ticks: NonZeroU16,
    },
    EventPark {
        node: u16,
        edges: u32,
        hold_us: u32,
    },
    Pause(u16, NonZeroU16),
    Restart(u16, NonZeroU16),
    Hook(u32, NonZeroU16),
    Interrupt(u32, NonZeroU16),
}

impl FaultAction {
    #[must_use]
    pub fn ticks(&self) -> u64 {
        match *self {
            Self::Wait(ticks)
            | Self::Kill(_, ticks)
            | Self::EventKill { ticks, .. }
            | Self::Pause(_, ticks)
            | Self::Restart(_, ticks)
            | Self::Hook(_, ticks)
            | Self::Interrupt(_, ticks) => u64::from(ticks.get()),
            Self::EventPark { hold_us, .. } => {
                u64::from(hold_us).div_ceil(SUPERVISOR_TICK_MICROS).max(1)
            }
        }
    }

    #[must_use]
    pub fn with_ticks(self, ticks: NonZeroU16) -> Self {
        match self {
            Self::Wait(_) => Self::Wait(ticks),
            Self::Kill(node, _) => Self::Kill(node, ticks),
            Self::EventKill { node, rarity, .. } => Self::EventKill {
                node,
                rarity,
                ticks,
            },
            Self::EventPark { node, edges, .. } => Self::EventPark {
                node,
                edges,
                hold_us: u32::from(ticks.get())
                    .saturating_mul(u32::try_from(SUPERVISOR_TICK_MICROS).unwrap_or(u32::MAX)),
            },
            Self::Pause(node, _) => Self::Pause(node, ticks),
            Self::Restart(node, _) => Self::Restart(node, ticks),
            Self::Hook(id, _) => Self::Hook(id, ticks),
            Self::Interrupt(vector, _) => Self::Interrupt(vector, ticks),
        }
    }
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
}

impl ActionWindows {
    fn end(self, start: u64, action: &FaultAction) -> Result<u64, String> {
        start
            .checked_add(action.ticks() * SUPERVISOR_TICK_NANOS)
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
    action.ticks()
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
        FaultAction::EventKill { node, rarity, .. } => ActionDelta {
            standing: Some(standing(
                process_target(node, &Fault::ProcEventKill { rarity }),
                (start, u64::MAX),
            )),
            perturb: None,
        },
        FaultAction::EventPark {
            node,
            edges,
            hold_us,
        } => {
            let hold = u64::from(hold_us).saturating_mul(1_000);
            ActionDelta {
                standing: Some(standing(
                    process_target(
                        node,
                        &Fault::ProcEventPark {
                            edges,
                            hold: Span(hold),
                        },
                    ),
                    (start, end.max(start.saturating_add(hold))),
                )),
                perturb: None,
            }
        }
        FaultAction::Kill(node, _) => ActionDelta {
            standing: Some(standing(
                process_target(node, &Fault::ProcKill),
                (start, end),
            )),
            perturb: None,
        },
        FaultAction::Pause(node, _) => ActionDelta {
            standing: Some(standing(
                process_target(node, &Fault::ProcPause(Span(horizon))),
                (start, end),
            )),
            perturb: None,
        },
        FaultAction::Restart(node, _) => ActionDelta {
            standing: Some(standing(
                process_target(node, &Fault::ProcRestart),
                (
                    start,
                    start.saturating_add(
                        (horizon / RESTART_DOWN_DIVISOR).max(SUPERVISOR_TICK_NANOS),
                    ),
                ),
            )),
            perturb: None,
        },
        FaultAction::Hook(id, _) => ActionDelta {
            standing: Some(standing(
                process_target(0, &Fault::RunHook(id)),
                (start, end),
            )),
            perturb: None,
        },
        FaultAction::Interrupt(vector, _) => ActionDelta {
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
    pub points: Vec<String>,
    pub pending_faults: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SdkCapture {
    pub registers: BTreeMap<u32, u64>,
    pub assertions: Assertions,
    pub setup_complete: bool,
    pub completed_check: Option<CompletedCheck>,
    pub parks: Vec<ParkLanding>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParkLanding {
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
    fn note_completed_check(&mut self, run: u64, check_passes: &BTreeMap<u64, BTreeSet<String>>) {
        let register = |id: u32| self.registers.get(&id).copied().unwrap_or_default();
        let Some(points) = check_passes
            .get(&register(reg::COMPLETED_CHECK_PID))
            .filter(|points| !points.is_empty())
        else {
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

pub fn decode_sdk_events(events: &[(u64, u32, Vec<u8>)]) -> Result<SdkCapture, String> {
    let mut capture = SdkCapture::default();
    let mut check_passes: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
    for (moment, event_id, bytes) in events {
        if *event_id == JSON_EVENT_ID && bytes.trim_ascii_start().first() == Some(&b'{') {
            if let Some(event) = decode_json_event(bytes) {
                capture.setup_complete |= event.setup_complete;
                if let Some(park) = event.park {
                    capture.parks.push(ParkLanding {
                        moment: *moment,
                        site: park.site,
                        edges: park.edges,
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
        let namespace = (event_id >> NS_SHIFT) as u8;
        let local = event_id & ((1 << NS_SHIFT) - 1);
        match namespace {
            NS_ASSERT if bytes.len() == ASSERT_PAYLOAD_LEN => {
                let (kind, passed) = match bytes[0] {
                    DISP_HIT => (AssertionKind::Sometimes, true),
                    DISP_VIOLATION => (AssertionKind::Always, false),
                    _ => return Err("SDK assertion event has an unknown disposition".to_owned()),
                };
                capture.assertions.record(
                    local.to_string(),
                    AssertionOutcome {
                        kind,
                        message: local.to_string(),
                        location: String::new(),
                        passed,
                        failed: !passed,
                    },
                );
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
                        }
                        if local == reg::COMPLETED_CHECK_RUN {
                            capture.note_completed_check(value, &check_passes);
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
    pub parks: Vec<ParkLanding>,
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
                    pending_faults: value(reg::PENDING_FAULTS),
                }
            }),
            assertions: capture.assertions.clone(),
            stop,
            watchdog_cutoff: false,
            parks: capture.parks.clone(),
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
        self.stop.is_bug() || !self.violations().is_empty()
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
    const WINDOWS: ActionWindows = ActionWindows { root_seal: ROOT };
    const TICK: u64 = SUPERVISOR_TICK_NANOS;

    fn ticks(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).unwrap()
    }

    fn kills(count: usize) -> Vec<FaultAction> {
        vec![FaultAction::Kill(0, ticks(50)); count]
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
            FaultAction::Kill(0, ticks(3)),
            FaultAction::Hook(1, ticks(7)),
            FaultAction::Pause(0, ticks(2)),
        ];
        assert_eq!(
            WINDOWS.window(&mixed, 2).unwrap(),
            (ROOT + 10 * TICK, ROOT + 12 * TICK)
        );
    }

    #[test]
    fn every_action_takes_a_recorded_duration() {
        let actions = [
            FaultAction::Wait(ticks(9)),
            FaultAction::Kill(1, ticks(9)),
            FaultAction::EventKill {
                node: 1,
                rarity: 3,
                ticks: ticks(9),
            },
            FaultAction::EventPark {
                node: 1,
                edges: 5,
                hold_us: 1,
            },
            FaultAction::Pause(1, ticks(9)),
            FaultAction::Restart(1, ticks(9)),
            FaultAction::Hook(4, ticks(9)),
            FaultAction::Interrupt(0x20, ticks(9)),
        ];
        for action in actions {
            let adapted = action.with_ticks(ticks(12));
            assert_eq!(adapted.ticks(), 12);
            assert_eq!(action_ticks(&adapted), 12);
        }
        assert_eq!(
            FaultAction::EventPark {
                node: 0,
                edges: 1,
                hold_us: 10_001,
            }
            .ticks(),
            2
        );
    }

    #[test]
    fn an_overflowing_timeline_is_rejected() {
        let windows = ActionWindows {
            root_seal: u64::MAX - 1,
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
            FaultAction::Wait(ticks(8)),
            FaultAction::Kill(0, ticks(50)),
            FaultAction::Wait(ticks(8192)),
            FaultAction::Hook(1, ticks(50)),
        ];
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
    fn an_event_park_stands_until_its_hold_can_finish() {
        let action = FaultAction::EventPark {
            node: 2,
            edges: 4_096,
            hold_us: 2_000_000,
        };
        let window = WINDOWS.window(&[action], 0).unwrap();
        let fault = action_delta(action, window).standing.unwrap();
        assert_eq!(fault.start, ROOT);
        assert_eq!(fault.end, ROOT + 2_000_000_000);
        assert_eq!(
            decode_process_target(&fault.target),
            Some((
                2,
                Fault::ProcEventPark {
                    edges: 4_096,
                    hold: Span(2_000_000_000),
                }
            ))
        );
    }

    #[test]
    fn wait_installs_nothing() {
        assert_eq!(
            action_delta(
                FaultAction::Wait(NonZeroU16::MIN),
                WINDOWS.window(&kills(4), 0).unwrap()
            ),
            ActionDelta::default()
        );
        assert!(
            standing_windows(
                WINDOWS,
                &[
                    FaultAction::Wait(NonZeroU16::MIN),
                    FaultAction::Wait(NonZeroU16::MIN)
                ]
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn kill_holds_its_whole_window_for_its_node() {
        let window = WINDOWS.window(&kills(4), 1).unwrap();
        let delta = action_delta(FaultAction::Kill(2, ticks(50)), window);
        let fault = delta.standing.expect("kill installs a standing fault");
        assert_eq!(fault.class, DecisionClass::Process.as_u16());
        assert_eq!(
            decode_process_target(&fault.target),
            Some((2, Fault::ProcKill))
        );
        assert_eq!((fault.start, fault.end), window);
        assert!(delta.perturb.is_none());
    }

    #[test]
    fn pause_holds_its_node_for_its_recorded_duration() {
        for duration in [1_u16, 7, 1_024] {
            let actions = [FaultAction::Pause(1, ticks(duration))];
            let (start, end) = WINDOWS.window(&actions, 0).unwrap();
            let fault = action_delta(actions[0], (start, end))
                .standing
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
            let actions = [FaultAction::Restart(0, ticks(duration))];
            let (start, end) = WINDOWS.window(&actions, 0).unwrap();
            let fault = action_delta(actions[0], (start, end))
                .standing
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
        let fault = action_delta(
            FaultAction::Hook(9, ticks(50)),
            WINDOWS.window(&kills(4), 0).unwrap(),
        )
        .standing
        .expect("hook installs a standing fault");
        assert_eq!(
            decode_process_target(&fault.target),
            Some((0, Fault::RunHook(9)))
        );
    }

    #[test]
    fn interrupt_stages_a_host_fault_and_no_standing_fault() {
        let window = WINDOWS.window(&kills(4), 2).unwrap();
        let delta = action_delta(FaultAction::Interrupt(0x30, ticks(50)), window);
        assert!(delta.standing.is_none());
        let perturb = delta.perturb.expect("interrupt stages a host fault");
        assert_eq!(perturb.at, window.0);
        assert_eq!(
            HostFault::decode(&perturb.fault),
            Ok(HostFault::InjectInterrupt { vector: 0x30 })
        );
    }

    #[test]
    fn an_input_installs_one_standing_fault_per_faulting_action() {
        let actions = [
            FaultAction::Hook(1, ticks(50)),
            FaultAction::Wait(NonZeroU16::MIN),
            FaultAction::Interrupt(32, ticks(50)),
            FaultAction::Kill(0, ticks(50)),
        ];
        let faults = standing_windows(WINDOWS, &actions).unwrap();
        assert_eq!(faults.len(), 2);
        assert_eq!(
            (faults[0].start, faults[0].end),
            WINDOWS.window(&actions, 0).unwrap()
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
        let actions = [
            FaultAction::Kill(1, ticks(50)),
            FaultAction::Hook(2, ticks(50)),
        ];
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
            FaultAction::Restart(3, ticks(50)),
            FaultAction::Wait(NonZeroU16::MIN),
        ];
        assert_eq!(
            standing_windows(WINDOWS, &actions),
            standing_windows(WINDOWS, &actions)
        );
        let moved = ActionWindows { root_seal: 2_000 };
        assert_ne!(
            standing_windows(WINDOWS, &actions),
            standing_windows(moved, &actions)
        );
        let shorter = [
            FaultAction::Restart(3, ticks(10)),
            FaultAction::Wait(NonZeroU16::MIN),
        ];
        assert_ne!(
            standing_windows(WINDOWS, &actions),
            standing_windows(WINDOWS, &shorter)
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
        ])
        .expect("decode");
        let observations = FaultObservations::new(77, &capture, FaultStop::Deadline);
        assert_eq!(observations.moment, 77);
        assert_eq!(observations.alive, 0b101);
        assert_eq!(observations.hooks_finished, 2);
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
    fn park_reports_become_landings_with_their_moment() {
        let capture = decode_sdk_events(&[(
            41,
            JSON_EVENT_ID,
            br#"{"harmony_attribution":{"rip":"0x1","pid":7,"comm_hex":"61"},"harmony_park":{"site":913,"edges":4096}}
"#
            .to_vec(),
        )])
        .unwrap();
        assert_eq!(
            capture.parks,
            vec![ParkLanding {
                moment: 41,
                site: 913,
                edges: 4096,
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
                pending_faults: 1,
            })
        );
        assert!(FaultObservations::default().check.is_none());
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
            (0, JSON_EVENT_ID, b"SDKC".to_vec()),
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
