// SPDX-License-Identifier: AGPL-3.0-or-later

//! The fault-library action vocabulary, its environment delta, and the
//! decoding of one endpoint's guest-published evidence.
//!
//! Every function here is pure: an action list maps to standing faults and
//! staged host perturbations by arithmetic on the root seal `Moment` alone, and
//! an endpoint's evidence decodes from a captured SDK event page. Neither needs
//! a live guest, so both are exercised directly by unit tests.

use std::collections::{BTreeMap, BTreeSet};

use control_proto::{Moment, Reproducer, StopReason};
use environment::{
    DecisionClass, EnvSpec, Fault, FaultPolicy, Span, StandingFault, process_target,
};
use serde::{Deserialize, Serialize};

use crate::target::ExitKind;

/// Virtual nanoseconds one action runs for before its endpoint is sealed.
pub const HORIZON_NANOS: u64 = 2_000_000_000;
/// Virtual nanoseconds of one guest fault-agent reconcile tick. A `Pause`
/// duration is expressed in these because the agent can only observe a window
/// boundary on a tick.
pub const AGENT_TICK_NANOS: u64 = 10_000_000;
/// Virtual nanoseconds a `Restart` holds a node down before the agent sees the
/// window leave and starts it again. Held to a fraction of the horizon so the
/// restarted node is observable inside the same action.
pub const RESTART_DOWN_NANOS: u64 = HORIZON_NANOS / 4;
/// Largest action count one fault-library input may carry.
pub const MAX_FAULTLAB_ACTIONS: usize = 256;
/// Seed of the fault-library environment; the search perturbs the standing
/// fault list, never the seed.
pub const FAULTLAB_SEED: u64 = 0x6661_756c_746c_6162;

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

/// One total action of the fault-library vocabulary.
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
}

/// Portable campaign snapshot: the action prefix that reaches an endpoint plus
/// the endpoint's decoded evidence. Each evaluator maps the prefix back to its
/// own real whole-VM snapshot.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultlabSnapshot {
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
    /// Encoded `environment::HostFault` bytes.
    pub fault: Vec<u8>,
    /// The `Moment` the backend applies the fault at.
    pub at: Moment,
}

/// One action's environment delta over its own horizon window.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActionDelta {
    /// The Process-class standing fault the action installs, if any.
    pub standing: Option<StandingFault>,
    /// The host-plane perturbation the action stages, if any.
    pub perturb: Option<StagedPerturb>,
}

/// The half-open window the action at `index` owns, measured from the root
/// seal. Saturating so a long input can never wrap the V-time axis.
#[must_use]
pub fn action_window(root_seal: u64, index: usize) -> (u64, u64) {
    let offset = (index as u64).saturating_mul(HORIZON_NANOS);
    let start = root_seal.saturating_add(offset);
    (start, start.saturating_add(HORIZON_NANOS))
}

/// The deadline a run must reach for the action at `index` to be complete.
#[must_use]
pub fn action_deadline(root_seal: u64, index: usize) -> Moment {
    Moment(action_window(root_seal, index).1)
}

fn standing(target: Vec<u8>, window: (u64, u64)) -> StandingFault {
    StandingFault {
        class: DecisionClass::Process,
        target,
        window,
    }
}

/// Map one action to its environment delta over `window`.
#[must_use]
pub fn action_delta(action: FaultAction, window: (u64, u64)) -> ActionDelta {
    let (start, end) = window;
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
            // The pause must lift inside its own horizon, so the search always
            // observes the resumed node rather than inheriting a stopped one.
            let held = u64::from(ticks)
                .saturating_mul(AGENT_TICK_NANOS)
                .min(HORIZON_NANOS.saturating_sub(AGENT_TICK_NANOS))
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
                (start, start.saturating_add(RESTART_DOWN_NANOS)),
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
        FaultAction::Interrupt(vector) => ActionDelta {
            standing: None,
            perturb: Some(StagedPerturb {
                fault: environment::HostFault::InjectInterrupt { vector }.encode(),
                at: Moment(start),
            }),
        },
    }
}

/// Every action's delta in input order.
#[must_use]
pub fn action_deltas(root_seal: u64, actions: &[FaultAction]) -> Vec<ActionDelta> {
    actions
        .iter()
        .enumerate()
        .map(|(index, action)| action_delta(*action, action_window(root_seal, index)))
        .collect()
}

/// The standing-fault list an input installs, in input order.
#[must_use]
pub fn standing_faults(root_seal: u64, actions: &[FaultAction]) -> Vec<StandingFault> {
    action_deltas(root_seal, actions)
        .into_iter()
        .filter_map(|delta| delta.standing)
        .collect()
}

/// The environment an input reproduces from: the fixed fault-library seed with
/// the input's whole standing-fault list. Host-plane perturbations are staged
/// separately and the backend records them into this environment itself.
#[must_use]
pub fn reproducer(root_seal: u64, actions: &[FaultAction]) -> Reproducer {
    let mut spec = EnvSpec::Seeded {
        seed: FAULTLAB_SEED,
        policy: FaultPolicy::none(),
    };
    spec.set_standing(standing_faults(root_seal, actions));
    Reproducer {
        blob_version: EnvSpec::BLOB_VERSION,
        bytes: spec.encode(),
    }
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
    use environment::decode_process_target;

    const ROOT: u64 = 1_000;

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
        assert_eq!(action_window(ROOT, 0), (1_000, 1_000 + HORIZON_NANOS));
        let (start, end) = action_window(ROOT, 3);
        assert_eq!(start, 1_000 + 3 * HORIZON_NANOS);
        assert_eq!(end, start + HORIZON_NANOS);
        assert_eq!(action_deadline(ROOT, 3), Moment(end));
    }

    #[test]
    fn a_long_input_saturates_instead_of_wrapping() {
        let (start, end) = action_window(u64::MAX - 1, usize::MAX);
        assert_eq!(start, u64::MAX);
        assert_eq!(end, u64::MAX);
    }

    #[test]
    fn wait_installs_nothing() {
        assert_eq!(
            action_delta(FaultAction::Wait, action_window(ROOT, 0)),
            ActionDelta::default()
        );
        assert!(standing_faults(ROOT, &[FaultAction::Wait, FaultAction::Wait]).is_empty());
    }

    #[test]
    fn kill_holds_the_whole_horizon_for_its_node() {
        let delta = action_delta(FaultAction::Kill(2), action_window(ROOT, 1));
        let fault = delta.standing.expect("kill installs a standing fault");
        assert_eq!(fault.class, DecisionClass::Process);
        assert_eq!(
            decode_process_target(&fault.target),
            Some((2, Fault::ProcKill))
        );
        assert_eq!(fault.window, action_window(ROOT, 1));
        assert!(delta.perturb.is_none());
    }

    #[test]
    fn pause_lifts_inside_its_own_horizon() {
        for ticks in [0_u32, 1, 7, u32::MAX] {
            let (start, end) = action_window(ROOT, 0);
            let delta = action_delta(FaultAction::Pause(1, ticks), (start, end));
            let fault = delta.standing.expect("pause installs a standing fault");
            let (node, decoded) = decode_process_target(&fault.target).expect("decode");
            assert_eq!(node, 1);
            let held = match decoded {
                Fault::ProcPause(Span(held)) => held,
                other => panic!("pause encoded as {other:?}"),
            };
            assert_eq!(fault.window.0, start);
            assert_eq!(fault.window.1, start + held);
            assert!(fault.window.1 < end, "a pause must lift before the horizon");
            assert!(held >= AGENT_TICK_NANOS, "a pause must span a whole tick");
        }
    }

    #[test]
    fn restart_leaves_its_window_inside_the_horizon() {
        let (start, end) = action_window(ROOT, 0);
        let fault = action_delta(FaultAction::Restart(0), (start, end))
            .standing
            .expect("restart installs a standing fault");
        assert_eq!(
            decode_process_target(&fault.target),
            Some((0, Fault::ProcRestart))
        );
        assert_eq!(fault.window.1, start + RESTART_DOWN_NANOS);
        assert!(fault.window.1 < end);
    }

    #[test]
    fn hook_targets_the_agent_rather_than_a_node() {
        let fault = action_delta(FaultAction::Hook(9), action_window(ROOT, 0))
            .standing
            .expect("hook installs a standing fault");
        assert_eq!(
            decode_process_target(&fault.target),
            Some((0, Fault::RunHook(9)))
        );
    }

    #[test]
    fn interrupt_stages_a_host_fault_and_no_standing_fault() {
        let (start, _) = action_window(ROOT, 2);
        let delta = action_delta(FaultAction::Interrupt(0x30), action_window(ROOT, 2));
        assert!(delta.standing.is_none());
        let perturb = delta.perturb.expect("interrupt stages a host fault");
        assert_eq!(perturb.at, Moment(start));
        assert_eq!(
            environment::HostFault::decode(&perturb.fault),
            Ok(environment::HostFault::InjectInterrupt { vector: 0x30 })
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
        let faults = standing_faults(ROOT, &actions);
        assert_eq!(faults.len(), 2);
        assert_eq!(faults[0].window, action_window(ROOT, 0));
        assert_eq!(faults[1].window, action_window(ROOT, 3));
        assert!(
            faults
                .iter()
                .all(|fault| fault.class == DecisionClass::Process)
        );
    }

    #[test]
    fn the_reproducer_round_trips_the_standing_list() {
        let actions = [FaultAction::Kill(1), FaultAction::Hook(2)];
        let reproducer = reproducer(ROOT, &actions);
        assert_eq!(reproducer.blob_version, EnvSpec::BLOB_VERSION);
        let spec = EnvSpec::decode(&reproducer.bytes).expect("decode reproducer");
        // The encoder canonicalizes the list, so compare it as a set.
        let key = |fault: &StandingFault| (fault.target.clone(), fault.window);
        let mut decoded = spec.standing().iter().map(key).collect::<Vec<_>>();
        let mut expected = standing_faults(ROOT, &actions)
            .iter()
            .map(key)
            .collect::<Vec<_>>();
        decoded.sort();
        expected.sort();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn the_reproducer_is_a_function_of_the_actions_alone() {
        let actions = [FaultAction::Restart(3), FaultAction::Wait];
        assert_eq!(reproducer(ROOT, &actions), reproducer(ROOT, &actions));
        assert_ne!(reproducer(ROOT, &actions), reproducer(2_000, &actions));
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
