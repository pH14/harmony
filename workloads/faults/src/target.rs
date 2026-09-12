// SPDX-License-Identifier: AGPL-3.0-or-later

//! The fault action vocabulary, its environment delta, and the decoding of one
//! endpoint's guest-published evidence.
//!
//! Every function here is pure: an action list maps to standing faults and
//! staged host perturbations by arithmetic on the root seal `Moment` alone, and
//! an endpoint's evidence decodes from a captured SDK event page. Neither needs
//! a live guest, so both are exercised directly by unit tests.

use std::collections::{BTreeMap, BTreeSet};

use control_proto::StopReason;
use fault_policy::{DecisionClass, Fault, HostFault, Span, StandingWindow, process_target};
use searcher::target::ExitKind;
use serde::{Deserialize, Serialize};

/// Virtual nanoseconds one action runs for before its endpoint is sealed,
/// unless the campaign sets its own horizon.
pub const DEFAULT_HORIZON_NANOS: u64 = 2_000_000_000;
/// Virtual nanoseconds of one guest fault-agent reconcile tick. A `Pause`
/// duration is expressed in these because the agent can only observe a window
/// boundary on a tick.
pub const AGENT_TICK_NANOS: u64 = 10_000_000;
/// A `Restart` holds a node down for this fraction of the horizon before the
/// agent sees the window leave and starts it again, so the restarted node is
/// observable inside the same action.
const RESTART_DOWN_DIVISOR: u64 = 4;
/// Largest action count one fault input may carry.
pub const MAX_FAULT_ACTIONS: usize = 256;

/// Guest fault-agent state registers, namespace 2 of the SDK event stream.
pub mod reg {
    /// Agent reconcile ticks completed.
    pub const TICKS: u32 = 1;
    /// Bitmap of nodes currently running, bit `n` for node `n`.
    pub const ALIVE: u32 = 2;
    /// Hooks the agent has spawned.
    pub const HOOKS_STARTED: u32 = 3;
    /// Hooks that ran to completion.
    pub const HOOKS_FINISHED: u32 = 4;
    /// Bitmap of `sometimes` sites hit, first 48 ids.
    pub const SOMETIMES: u32 = 5;
    /// Nodes that died with no fault active.
    pub const UNEXPECTED_DEATHS: u32 = 6;
    /// Nodes the agent restarted.
    pub const RESTARTS: u32 = 7;
    /// Threads the guest kernel parked at a place.
    pub const PARKED: u32 = 8;
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

/// Widest `sometimes` site id the archive key can distinguish.
pub const SOMETIMES_KEY_BITS: u32 = 64;

/// One total action of the fault vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum FaultAction {
    /// Let the workload run through the horizon with no new fault.
    Wait,
    /// SIGKILL one node for the whole horizon.
    Kill(u16),
    /// SIGSTOP one node for the given number of agent ticks, then SIGCONT.
    Pause(u16, u32),
    /// SIGKILL one node and let the agent start it again inside the horizon.
    Restart(u16),
    /// Spawn one workload hook once.
    Hook(u32),
    /// Inject one interrupt vector at the start of the horizon.
    Interrupt(u32),
    /// Hold one thread of a node at an instruction for the horizon: the
    /// thread that reaches `addr` for the `hits`-th time stops there, before
    /// the instruction runs, for `hold_us` microseconds. The guest kernel
    /// counts and holds, so the node sees only time.
    Park {
        /// The node.
        node: u16,
        /// User virtual address of the instruction in the node's process.
        addr: u64,
        /// The hit that parks, counted from 1.
        hits: u32,
        /// Length of the hold in microseconds.
        hold_us: u32,
    },
}

/// Portable campaign snapshot: the action prefix that reaches an endpoint plus
/// the endpoint's decoded evidence. Each evaluator maps the prefix back to its
/// own real whole-VM snapshot.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultSnapshot {
    /// The action prefix, in execution order.
    pub actions: Vec<FaultAction>,
    /// The endpoint's observations.
    pub observation: FaultObservations,
    /// Whether the evaluator that produced it had already failed.
    pub failed: bool,
}

/// A staged host-plane perturbation: opaque `HostFault` bytes and the `Moment`
/// they apply at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedPerturb {
    /// Encoded [`HostFault`] bytes.
    pub fault: Vec<u8>,
    /// The `Moment` the backend applies the fault at.
    pub at: u64,
}

/// One action's environment delta over its own horizon window.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActionDelta {
    /// The Process-class standing fault the action installs, if any.
    pub standing: Option<StandingWindow>,
    /// The host-plane perturbation the action stages, if any.
    pub perturb: Option<StagedPerturb>,
}

/// The tiling every action window is cut from: equal horizons laid end to end
/// from the root seal.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActionWindows {
    /// The sealed setup `Moment` the first window starts at.
    pub root_seal: u64,
    /// Virtual nanoseconds each window spans.
    pub horizon_nanos: u64,
}

impl ActionWindows {
    /// The half-open window the action at `index` owns. Saturating so a long
    /// input can never wrap the V-time axis.
    #[must_use]
    pub fn window(self, index: usize) -> (u64, u64) {
        let offset = (index as u64).saturating_mul(self.horizon_nanos);
        let start = self.root_seal.saturating_add(offset);
        (start, start.saturating_add(self.horizon_nanos))
    }

    /// The deadline a run must reach for the action at `index` to be complete.
    #[must_use]
    pub fn deadline(self, index: usize) -> u64 {
        self.window(index).1
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

/// Map one action to its environment delta over `window`.
#[must_use]
pub fn action_delta(action: FaultAction, window: (u64, u64)) -> ActionDelta {
    let (start, end) = window;
    let horizon = end.saturating_sub(start);
    match action {
        FaultAction::Wait => ActionDelta::default(),
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

/// Every action's delta in input order.
#[must_use]
pub fn action_deltas(windows: ActionWindows, actions: &[FaultAction]) -> Vec<ActionDelta> {
    actions
        .iter()
        .enumerate()
        .map(|(index, action)| action_delta(*action, windows.window(index)))
        .collect()
}

/// The standing-fault window list an input installs, in input order. These are
/// the configuration bytes the package's service handler answers guest polls
/// from.
#[must_use]
pub fn standing_windows(windows: ActionWindows, actions: &[FaultAction]) -> Vec<StandingWindow> {
    action_deltas(windows, actions)
        .into_iter()
        .filter_map(|delta| delta.standing)
        .collect()
}

/// Everything one SDK event page says about an endpoint.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SdkCapture {
    /// State registers, last write wins for `state_set` and the maximum for
    /// `state_max`.
    pub registers: BTreeMap<u32, u64>,
    /// `sometimes` sites hit, namespace 1 dispositions of kind hit.
    pub sometimes: BTreeSet<u32>,
    /// Assertion violations, namespace 1 dispositions of kind violation.
    pub violations: BTreeSet<u32>,
}

/// Decode one SDK event page.
///
/// Namespace-1 hits are the searcher's coverage signal here, so unlike the
/// register-only decoders they are kept rather than skipped.
///
/// # Errors
///
/// Returns an error when a state event carries an unknown operation. A payload
/// of the wrong length for its namespace is ignored, never misread.
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

/// How one action's run ended.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum FaultStop {
    /// The run reached its horizon deadline, the nominal outcome.
    #[default]
    Deadline,
    /// The guest went quiescent before the deadline.
    Quiescent,
    /// An `assert_always` fired: a bug.
    Assertion {
        /// The assertion point id.
        point: u32,
    },
    /// The guest crashed: a bug.
    Crash,
    /// Any other stop; the endpoint is not usable as a parent.
    Unexpected,
}

impl FaultStop {
    /// Classify one control-plane stop.
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

    /// Whether this stop is a bug the campaign must report.
    #[must_use]
    pub fn is_bug(self) -> bool {
        matches!(self, Self::Assertion { .. } | Self::Crash)
    }

    /// Whether the endpoint can still be branched from.
    #[must_use]
    pub fn is_continuable(self) -> bool {
        matches!(self, Self::Deadline)
    }
}

/// One endpoint's observations: the guest's state registers, the `sometimes`
/// sites hit along the way, node liveness, and how the run ended.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultObservations {
    /// V-time of the endpoint.
    pub moment: u64,
    /// Agent reconcile ticks.
    pub ticks: u64,
    /// Bitmap of live nodes.
    pub alive: u64,
    /// Hooks spawned.
    pub hooks_started: u64,
    /// Hooks completed.
    pub hooks_finished: u64,
    /// Nodes that died with no fault active.
    pub unexpected_deaths: u64,
    /// Nodes the agent restarted.
    pub restarts: u64,
    /// Threads the guest kernel parked at a place.
    pub parked: u64,
    /// The `sometimes` bitmap the agent publishes for the first 48 sites.
    pub sometimes_register: u64,
    /// Every `sometimes` site hit, decoded from namespace-1 hits.
    pub sometimes: BTreeSet<u32>,
    /// Assertion violations decoded from namespace-1 violations.
    pub violations: BTreeSet<u32>,
    /// How the run ended.
    pub stop: FaultStop,
}

impl FaultObservations {
    /// Build observations from one endpoint's SDK capture and stop.
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
            sometimes_register: value(reg::SOMETIMES),
            sometimes: capture.sometimes.clone(),
            violations: capture.violations.clone(),
            stop,
        }
    }

    /// The `sometimes` set as the fixed-width bitmap the archive key carries.
    /// A site id at or beyond [`SOMETIMES_KEY_BITS`] cannot widen the key and
    /// is left out of it; the full set stays in the observation.
    #[must_use]
    pub fn sometimes_bitmap(&self) -> u64 {
        self.sometimes
            .iter()
            .filter(|id| **id < SOMETIMES_KEY_BITS)
            .fold(0_u64, |bits, id| bits | (1_u64 << id))
    }

    /// Whether this endpoint found a bug.
    #[must_use]
    pub fn is_bug(&self) -> bool {
        self.stop.is_bug() || !self.violations.is_empty()
    }

    /// Generic exit classification: a crashed guest yields no further evidence.
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
        assert_eq!(WINDOWS.window(0), (1_000, 1_000 + DEFAULT_HORIZON_NANOS));
        let (start, end) = WINDOWS.window(3);
        assert_eq!(start, 1_000 + 3 * DEFAULT_HORIZON_NANOS);
        assert_eq!(end, start + DEFAULT_HORIZON_NANOS);
        assert_eq!(WINDOWS.deadline(3), end);
        let short = ActionWindows {
            horizon_nanos: 100_000_000,
            ..WINDOWS
        };
        assert_eq!(short.window(3), (1_000 + 300_000_000, 1_000 + 400_000_000));
    }

    #[test]
    fn a_long_input_saturates_instead_of_wrapping() {
        let (start, end) = ActionWindows {
            root_seal: u64::MAX - 1,
            ..WINDOWS
        }
        .window(usize::MAX);
        assert_eq!(start, u64::MAX);
        assert_eq!(end, u64::MAX);
    }

    #[test]
    fn wait_installs_nothing() {
        assert_eq!(
            action_delta(FaultAction::Wait, WINDOWS.window(0)),
            ActionDelta::default()
        );
        assert!(standing_windows(WINDOWS, &[FaultAction::Wait, FaultAction::Wait]).is_empty());
    }

    #[test]
    fn kill_holds_the_whole_horizon_for_its_node() {
        let delta = action_delta(FaultAction::Kill(2), WINDOWS.window(1));
        let fault = delta.standing.expect("kill installs a standing fault");
        assert_eq!(fault.class, DecisionClass::Process.as_u16());
        assert_eq!(
            decode_process_target(&fault.target),
            Some((2, Fault::ProcKill))
        );
        assert_eq!((fault.start, fault.end), WINDOWS.window(1));
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
                let (start, end) = windows.window(0);
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
            let (start, end) = windows.window(0);
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
        let fault = action_delta(FaultAction::Hook(9), WINDOWS.window(0))
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
            WINDOWS.window(0),
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
        let (start, _) = WINDOWS.window(2);
        let delta = action_delta(FaultAction::Interrupt(0x30), WINDOWS.window(2));
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
            FaultAction::Wait,
            FaultAction::Interrupt(32),
            FaultAction::Kill(0),
        ];
        let faults = standing_windows(WINDOWS, &actions);
        assert_eq!(faults.len(), 2);
        assert_eq!((faults[0].start, faults[0].end), WINDOWS.window(0));
        assert_eq!((faults[1].start, faults[1].end), WINDOWS.window(3));
        assert!(
            faults
                .iter()
                .all(|fault| fault.class == DecisionClass::Process.as_u16())
        );
    }

    #[test]
    fn the_window_list_round_trips_through_the_shared_codec() {
        let actions = [FaultAction::Kill(1), FaultAction::Hook(2)];
        let windows = standing_windows(WINDOWS, &actions);
        let bytes = fault_policy::encode_windows(&windows).expect("encode");
        assert_eq!(
            fault_policy::decode_windows(&bytes).expect("decode"),
            windows
        );
    }

    #[test]
    fn the_window_list_is_a_function_of_the_actions_and_the_tiling() {
        let actions = [FaultAction::Restart(3), FaultAction::Wait];
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
