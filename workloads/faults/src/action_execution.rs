// SPDX-License-Identifier: AGPL-3.0-or-later

//! Ordered activation and bounded execution shared by fault recording and
//! continuation. The runtime supplies guest operations; the executor owns the
//! retained action position independently of any hypervisor.

use crate::execution::ActionCursor;
use crate::target::{ActionWindows, FaultAction, action_delta, standing_windows};
use control_proto::{SnapId, StopReason};
use environment::{channel::Effect, input_spec::ServiceConfig};
use fault_policy::encode_windows;
use std::error::Error;

/// Identity of the package's standing-fault service.
pub const SERVICE_IDENTITY: &[u8] = b"faults-standing-v1";

/// The branch configuration that installs `actions`' standing faults.
fn branch_config(
    windows: ActionWindows,
    actions: &[FaultAction],
) -> Result<ServiceConfig, Box<dyn Error>> {
    Ok(ServiceConfig {
        identity: SERVICE_IDENTITY.to_vec(),
        configuration: encode_windows(&standing_windows(windows, actions))?,
    })
}

/// The action position owned by a running or retained execution. Recording and
/// continuation use the same activation and run operations; a restore supplies
/// the retained cursor instead of deriving it from virtual time.
#[derive(Clone, Copy, Debug)]
pub struct ActionExecution {
    windows: ActionWindows,
    cursor: ActionCursor,
}

pub trait ActionRuntime {
    fn activate_prefix(
        &mut self,
        parent: SnapId,
        config: ServiceConfig,
        effects: Vec<(u64, Effect)>,
    ) -> Result<(), Box<dyn Error>>;
    fn run_action_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>>;
}

impl ActionExecution {
    pub fn new(windows: ActionWindows, cursor: ActionCursor) -> Self {
        Self { windows, cursor }
    }

    pub fn cursor(&self) -> ActionCursor {
        self.cursor
    }

    pub fn validate(&self, at: u64, action_count: usize) -> Result<(), Box<dyn Error>> {
        self.cursor.validate(u64::try_from(action_count)?)?;
        let next = usize::try_from(self.cursor.completed())?;
        if at < self.windows.window(next).0 {
            return Err("action cursor precedes its completed horizon".into());
        }
        Ok(())
    }

    /// Install exactly the next prefix. Only successful activation advances
    /// the cursor. The caller must abandon the session on a backend error.
    pub fn activate(
        &mut self,
        session: &mut impl ActionRuntime,
        parent: SnapId,
        floor: u64,
        actions: &[FaultAction],
    ) -> Result<(), Box<dyn Error>> {
        self.validate(floor, actions.len())?;
        let index = usize::try_from(self.cursor.completed())?;
        let mut next = self.cursor;
        next.activate(u64::try_from(index)?)?;
        let prefix = actions
            .get(..=index)
            .ok_or("no remaining recorded action")?;
        let config = branch_config(self.windows, prefix)?;
        let effects = match action_delta(prefix[index], self.windows.window(index)).perturb {
            None => Vec::new(),
            Some(perturb) => {
                let fault = fault_policy::HostFault::decode(&perturb.fault)?;
                let effect = fault_policy::consonance::effect(&fault)?;
                vec![(perturb.at.max(floor), effect)]
            }
        };
        session.activate_prefix(parent, config, effects)?;
        self.cursor = next;
        Ok(())
    }

    /// A bounded run may stop inside the active action. Only an observed
    /// deadline at or beyond that action's horizon completes it; assertion and
    /// other stops retain its activation, including when time overshoots.
    pub fn run_until(
        &mut self,
        session: &mut impl ActionRuntime,
        target: u64,
    ) -> Result<StopReason, Box<dyn Error>> {
        if !self.cursor.active() {
            return Err("no active recorded action".into());
        }
        let index = usize::try_from(self.cursor.completed())?;
        let horizon = self.windows.deadline(index);
        let stop = session.run_action_until(target.min(horizon))?;
        if matches!(&stop, StopReason::Deadline { vtime } if vtime.0 >= horizon) {
            self.cursor.complete(u64::try_from(index)?)?;
        }
        Ok(stop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, error::Error};

    #[derive(Debug)]
    struct ActivationCapture {
        parent: SnapId,
        identity: Vec<u8>,
        configuration: Vec<u8>,
        effects: Vec<(u64, Effect)>,
    }

    #[derive(Debug, Default)]
    struct FakeActionRuntime {
        activation_attempts: usize,
        activations: Vec<ActivationCapture>,
        run_deadlines: Vec<u64>,
        outcomes: VecDeque<Result<StopReason, String>>,
        activation_error: Option<String>,
    }

    impl FakeActionRuntime {
        fn with_outcomes(outcomes: impl IntoIterator<Item = StopReason>) -> Self {
            Self {
                outcomes: outcomes.into_iter().map(Ok).collect(),
                ..Self::default()
            }
        }
    }

    impl ActionRuntime for FakeActionRuntime {
        fn activate_prefix(
            &mut self,
            parent: SnapId,
            config: ServiceConfig,
            effects: Vec<(u64, Effect)>,
        ) -> Result<(), Box<dyn Error>> {
            self.activation_attempts += 1;
            if let Some(error) = self.activation_error.take() {
                return Err(std::io::Error::other(error).into());
            }
            self.activations.push(ActivationCapture {
                parent,
                identity: config.identity,
                configuration: config.configuration,
                effects,
            });
            Ok(())
        }

        fn run_action_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
            self.run_deadlines.push(deadline);
            match self.outcomes.pop_front() {
                Some(Ok(stop)) => Ok(stop),
                Some(Err(error)) => Err(std::io::Error::other(error).into()),
                None => Err(std::io::Error::other("fake runtime exhausted").into()),
            }
        }
    }

    fn deadline_stop(vtime: u64) -> StopReason {
        StopReason::Deadline {
            vtime: control_proto::Moment(vtime),
        }
    }

    fn assertion_stop(vtime: u64) -> StopReason {
        StopReason::Assertion {
            vtime: control_proto::Moment(vtime),
            ev: control_proto::EventRef {
                id: 7,
                data: vec![0xa5],
            },
        }
    }

    #[test]
    fn action_execution_cold_restores_mid_action_without_reactivation() {
        let windows = ActionWindows {
            root_seal: 1_000,
            horizon_nanos: 100,
        };
        let actions = [FaultAction::Interrupt(0x30), FaultAction::Wait];
        let parent = SnapId(41);

        let mut uninterrupted_runtime = FakeActionRuntime::with_outcomes([deadline_stop(1_100)]);
        let mut uninterrupted = ActionExecution::new(windows, ActionCursor::default());
        uninterrupted
            .activate(
                &mut uninterrupted_runtime,
                parent,
                windows.window(0).0,
                &actions,
            )
            .expect("activate first action");
        uninterrupted
            .run_until(&mut uninterrupted_runtime, windows.deadline(0))
            .expect("finish first action in one warm run");
        assert_eq!(
            uninterrupted.cursor(),
            ActionCursor::from_parts(1, false).unwrap()
        );
        assert_eq!(
            uninterrupted_runtime.run_deadlines,
            vec![windows.deadline(0)]
        );

        let mut cold_runtime =
            FakeActionRuntime::with_outcomes([deadline_stop(1_050), deadline_stop(1_100)]);
        let mut cold = ActionExecution::new(windows, ActionCursor::default());
        cold.activate(&mut cold_runtime, parent, windows.window(0).0, &actions)
            .expect("activate first action for cold restore");
        cold.run_until(&mut cold_runtime, 1_050)
            .expect("run first partial interval for cold restore");

        let encoded = serde_json::to_vec(&cold.cursor()).expect("serialize active cursor");
        let serde_restored_cursor: ActionCursor =
            serde_json::from_slice(&encoded).expect("deserialize active cursor");
        assert_eq!(serde_restored_cursor, cold.cursor());

        let checkpoint = crate::checkpoint::Checkpoint {
            at: 1_050,
            cursor: Some(serde_restored_cursor),
            ..crate::checkpoint::Checkpoint::default()
        };
        let checkpoint_bytes = checkpoint.encode().expect("encode v2 checkpoint");
        assert_eq!(
            &checkpoint_bytes[crate::checkpoint::MAGIC.len()
                ..crate::checkpoint::MAGIC.len() + std::mem::size_of::<u32>()],
            &crate::checkpoint::VERSION.to_le_bytes()
        );
        let restored_cursor = crate::checkpoint::Checkpoint::decode(&checkpoint_bytes)
            .expect("decode v2 checkpoint")
            .cursor
            .expect("v2 checkpoint cursor");
        assert_eq!(restored_cursor, cold.cursor());

        let attempts_before = cold_runtime.activation_attempts;
        let cursor_before = cold.cursor();
        let error = cold
            .activate(&mut cold_runtime, parent, 1_050, &actions)
            .expect_err("an active action cannot be activated again");
        assert!(error.to_string().contains("already active"), "{error}");
        assert_eq!(cold_runtime.activation_attempts, attempts_before);
        assert_eq!(cold.cursor(), cursor_before);

        let mut restored = ActionExecution::new(windows, restored_cursor);
        restored
            .validate(checkpoint.at, actions.len())
            .expect("retained cursor matches the checkpoint endpoint");
        restored
            .run_until(&mut cold_runtime, windows.deadline(0))
            .expect("finish the restored action without activation");
        assert_eq!(restored.cursor(), uninterrupted.cursor());
        assert_eq!(cold_runtime.run_deadlines, vec![1_050, windows.deadline(0)]);
        assert_eq!(cold_runtime.activations.len(), 1);
        assert_eq!(uninterrupted_runtime.activations.len(), 1);

        let cold_activation = &cold_runtime.activations[0];
        let uninterrupted_activation = &uninterrupted_runtime.activations[0];
        assert_eq!(cold_activation.parent, uninterrupted_activation.parent);
        assert_eq!(cold_activation.identity, uninterrupted_activation.identity);
        assert_eq!(
            cold_activation.configuration,
            uninterrupted_activation.configuration
        );
        assert_eq!(cold_activation.effects, uninterrupted_activation.effects);
        assert_eq!(cold_activation.effects.len(), 1);
        assert_eq!(cold_activation.effects[0].0, windows.window(0).0);
    }

    #[test]
    fn action_execution_overshoot_completes_one_ordinal_action_at_a_time() {
        let windows = ActionWindows {
            root_seal: 1_000,
            horizon_nanos: 100,
        };
        let actions = [
            FaultAction::Hook(1),
            FaultAction::Hook(2),
            FaultAction::Hook(3),
        ];
        let parent = SnapId(52);
        let mut runtime =
            FakeActionRuntime::with_outcomes([deadline_stop(1_350), deadline_stop(1_450)]);
        let mut execution = ActionExecution::new(windows, ActionCursor::default());

        execution
            .activate(&mut runtime, parent, windows.window(0).0, &actions)
            .expect("activate first action");
        let first = execution
            .run_until(&mut runtime, 1_350)
            .expect("run an overshooting guest step");
        assert_eq!(first, deadline_stop(1_350));
        assert_eq!(
            execution.cursor(),
            ActionCursor::from_parts(1, false).unwrap()
        );
        assert_eq!(runtime.run_deadlines, vec![windows.deadline(0)]);

        execution
            .activate(&mut runtime, parent, 1_350, &actions)
            .expect("activate exactly the second action");
        assert_eq!(
            execution.cursor(),
            ActionCursor::from_parts(1, true).unwrap()
        );
        execution
            .run_until(&mut runtime, 1_450)
            .expect("run the second overshooting guest step");
        assert_eq!(
            execution.cursor(),
            ActionCursor::from_parts(2, false).unwrap()
        );
        assert_eq!(
            runtime.run_deadlines,
            vec![windows.deadline(0), windows.deadline(1)]
        );

        execution
            .activate(&mut runtime, parent, 1_450, &actions)
            .expect("activate exactly the third action");
        assert_eq!(
            execution.cursor(),
            ActionCursor::from_parts(2, true).unwrap()
        );
        assert_eq!(runtime.activations.len(), 3);
        for (index, activation) in runtime.activations.iter().enumerate() {
            assert_eq!(activation.parent, parent);
            assert!(activation.effects.is_empty());
            let standing = fault_policy::decode_windows(&activation.configuration)
                .expect("decode captured standing configuration");
            assert_eq!(standing.len(), index + 1);
            assert_eq!(standing[0].start, windows.window(0).0);
            if index >= 1 {
                assert_eq!(standing[1].start, windows.window(1).0);
            }
            if index >= 2 {
                assert_eq!(standing[2].start, windows.window(2).0);
            }
        }
    }

    #[test]
    fn action_execution_failures_preserve_cursor_and_reject_before_runtime_call() {
        let windows = ActionWindows {
            root_seal: 1_000,
            horizon_nanos: 100,
        };
        let parent = SnapId(63);
        let actions = [FaultAction::Hook(1)];

        let mut activation_runtime = FakeActionRuntime {
            activation_error: Some("activation failed".to_owned()),
            ..FakeActionRuntime::default()
        };
        let mut activation_execution = ActionExecution::new(windows, ActionCursor::default());
        let error = activation_execution
            .activate(
                &mut activation_runtime,
                parent,
                windows.window(0).0,
                &actions,
            )
            .expect_err("surface activation failure to caller");
        assert!(error.to_string().contains("activation failed"), "{error}");
        assert_eq!(activation_execution.cursor(), ActionCursor::default());
        assert_eq!(activation_runtime.activation_attempts, 1);
        assert!(activation_runtime.activations.is_empty());

        let impossible_cursor = ActionCursor::from_parts(1, false).expect("valid cursor");
        let mut impossible_execution = ActionExecution::new(windows, impossible_cursor);
        let mut impossible_runtime = FakeActionRuntime::default();
        let error = impossible_execution
            .activate(
                &mut impossible_runtime,
                parent,
                windows.window(1).0 - 1,
                &[FaultAction::Wait, FaultAction::Wait],
            )
            .expect_err("reject a cursor before its next window");
        assert!(
            error.to_string().contains("precedes its completed horizon"),
            "{error}"
        );
        assert_eq!(impossible_execution.cursor(), impossible_cursor);
        assert_eq!(impossible_runtime.activation_attempts, 0);

        let mut run_runtime = FakeActionRuntime::default();
        let mut run_execution = ActionExecution::new(windows, ActionCursor::default());
        run_execution
            .activate(&mut run_runtime, parent, windows.window(0).0, &actions)
            .expect("activate before run failure");
        let active_cursor = run_execution.cursor();
        run_runtime.outcomes.push_back(Err("run failed".to_owned()));
        let error = run_execution
            .run_until(&mut run_runtime, windows.deadline(0))
            .expect_err("surface run failure to caller");
        assert!(error.to_string().contains("run failed"), "{error}");
        assert_eq!(run_execution.cursor(), active_cursor);
        assert!(run_execution.cursor().active());
        assert_eq!(run_runtime.activations.len(), 1);
        assert_eq!(run_runtime.run_deadlines, vec![windows.deadline(0)]);
    }

    #[test]
    fn action_execution_assertion_beyond_horizon_requires_a_later_deadline() {
        let windows = ActionWindows {
            root_seal: 5_000,
            horizon_nanos: 100,
        };
        let actions = [FaultAction::Wait, FaultAction::Wait];
        let parent = SnapId(74);
        let mut runtime =
            FakeActionRuntime::with_outcomes([assertion_stop(5_500), deadline_stop(5_500)]);
        let mut execution = ActionExecution::new(windows, ActionCursor::default());
        execution
            .activate(&mut runtime, parent, windows.window(0).0, &actions)
            .expect("activate action before assertion");
        let active_cursor = execution.cursor();

        let assertion = execution
            .run_until(&mut runtime, 5_500)
            .expect("surface assertion even after horizon");
        assert_eq!(assertion, assertion_stop(5_500));
        assert_eq!(execution.cursor(), active_cursor);
        assert!(execution.cursor().active());

        execution
            .run_until(&mut runtime, windows.deadline(0))
            .expect("complete the retained action on its later deadline");
        assert_eq!(
            execution.cursor(),
            ActionCursor::from_parts(1, false).unwrap()
        );
        assert_eq!(runtime.activations.len(), 1);
        assert_eq!(
            runtime.run_deadlines,
            vec![windows.deadline(0), windows.deadline(0)]
        );
    }

    #[test]
    fn action_execution_keeps_actions_ordinal_when_windows_share_a_moment() {
        let windows = ActionWindows {
            root_seal: 2_000,
            horizon_nanos: 0,
        };
        let actions = [
            FaultAction::Hook(11),
            FaultAction::Hook(12),
            FaultAction::Hook(13),
        ];
        let parent = SnapId(85);
        let mut runtime = FakeActionRuntime::with_outcomes([
            deadline_stop(2_000),
            deadline_stop(2_000),
            deadline_stop(2_000),
        ]);
        let mut execution = ActionExecution::new(windows, ActionCursor::default());

        for index in 0..actions.len() {
            execution
                .activate(&mut runtime, parent, 2_000, &actions)
                .expect("activate next same-moment action");
            assert_eq!(
                execution.cursor(),
                ActionCursor::from_parts(index as u64, true).unwrap()
            );
            execution
                .run_until(&mut runtime, 2_000)
                .expect("complete next same-moment action");
            assert_eq!(
                execution.cursor(),
                ActionCursor::from_parts((index + 1) as u64, false).unwrap()
            );
        }

        assert_eq!(runtime.activations.len(), actions.len());
        assert_eq!(runtime.run_deadlines, vec![2_000; actions.len()]);
        for (index, activation) in runtime.activations.iter().enumerate() {
            let standing = fault_policy::decode_windows(&activation.configuration)
                .expect("decode same-moment standing configuration");
            assert_eq!(standing.len(), index + 1);
        }
    }
}
