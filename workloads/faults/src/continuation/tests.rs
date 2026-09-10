use super::{ContinuationRuntime, ContinuationStop, RecordedContinuation};
use crate::{
    action_execution::ActionRuntime,
    checkpoint::Checkpoint,
    execution::ActionCursor,
    retained::RetainedContinuation,
    target::{ActionWindows, FaultAction},
};
use control_proto::{Moment, SnapId, StopReason};
use environment::{channel::Effect, input_spec::ServiceConfig};
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, collections::BTreeMap, error::Error, rc::Rc};

#[test]
fn an_observer_stops_before_the_next_action_and_retains_that_position_cold() {
    let windows = ActionWindows {
        root_seal: 100,
        horizon_nanos: 20,
    };
    let runtime = ModelRuntime::new(100, 17);
    let view = runtime.clone();
    let mut continuation =
        RecordedContinuation::from_setup(runtime, windows, vec![FaultAction::Wait; 3]);
    let mut before_calls = 0;
    let mut observed = Vec::new();
    let stop = continuation
        .drive_observed(
            Some(160),
            false,
            &mut |_| {
                before_calls += 1;
                Ok(())
            },
            &mut |runtime, stop| {
                observed.push((runtime.state().time, stop.clone()));
                Ok(runtime.state().time == 120)
            },
        )
        .expect("observer returns first observed endpoint");
    assert_eq!(
        stop,
        ContinuationStop::Guest(StopReason::Deadline { vtime: Moment(120) })
    );
    assert_eq!(before_calls, 1);
    assert_eq!(observed.len(), 1);
    assert_eq!(view.trace.borrow().branch_calls, 1);
    assert_eq!(
        continuation.cursor(),
        ActionCursor::from_parts(1, false).unwrap()
    );
    let saved = continuation.checkpoint().unwrap();
    let cold_runtime = ModelRuntime::new(100, 99);
    let cold_view = cold_runtime.clone();
    let mut cold = RecordedContinuation::restore(cold_runtime, &saved).unwrap();
    assert_eq!(cold_view.trace.borrow().run_calls, 0);
    assert_eq!(cold.reproduce().unwrap(), ContinuationStop::RecordedEnd);
    assert_eq!(cold_view.trace.borrow().branch_calls, 2);
    continuation.reproduce().unwrap();
    assert_eq!(view.state(), cold_view.state());
}

#[test]
fn watchdog_and_observation_failures_poison_before_another_segment() {
    let windows = ActionWindows {
        root_seal: 100,
        horizon_nanos: 20,
    };
    for fail_before in [true, false] {
        let runtime = ModelRuntime::new(100, 17);
        let view = runtime.clone();
        let mut continuation =
            RecordedContinuation::from_setup(runtime, windows, vec![FaultAction::Wait; 3]);
        let failure = continuation
            .drive_observed(
                Some(160),
                false,
                &mut |_| {
                    if fail_before {
                        Err(error("watchdog expired"))
                    } else {
                        Ok(())
                    }
                },
                &mut |_, _| Err(error("event read failed")),
            )
            .expect_err("failed callback aborts the operation");
        assert!(failure.to_string().contains(if fail_before {
            "watchdog expired"
        } else {
            "event read failed"
        }));
        let calls = view.trace.borrow().run_calls;
        assert_eq!(calls, usize::from(!fail_before));
        assert!(continuation.reproduce().is_err());
        assert!(continuation.checkpoint().is_err());
        assert_eq!(view.trace.borrow().run_calls, calls);
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum ModelEffect {
    WriteMemory { gpa: u64, bytes: Vec<u8> },
    XorMemory { gpa: u64, bytes: Vec<u8> },
    InjectInterrupt { vector: u32 },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ScheduledEffect {
    at: u64,
    effect: ModelEffect,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ModelEvent {
    at: u64,
    kind: u8,
    value: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ModelState {
    time: u64,
    entropy: u64,
    value: u64,
    environment: Vec<u8>,
    pending: Vec<ScheduledEffect>,
    events: Vec<ModelEvent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActivationRecord {
    parent: SnapId,
    config: ServiceConfig,
    effects: Vec<(u64, Effect)>,
}

#[derive(Debug)]
struct RuntimeTrace {
    state: ModelState,
    snapshots: BTreeMap<SnapId, ModelState>,
    next_snapshot: u64,
    capture_calls: usize,
    export_calls: usize,
    release_calls: usize,
    restore_calls: usize,
    branch_calls: usize,
    reseed_calls: usize,
    run_calls: usize,
    steps: usize,
    run_deadlines: Vec<u64>,
    activations: Vec<ActivationRecord>,
    assertion_once_at: Option<u64>,
    capture_drift: bool,
    export_error: Option<String>,
    release_error: Option<String>,
    restore_error: Option<String>,
    run_error: Option<String>,
    overshoot: u64,
}

#[derive(Clone)]
struct ModelRuntime {
    trace: Rc<RefCell<RuntimeTrace>>,
}

impl ModelRuntime {
    fn new(at: u64, entropy: u64) -> Self {
        let pending_at = at.checked_add(50).expect("model seed time fits");
        Self {
            trace: Rc::new(RefCell::new(RuntimeTrace {
                state: ModelState {
                    time: at,
                    entropy,
                    value: entropy.rotate_left(7),
                    environment: Vec::new(),
                    pending: vec![ScheduledEffect {
                        at: pending_at,
                        effect: ModelEffect::XorMemory {
                            gpa: 0x2000,
                            bytes: vec![0xa5, 0x5a],
                        },
                    }],
                    events: Vec::new(),
                },
                snapshots: BTreeMap::new(),
                next_snapshot: 1,
                capture_calls: 0,
                export_calls: 0,
                release_calls: 0,
                restore_calls: 0,
                branch_calls: 0,
                reseed_calls: 0,
                run_calls: 0,
                steps: 0,
                run_deadlines: Vec::new(),
                activations: Vec::new(),
                assertion_once_at: None,
                capture_drift: false,
                export_error: None,
                release_error: None,
                restore_error: None,
                run_error: None,
                overshoot: 0,
            })),
        }
    }

    fn state(&self) -> ModelState {
        self.trace.borrow().state.clone()
    }
}

fn error(message: impl Into<String>) -> Box<dyn Error> {
    std::io::Error::other(message.into()).into()
}

fn expect_error<T>(result: Result<T, Box<dyn Error>>, message: &str) -> Box<dyn Error> {
    match result {
        Ok(_) => panic!("{message}"),
        Err(error) => error,
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

fn next_entropy(entropy: u64) -> u64 {
    entropy
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407)
}

fn model_effect(effect: &Effect) -> ModelEffect {
    match effect {
        Effect::WriteMemory { gpa, bytes } => ModelEffect::WriteMemory {
            gpa: *gpa,
            bytes: bytes.clone(),
        },
        Effect::XorMemory { gpa, bytes } => ModelEffect::XorMemory {
            gpa: *gpa,
            bytes: bytes.clone(),
        },
        Effect::InjectInterrupt { vector } => ModelEffect::InjectInterrupt { vector: *vector },
    }
}

fn model_effect_value(effect: &ModelEffect) -> u64 {
    match effect {
        ModelEffect::WriteMemory { gpa, bytes } => gpa.wrapping_add(hash_bytes(bytes)),
        ModelEffect::XorMemory { gpa, bytes } => gpa.rotate_left(13) ^ hash_bytes(bytes),
        ModelEffect::InjectInterrupt { vector } => u64::from(*vector),
    }
}

impl RuntimeTrace {
    fn apply_due_effects(&mut self) {
        let now = self.state.time;
        let pending = std::mem::take(&mut self.state.pending);
        let mut remaining = Vec::with_capacity(pending.len());
        for scheduled in pending {
            if scheduled.at <= now {
                self.state.value = self
                    .state
                    .value
                    .wrapping_add(model_effect_value(&scheduled.effect));
                self.state.events.push(ModelEvent {
                    at: now,
                    kind: 2,
                    value: model_effect_value(&scheduled.effect),
                });
            } else {
                remaining.push(scheduled);
            }
        }
        self.state.pending = remaining;
    }

    fn tick(&mut self) -> Result<(), Box<dyn Error>> {
        self.state.time = self
            .state
            .time
            .checked_add(1)
            .ok_or_else(|| error("model virtual time overflows"))?;
        self.steps += 1;
        self.state.entropy = next_entropy(self.state.entropy);
        self.state.value = self
            .state
            .value
            .wrapping_add(self.state.entropy ^ self.state.time);
        self.state.events.push(ModelEvent {
            at: self.state.time,
            kind: 1,
            value: self.state.entropy,
        });
        self.apply_due_effects();
        Ok(())
    }
}

impl ActionRuntime for ModelRuntime {
    fn activate_prefix(
        &mut self,
        parent: SnapId,
        config: ServiceConfig,
        effects: Vec<(u64, Effect)>,
    ) -> Result<(), Box<dyn Error>> {
        let mut trace = self.trace.borrow_mut();
        let parent_state = trace
            .snapshots
            .get(&parent)
            .cloned()
            .ok_or_else(|| error(format!("unknown model parent {parent:?}")))?;
        trace.branch_calls += 1;

        let mut reseed = parent_state.entropy ^ hash_bytes(&config.encode());
        for (at, effect) in &effects {
            reseed ^= at.rotate_left(17) ^ hash_bytes(&effect.encode());
        }
        trace.state = parent_state;
        trace.state.environment = config.configuration.clone();
        trace.state.entropy = next_entropy(reseed);
        trace.state.value = trace.state.value.wrapping_add(reseed);
        let at = trace.state.time;
        let entropy = trace.state.entropy;
        trace.state.events.push(ModelEvent {
            at,
            kind: 3,
            value: entropy,
        });
        trace
            .state
            .pending
            .extend(effects.iter().map(|(at, effect)| ScheduledEffect {
                at: *at,
                effect: model_effect(effect),
            }));
        trace.reseed_calls += 1;
        trace.activations.push(ActivationRecord {
            parent,
            config,
            effects,
        });
        Ok(())
    }

    fn run_action_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
        let mut trace = self.trace.borrow_mut();
        trace.run_calls += 1;
        trace.run_deadlines.push(deadline);
        if let Some(message) = trace.run_error.take() {
            return Err(error(message));
        }
        trace.apply_due_effects();
        if deadline <= trace.state.time {
            return Ok(StopReason::Deadline {
                vtime: Moment(trace.state.time),
            });
        }
        let actual = deadline
            .checked_add(trace.overshoot)
            .ok_or_else(|| error("model run target overflows"))?;
        if trace
            .assertion_once_at
            .is_some_and(|assertion_at| assertion_at <= actual)
        {
            let assertion_at = trace
                .assertion_once_at
                .take()
                .expect("assertion stop was configured");
            let stop_at = assertion_at.max(trace.state.time);
            while trace.state.time < stop_at {
                trace.tick()?;
            }
            return Ok(StopReason::Assertion {
                vtime: Moment(trace.state.time),
                ev: control_proto::EventRef {
                    id: 99,
                    data: vec![0x42],
                },
            });
        }
        while trace.state.time < actual {
            trace.tick()?;
        }
        Ok(StopReason::Deadline {
            vtime: Moment(trace.state.time),
        })
    }
}

impl ContinuationRuntime for ModelRuntime {
    fn capture(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
        let mut trace = self.trace.borrow_mut();
        let snapshot = SnapId(trace.next_snapshot);
        trace.next_snapshot = trace
            .next_snapshot
            .checked_add(1)
            .ok_or_else(|| error("model snapshot handle overflows"))?;
        let at = trace.state.time;
        let state = trace.state.clone();
        trace.snapshots.insert(snapshot, state);
        trace.capture_calls += 1;
        let reported_at = if trace.capture_drift {
            at.checked_add(1)
                .ok_or_else(|| error("model capture time overflows"))?
        } else {
            at
        };
        Ok((snapshot, reported_at))
    }

    fn release(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        let mut trace = self.trace.borrow_mut();
        trace.release_calls += 1;
        let existed = trace.snapshots.remove(&snapshot).is_some();
        if let Some(message) = trace.release_error.take() {
            return Err(error(message));
        }
        if existed {
            Ok(())
        } else {
            Err(error(format!("unknown model snapshot {snapshot:?}")))
        }
    }

    fn export(&mut self, snapshot: SnapId, at: u64) -> Result<Checkpoint, Box<dyn Error>> {
        let mut trace = self.trace.borrow_mut();
        trace.export_calls += 1;
        if let Some(message) = trace.export_error.take() {
            return Err(error(message));
        }
        let state = trace
            .snapshots
            .get(&snapshot)
            .cloned()
            .ok_or_else(|| error(format!("unknown model snapshot {snapshot:?}")))?;
        if state.time != at {
            return Err(error("model export endpoint differs from snapshot"));
        }
        Ok(Checkpoint {
            setup: 1,
            at,
            image_identity: [0x42; 32],
            pages: Vec::new(),
            sidecar: serde_json::to_vec(&state)?,
            cursor: None,
        })
    }

    fn restore(&mut self, checkpoint: &Checkpoint) -> Result<(), Box<dyn Error>> {
        let mut trace = self.trace.borrow_mut();
        trace.restore_calls += 1;
        if let Some(message) = trace.restore_error.take() {
            return Err(error(message));
        }
        let state: ModelState = serde_json::from_slice(&checkpoint.sidecar)?;
        if state.time != checkpoint.at {
            return Err(error("model restore endpoint differs from checkpoint"));
        }
        trace.state = state;
        Ok(())
    }
}

fn retained(
    checkpoint: Checkpoint,
    windows: ActionWindows,
    actions: Vec<FaultAction>,
) -> RetainedContinuation {
    RetainedContinuation {
        checkpoint,
        windows,
        actions,
    }
}

fn checkpoint_for(at: u64, cursor: Option<ActionCursor>) -> Checkpoint {
    Checkpoint {
        setup: 1,
        at,
        image_identity: [0x42; 32],
        pages: Vec::new(),
        sidecar: serde_json::to_vec(&ModelState {
            time: at,
            entropy: 0,
            value: 0,
            environment: Vec::new(),
            pending: Vec::new(),
            events: Vec::new(),
        })
        .expect("encode model state"),
        cursor,
    }
}

#[test]
fn reproduce_split_checkpoint_and_cold_restore_match_a_stateful_runtime() {
    let windows = ActionWindows {
        root_seal: 100,
        horizon_nanos: 20,
    };
    let actions = vec![
        FaultAction::Interrupt(31),
        FaultAction::Hook(7),
        FaultAction::Wait,
    ];
    let parent = SnapId(1);
    let seed = 0x1234_5678_9abc_def0;

    let warm_runtime = ModelRuntime::new(windows.root_seal, seed);
    let warm_view = warm_runtime.clone();
    let mut warm = RecordedContinuation::from_setup(warm_runtime, windows, actions.clone());
    assert_eq!(
        warm.reproduce().expect("warm reproduce"),
        ContinuationStop::RecordedEnd
    );
    let warm_state = warm_view.state();
    let warm_cursor = warm.cursor();
    assert_eq!(warm.at(), 160);
    assert_eq!(warm_cursor, ActionCursor::from_parts(3, false).unwrap());
    assert_eq!(warm_view.trace.borrow().activations.len(), 3);

    let split_runtime = ModelRuntime::new(windows.root_seal, seed);
    let split_view = split_runtime.clone();
    let mut split = RecordedContinuation::from_setup(split_runtime, windows, actions.clone());
    let split_at = 107;
    assert_eq!(
        split.advance_to(split_at, false).expect("bounded split"),
        ContinuationStop::BoundReached
    );
    assert_eq!(split.at(), split_at);
    assert_eq!(split.cursor(), ActionCursor::from_parts(0, true).unwrap());
    let before_checkpoint_state = split_view.state();
    assert!(!before_checkpoint_state.pending.is_empty());
    let before_checkpoint_steps = split_view.trace.borrow().steps;
    let retained = split.checkpoint().expect("split checkpoint");
    let split_cursor = split.cursor();
    assert_eq!(split_view.state(), before_checkpoint_state);
    assert_eq!(split_view.trace.borrow().steps, before_checkpoint_steps);
    assert_eq!(split_view.trace.borrow().capture_calls, 2);
    assert_eq!(split_view.trace.borrow().export_calls, 1);
    assert_eq!(split_view.trace.borrow().release_calls, 2);
    assert_eq!(retained.checkpoint.cursor, Some(split_cursor));
    assert_eq!(retained.windows, windows);
    assert_eq!(retained.actions, actions);
    let split_prefix_activations = split_view.trace.borrow().activations.clone();

    assert_eq!(
        split.reproduce().expect("continue after saving"),
        ContinuationStop::RecordedEnd
    );
    assert_eq!(split.at(), warm.at());
    assert_eq!(split.cursor(), warm_cursor);
    assert_eq!(split_view.state(), warm_state);

    let retained_bytes = retained.encode().expect("encode retained continuation");
    let decoded_retained =
        RetainedContinuation::decode(&retained_bytes).expect("decode retained continuation");
    assert_eq!(decoded_retained, retained);
    let machine_bytes = retained
        .checkpoint
        .encode()
        .expect("encode machine checkpoint");
    assert_eq!(
        Checkpoint::decode(&machine_bytes).expect("decode machine checkpoint"),
        retained.checkpoint
    );

    let cold_runtime = ModelRuntime::new(windows.root_seal, 0xfeed_face_cafe_beef);
    let cold_view = cold_runtime.clone();
    let mut cold = RecordedContinuation::restore(cold_runtime, &decoded_retained)
        .expect("restore into a fresh model runtime");
    {
        let trace = cold_view.trace.borrow();
        assert_eq!(trace.restore_calls, 1);
        assert_eq!(trace.branch_calls, 0);
        assert_eq!(trace.reseed_calls, 0);
        assert_eq!(trace.steps, 0);
        assert_eq!(trace.run_calls, 0);
    }
    assert_eq!(cold.at(), split_at);
    assert_eq!(cold.cursor(), split_cursor);
    assert_eq!(cold_view.state(), before_checkpoint_state);
    assert_eq!(cold_view.state().pending, before_checkpoint_state.pending);

    assert_eq!(
        cold.reproduce().expect("cold continuation"),
        ContinuationStop::RecordedEnd
    );
    assert_eq!(cold.at(), warm.at());
    assert_eq!(cold.cursor(), warm_cursor);
    assert_eq!(cold_view.state(), warm_state);
    assert_eq!(cold_view.trace.borrow().branch_calls, 2);
    assert_eq!(cold_view.trace.borrow().reseed_calls, 2);

    let mut continued_activations = split_prefix_activations;
    continued_activations.extend(cold_view.trace.borrow().activations.clone());
    let activation_shape = |records: &[ActivationRecord]| {
        records
            .iter()
            .map(|record| (record.config.clone(), record.effects.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        activation_shape(&continued_activations),
        activation_shape(&warm_view.trace.borrow().activations)
    );
    assert_eq!(
        cold_view.trace.borrow().state.events,
        warm_view.trace.borrow().state.events
    );
    assert_eq!(
        cold_view.trace.borrow().state.environment,
        warm_state.environment
    );
    assert_eq!(parent, warm_view.trace.borrow().activations[0].parent);
}

#[test]
fn reproduce_uses_ordinal_actions_after_multiple_time_overshoots() {
    let windows = ActionWindows {
        root_seal: 100,
        horizon_nanos: 20,
    };
    let actions = vec![
        FaultAction::Hook(1),
        FaultAction::Hook(2),
        FaultAction::Hook(3),
    ];
    let runtime = ModelRuntime::new(windows.root_seal, 0xabc);
    let view = runtime.clone();
    view.trace.borrow_mut().overshoot = 50;
    let mut continuation = RecordedContinuation::from_setup(runtime, windows, actions);

    assert_eq!(
        continuation.reproduce().expect("overshooting reproduce"),
        ContinuationStop::RecordedEnd
    );
    assert_eq!(
        continuation.cursor(),
        ActionCursor::from_parts(3, false).unwrap()
    );
    assert_eq!(continuation.at(), 170);
    assert_eq!(view.trace.borrow().steps, 70);
    assert_eq!(view.trace.borrow().run_deadlines, vec![120, 140, 160]);
    assert_eq!(view.trace.borrow().activations.len(), 3);
    for (index, activation) in view.trace.borrow().activations.iter().enumerate() {
        let standing = fault_policy::decode_windows(&activation.config.configuration)
            .expect("decode ordinal prefix configuration");
        assert_eq!(standing.len(), index + 1);
    }
}

#[test]
fn zero_horizon_reproduction_stays_at_one_moment_while_a_bound_stops_before_activation() {
    let windows = ActionWindows {
        root_seal: 2_000,
        horizon_nanos: 0,
    };
    let actions = vec![FaultAction::Hook(11), FaultAction::Hook(12)];

    let reproduce_runtime = ModelRuntime::new(windows.root_seal, 0xa1);
    let reproduce_view = reproduce_runtime.clone();
    reproduce_view.trace.borrow_mut().overshoot = 7;
    let mut reproduce =
        RecordedContinuation::from_setup(reproduce_runtime, windows, actions.clone());
    assert_eq!(
        reproduce.reproduce().expect("same-moment reproduction"),
        ContinuationStop::RecordedEnd
    );
    assert_eq!(reproduce.at(), windows.root_seal);
    assert_eq!(
        reproduce.cursor(),
        ActionCursor::from_parts(actions.len() as u64, false).unwrap()
    );
    assert_eq!(reproduce_view.trace.borrow().branch_calls, actions.len());
    assert_eq!(reproduce_view.trace.borrow().reseed_calls, actions.len());
    assert_eq!(reproduce_view.trace.borrow().run_calls, actions.len());
    assert_eq!(reproduce_view.trace.borrow().steps, 0);

    let bounded_runtime = ModelRuntime::new(windows.root_seal, 0xa1);
    let bounded_view = bounded_runtime.clone();
    let mut bounded = RecordedContinuation::from_setup(bounded_runtime, windows, actions);
    assert_eq!(
        bounded
            .advance_to(windows.root_seal, false)
            .expect("same-moment bound"),
        ContinuationStop::BoundReached
    );
    assert_eq!(bounded.at(), windows.root_seal);
    assert_eq!(bounded.cursor(), ActionCursor::default());
    assert_eq!(bounded_view.trace.borrow().branch_calls, 0);
    assert_eq!(bounded_view.trace.borrow().reseed_calls, 0);
    assert_eq!(bounded_view.trace.borrow().run_calls, 0);
    assert_eq!(bounded_view.trace.borrow().steps, 0);
}

#[test]
fn assertion_stop_can_be_checkpointed_and_resumed_cold_without_reactivation() {
    let windows = ActionWindows {
        root_seal: 100,
        horizon_nanos: 20,
    };
    let actions = vec![FaultAction::Hook(1), FaultAction::Wait];
    let seed = 0xb2;

    let reference_runtime = ModelRuntime::new(windows.root_seal, seed);
    let reference_view = reference_runtime.clone();
    let mut reference =
        RecordedContinuation::from_setup(reference_runtime, windows, actions.clone());
    assert_eq!(
        reference.reproduce().expect("uninterrupted reproduction"),
        ContinuationStop::RecordedEnd
    );

    let interrupted_runtime = ModelRuntime::new(windows.root_seal, seed);
    let interrupted_view = interrupted_runtime.clone();
    interrupted_view.trace.borrow_mut().assertion_once_at = Some(110);
    let mut interrupted =
        RecordedContinuation::from_setup(interrupted_runtime, windows, actions.clone());
    let stop = interrupted.reproduce().expect("surface assertion stop");
    assert!(
        matches!(
            stop,
            ContinuationStop::Guest(StopReason::Assertion {
                vtime: Moment(110),
                ..
            })
        ),
        "{stop:?}"
    );
    assert_eq!(interrupted.at(), 110);
    assert_eq!(
        interrupted.cursor(),
        ActionCursor::from_parts(0, true).unwrap()
    );

    let state_before_checkpoint = interrupted_view.state();
    let steps_before_checkpoint = interrupted_view.trace.borrow().steps;
    let retained = interrupted.checkpoint().expect("checkpoint assertion stop");
    assert_eq!(interrupted_view.state(), state_before_checkpoint);
    assert_eq!(
        interrupted_view.trace.borrow().steps,
        steps_before_checkpoint
    );

    assert_eq!(
        interrupted
            .reproduce()
            .expect("continue after assertion save"),
        ContinuationStop::RecordedEnd
    );
    assert_eq!(interrupted.at(), reference.at());
    assert_eq!(interrupted.cursor(), reference.cursor());
    assert_eq!(interrupted_view.state(), reference_view.state());

    let retained_bytes = retained.encode().expect("encode assertion continuation");
    let decoded =
        RetainedContinuation::decode(&retained_bytes).expect("decode assertion continuation");
    assert_eq!(decoded, retained);

    let cold_runtime = ModelRuntime::new(windows.root_seal, 0xc3);
    let cold_view = cold_runtime.clone();
    let mut cold =
        RecordedContinuation::restore(cold_runtime, &decoded).expect("cold assertion restore");
    {
        let trace = cold_view.trace.borrow();
        assert_eq!(trace.restore_calls, 1);
        assert_eq!(trace.branch_calls, 0);
        assert_eq!(trace.reseed_calls, 0);
        assert_eq!(trace.run_calls, 0);
        assert_eq!(trace.steps, 0);
    }
    assert_eq!(cold.at(), 110);
    assert_eq!(cold.cursor(), ActionCursor::from_parts(0, true).unwrap());
    assert_eq!(cold_view.state(), state_before_checkpoint);

    assert_eq!(
        cold.reproduce().expect("continue cold after assertion"),
        ContinuationStop::RecordedEnd
    );
    assert_eq!(cold.at(), reference.at());
    assert_eq!(cold.cursor(), reference.cursor());
    assert_eq!(cold_view.state(), reference_view.state());
    assert_eq!(cold_view.state().events, reference_view.state().events);
}

#[test]
fn advance_to_stops_at_target_before_activating_the_next_action() {
    let windows = ActionWindows {
        root_seal: 100,
        horizon_nanos: 20,
    };
    let actions = vec![FaultAction::Hook(1), FaultAction::Interrupt(32)];
    let runtime = ModelRuntime::new(windows.root_seal, 0x55);
    let view = runtime.clone();
    let mut continuation = RecordedContinuation::from_setup(runtime, windows, actions);

    assert_eq!(
        continuation
            .advance_to(windows.deadline(0), false)
            .expect("advance to first endpoint"),
        ContinuationStop::BoundReached
    );
    assert_eq!(continuation.at(), windows.deadline(0));
    assert_eq!(
        continuation.cursor(),
        ActionCursor::from_parts(1, false).unwrap()
    );
    assert_eq!(view.trace.borrow().branch_calls, 1);
    assert_eq!(view.trace.borrow().reseed_calls, 1);
    assert_eq!(view.trace.borrow().activations.len(), 1);
    assert!(view.trace.borrow().activations[0].effects.is_empty());
    let standing =
        fault_policy::decode_windows(&view.trace.borrow().activations[0].config.configuration)
            .expect("decode first prefix configuration");
    assert_eq!(standing.len(), 1);
}

#[test]
fn extending_after_recorded_end_retains_the_final_environment() {
    let windows = ActionWindows {
        root_seal: 100,
        horizon_nanos: 20,
    };
    let runtime = ModelRuntime::new(windows.root_seal, 0x66);
    let view = runtime.clone();
    let mut continuation =
        RecordedContinuation::from_setup(runtime, windows, vec![FaultAction::Hook(9)]);
    assert_eq!(
        continuation.reproduce().expect("recorded action"),
        ContinuationStop::RecordedEnd
    );
    let environment = view.state().environment;
    let branches = view.trace.borrow().branch_calls;
    let activations = view.trace.borrow().activations.len();
    let steps = view.trace.borrow().steps;

    assert_eq!(
        continuation
            .advance_to(windows.deadline(0) + 10, true)
            .expect("extend final environment"),
        ContinuationStop::BoundReached
    );
    assert_eq!(continuation.at(), windows.deadline(0) + 10);
    assert_eq!(
        continuation.cursor(),
        ActionCursor::from_parts(1, false).unwrap()
    );
    assert_eq!(view.state().environment, environment);
    assert_eq!(view.trace.borrow().branch_calls, branches);
    assert_eq!(view.trace.borrow().reseed_calls, branches);
    assert_eq!(view.trace.borrow().activations.len(), activations);
    assert_eq!(view.trace.borrow().steps, steps + 10);
}

#[test]
fn capture_drift_reports_context_and_poisons_the_controller() {
    let windows = ActionWindows {
        root_seal: 100,
        horizon_nanos: 20,
    };
    let runtime = ModelRuntime::new(windows.root_seal, 0x77);
    let view = runtime.clone();
    view.trace.borrow_mut().capture_drift = true;
    let mut continuation =
        RecordedContinuation::from_setup(runtime, windows, vec![FaultAction::Wait]);
    let error = expect_error(
        continuation.reproduce(),
        "capture drift must fail reproduction",
    );
    assert!(
        error
            .to_string()
            .contains("activation snapshot changed the stopped moment"),
        "{error}"
    );
    assert_eq!(view.trace.borrow().capture_calls, 1);
    assert_eq!(view.trace.borrow().release_calls, 1);
    assert_eq!(view.trace.borrow().steps, 0);
    let poisoned = expect_error(
        continuation.reproduce(),
        "failed continuation must remain poisoned",
    );
    assert!(
        poisoned
            .to_string()
            .contains("restore a retained checkpoint"),
        "{poisoned}"
    );
    assert_eq!(view.trace.borrow().capture_calls, 1);

    let checkpoint_runtime = ModelRuntime::new(windows.root_seal, 0x78);
    let checkpoint_view = checkpoint_runtime.clone();
    checkpoint_view.trace.borrow_mut().capture_drift = true;
    let mut checkpoint_continuation =
        RecordedContinuation::from_setup(checkpoint_runtime, windows, vec![FaultAction::Wait]);
    let error = expect_error(
        checkpoint_continuation.checkpoint(),
        "checkpoint capture drift must fail",
    );
    assert!(
        error
            .to_string()
            .contains("checkpoint capture changed the stopped moment"),
        "{error}"
    );
    assert_eq!(checkpoint_view.trace.borrow().capture_calls, 1);
    assert_eq!(checkpoint_view.trace.borrow().release_calls, 1);
    let poisoned = expect_error(
        checkpoint_continuation.checkpoint(),
        "failed checkpoint continuation must remain poisoned",
    );
    assert!(
        poisoned
            .to_string()
            .contains("restore a retained checkpoint"),
        "{poisoned}"
    );
    assert_eq!(checkpoint_view.trace.borrow().capture_calls, 1);
}

#[test]
fn export_and_release_failures_report_diagnostics_and_poison() {
    let windows = ActionWindows {
        root_seal: 100,
        horizon_nanos: 20,
    };

    let export_runtime = ModelRuntime::new(windows.root_seal, 0x81);
    let export_view = export_runtime.clone();
    export_view.trace.borrow_mut().export_error = Some("export failed".to_owned());
    let mut export_continuation =
        RecordedContinuation::from_setup(export_runtime, windows, vec![FaultAction::Wait]);
    let error = expect_error(
        export_continuation.checkpoint(),
        "export failure must surface",
    );
    assert!(error.to_string().contains("export failed"), "{error}");
    assert_eq!(export_view.trace.borrow().capture_calls, 1);
    assert_eq!(export_view.trace.borrow().export_calls, 1);
    assert_eq!(export_view.trace.borrow().release_calls, 1);
    let poisoned = expect_error(
        export_continuation.checkpoint(),
        "export failure must poison",
    );
    assert!(
        poisoned
            .to_string()
            .contains("restore a retained checkpoint"),
        "{poisoned}"
    );

    let release_runtime = ModelRuntime::new(windows.root_seal, 0x82);
    let release_view = release_runtime.clone();
    release_view.trace.borrow_mut().release_error = Some("release failed".to_owned());
    let mut release_continuation =
        RecordedContinuation::from_setup(release_runtime, windows, vec![FaultAction::Wait]);
    let error = expect_error(
        release_continuation.checkpoint(),
        "release failure must surface",
    );
    assert!(error.to_string().contains("release failed"), "{error}");
    assert_eq!(release_view.trace.borrow().capture_calls, 1);
    assert_eq!(release_view.trace.borrow().export_calls, 1);
    assert_eq!(release_view.trace.borrow().release_calls, 1);
    let poisoned = expect_error(
        release_continuation.checkpoint(),
        "release failure must poison",
    );
    assert!(
        poisoned
            .to_string()
            .contains("restore a retained checkpoint"),
        "{poisoned}"
    );

    let both_runtime = ModelRuntime::new(windows.root_seal, 0x83);
    let both_view = both_runtime.clone();
    {
        let mut trace = both_view.trace.borrow_mut();
        trace.export_error = Some("export failed".to_owned());
        trace.release_error = Some("release failed".to_owned());
    }
    let mut both_continuation =
        RecordedContinuation::from_setup(both_runtime, windows, vec![FaultAction::Wait]);
    let error = expect_error(both_continuation.checkpoint(), "both failures must surface");
    assert!(error.to_string().contains("export failed"), "{error}");
    assert!(
        error
            .to_string()
            .contains("snapshot release also failed: release failed"),
        "{error}"
    );
    assert_eq!(both_view.trace.borrow().capture_calls, 1);
    assert_eq!(both_view.trace.borrow().export_calls, 1);
    assert_eq!(both_view.trace.borrow().release_calls, 1);
    let poisoned = expect_error(
        both_continuation.checkpoint(),
        "combined failure must poison",
    );
    assert!(
        poisoned
            .to_string()
            .contains("restore a retained checkpoint"),
        "{poisoned}"
    );
}

#[test]
fn legacy_and_impossible_retained_plans_fail_before_runtime_restore() {
    let windows = ActionWindows {
        root_seal: 100,
        horizon_nanos: 20,
    };
    let actions = vec![FaultAction::Wait, FaultAction::Wait];

    let legacy_runtime = ModelRuntime::new(windows.root_seal, 0x91);
    let legacy_view = legacy_runtime.clone();
    let legacy = retained(
        checkpoint_for(windows.root_seal, None),
        windows,
        actions.clone(),
    );
    let error = expect_error(
        RecordedContinuation::restore(legacy_runtime, &legacy),
        "legacy checkpoint must be rejected",
    );
    assert!(error.to_string().contains("no action cursor"), "{error}");
    assert_eq!(legacy_view.trace.borrow().restore_calls, 0);
    assert_eq!(legacy_view.trace.borrow().steps, 0);

    let impossible_count_runtime = ModelRuntime::new(windows.root_seal, 0x92);
    let impossible_count_view = impossible_count_runtime.clone();
    let impossible_count = retained(
        checkpoint_for(
            windows.root_seal,
            Some(ActionCursor::from_parts(3, false).unwrap()),
        ),
        windows,
        vec![FaultAction::Wait],
    );
    let error = expect_error(
        RecordedContinuation::restore(impossible_count_runtime, &impossible_count),
        "cursor beyond plan must be rejected",
    );
    assert!(error.to_string().contains("action count"), "{error}");
    assert_eq!(impossible_count_view.trace.borrow().restore_calls, 0);
    assert_eq!(impossible_count_view.trace.borrow().steps, 0);

    let impossible_time_runtime = ModelRuntime::new(windows.root_seal, 0x93);
    let impossible_time_view = impossible_time_runtime.clone();
    let impossible_time = retained(
        checkpoint_for(
            windows.window(1).0 - 1,
            Some(ActionCursor::from_parts(1, false).unwrap()),
        ),
        windows,
        actions,
    );
    let error = expect_error(
        RecordedContinuation::restore(impossible_time_runtime, &impossible_time),
        "cursor before next window must be rejected",
    );
    assert!(
        error.to_string().contains("precedes its completed horizon"),
        "{error}"
    );
    assert_eq!(impossible_time_view.trace.borrow().restore_calls, 0);
    assert_eq!(impossible_time_view.trace.borrow().steps, 0);
}
