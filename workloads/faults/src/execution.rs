// SPDX-License-Identifier: AGPL-3.0-or-later

//! The explicit action position retained by a fault execution.
//!
//! An action becomes active before its guest window runs and becomes completed
//! only after the window reaches its endpoint. The cursor carries that
//! distinction directly, so restoring a session does not infer a position from
//! virtual time or silently skip an action that shares a timestamp with its
//! predecessor.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The action position of one running or retained execution.
///
/// `completed` is the number of actions that have completed. When `active` is
/// true, the action at that same index has been activated and is still running.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ActionCursor {
    completed: u64,
    active: bool,
}

#[derive(Deserialize, Serialize)]
struct ActionCursorFields {
    completed: u64,
    active: bool,
}

impl ActionCursor {
    /// Construct a cursor from its persisted fields after validating them.
    pub fn from_parts(completed: u64, active: bool) -> Result<Self, String> {
        if active && completed == u64::MAX {
            return Err("an active action must have a checked successor".to_owned());
        }
        Ok(Self { completed, active })
    }

    /// Number of actions that have completed.
    #[must_use]
    pub const fn completed(self) -> u64 {
        self.completed
    }

    /// Whether the action at [`Self::completed`] is currently active.
    #[must_use]
    pub const fn active(self) -> bool {
        self.active
    }

    /// Number of actions that have been activated, including the active one.
    #[must_use]
    pub fn activated(self) -> u64 {
        self.completed.saturating_add(u64::from(self.active))
    }

    /// Validate this cursor against an action list length.
    pub fn validate(&self, action_count: u64) -> Result<(), String> {
        if self.completed > action_count {
            return Err(format!(
                "completed action count {} exceeds action count {action_count}",
                self.completed
            ));
        }
        if self.active && self.completed >= action_count {
            return Err(format!(
                "active action index {} is outside action count {action_count}",
                self.completed
            ));
        }
        Ok(())
    }

    /// Activate the next action at `index`.
    pub fn activate(&mut self, index: u64) -> Result<(), String> {
        if self.active {
            return Err("an action is already active".to_owned());
        }
        if index != self.completed {
            return Err(format!(
                "cannot activate action {index}; next action is {}",
                self.completed
            ));
        }
        let _successor = self
            .completed
            .checked_add(1)
            .ok_or_else(|| "the active action has no checked successor".to_owned())?;

        self.active = true;
        Ok(())
    }

    /// Complete the currently active action at `index`.
    pub fn complete(&mut self, index: u64) -> Result<(), String> {
        if !self.active {
            return Err("there is no active action to complete".to_owned());
        }
        if index != self.completed {
            return Err(format!(
                "cannot complete action {index}; active action is {}",
                self.completed
            ));
        }
        let next = self
            .completed
            .checked_add(1)
            .ok_or_else(|| "the completed action has no checked successor".to_owned())?;

        self.completed = next;
        self.active = false;
        Ok(())
    }
}

impl Serialize for ActionCursor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ActionCursorFields {
            completed: self.completed,
            active: self.active,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ActionCursor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let fields = ActionCursorFields::deserialize(deserializer)?;
        Self::from_parts(fields.completed, fields.active).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_transitions_leave_the_cursor_unchanged() {
        let mut cursor = ActionCursor::from_parts(2, false).expect("valid cursor");

        let before = cursor;
        assert!(cursor.activate(1).is_err());
        assert_eq!(cursor, before);

        cursor.activate(2).expect("activate next action");
        let before = cursor;
        assert!(cursor.activate(2).is_err());
        assert_eq!(cursor, before);

        let before = cursor;
        assert!(cursor.complete(1).is_err());
        assert_eq!(cursor, before);

        cursor.complete(2).expect("complete active action");
        let before = cursor;
        assert!(cursor.complete(2).is_err());
        assert_eq!(cursor, before);
    }

    #[test]
    fn cold_serde_round_trip_preserves_a_mid_action_cursor() {
        let mut cursor = ActionCursor::from_parts(3, false).expect("valid cursor");
        cursor.activate(3).expect("activate action");
        let encoded = serde_json::to_string(&cursor).expect("serialize cursor");
        assert_eq!(encoded, r#"{"completed":3,"active":true}"#);

        let restored: ActionCursor = serde_json::from_str(&encoded).expect("deserialize cursor");
        assert_eq!(restored, cursor);
        assert_eq!(restored.completed(), 3);
        assert!(restored.active());
        assert_eq!(restored.activated(), 4);
    }

    #[test]
    fn sequential_actions_cannot_skip_when_their_moments_match() {
        let mut cursor = ActionCursor::default();
        for index in 0..3 {
            assert_eq!(cursor.completed(), index);
            assert_eq!(cursor.activated(), index);
            cursor.activate(index).expect("activate next action");
            assert_eq!(cursor.completed(), index);
            assert_eq!(cursor.activated(), index + 1);
            cursor.complete(index).expect("complete active action");
            assert_eq!(cursor.completed(), index + 1);
            assert!(!cursor.active());
        }
        assert_eq!(cursor.activated(), 3);
        assert!(cursor.activate(4).is_err());
        assert_eq!(cursor.completed(), 3);
        assert!(!cursor.active());
    }

    #[test]
    fn action_count_and_exhausted_bounds_are_checked() {
        let mut cursor = ActionCursor::from_parts(3, false).expect("valid cursor");
        assert!(cursor.validate(3).is_ok());
        assert!(cursor.validate(2).is_err());
        cursor.activate(3).expect("activate action");
        assert!(cursor.validate(3).is_err());

        let exhausted = ActionCursor::from_parts(u64::MAX, false).expect("valid exhausted cursor");
        assert!(exhausted.validate(u64::MAX).is_ok());
        assert!(exhausted.validate(u64::MAX - 1).is_err());
        let mut exhausted = exhausted;
        assert!(exhausted.activate(u64::MAX).is_err());
        assert_eq!(
            exhausted,
            ActionCursor::from_parts(u64::MAX, false).unwrap()
        );

        let mut final_action = ActionCursor::from_parts(u64::MAX - 1, false).unwrap();
        final_action
            .activate(u64::MAX - 1)
            .expect("activate final representable action");
        final_action
            .complete(u64::MAX - 1)
            .expect("complete final representable action");
        assert_eq!(final_action, exhausted);
    }

    #[test]
    fn an_active_maximum_cursor_is_rejected_on_the_serde_wire() {
        let encoded = format!(r#"{{"completed":{},"active":true}}"#, u64::MAX);
        let error = serde_json::from_str::<ActionCursor>(&encoded).expect_err("invalid cursor");
        assert!(error.to_string().contains("successor"), "{error}");
        assert!(ActionCursor::from_parts(u64::MAX, true).is_err());
    }
}
