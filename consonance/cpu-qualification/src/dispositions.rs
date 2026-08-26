// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recorded acceptances for stage-0 deviations.
//!
//! A favorable deviation is still a deviation, so stage 0 never decides on its
//! own that one is acceptable. A person decides, in a file, naming the condition,
//! the reading they are accepting, and why. Stage 0 then marks the row
//! dispositioned instead of undecided, and the reason travels in the run's
//! records.
//!
//! An acceptance names the reading it accepts, not just the condition. A machine
//! that changes underneath a run produces a different reading, the acceptance
//! stops applying, and the deviation is live again — which is the difference
//! between accepting a known state and switching a check off.

use serde::{Deserialize, Serialize};

/// The `[dispositions] schema` token this crate reads.
pub const DISPOSITIONS_SCHEMA: &str = "cpu-qualification-dispositions-v1";

/// A refusal from loading dispositions.
#[derive(Debug, thiserror::Error)]
pub enum DispositionError {
    /// The file is not the TOML shape a dispositions file has.
    #[error("dispositions do not parse: {0}")]
    Parse(#[from] toml::de::Error),
    /// The `schema` token is not the one this crate reads.
    #[error("dispositions schema is {found:?}, expected {expected:?}")]
    Schema {
        /// The token the file declares.
        found: String,
        /// The token this crate reads.
        expected: &'static str,
    },
    /// One or more acceptances matched no deviating row. An acceptance that
    /// matches nothing describes a machine that is no longer there, and leaving
    /// it in place would accept whatever replaces it.
    #[error("{} acceptance(s) matched no deviating row: {}", .stale.len(), .stale.join("; "))]
    Stale {
        /// One description per acceptance that matched nothing.
        stale: Vec<String>,
    },
}

/// One recorded acceptance of a deviating condition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disposition {
    /// The condition's token, matching a [`crate::chips::HostConditionKind`].
    pub condition: String,
    /// The scope this applies to. Absent means every scope the condition is read
    /// at, mirroring how a lone pack expectation speaks for every scope.
    #[serde(default)]
    pub scope: Option<String>,
    /// The reading being accepted. The acceptance applies to this reading alone.
    pub found: String,
    /// Why this reading is acceptable here.
    pub why: String,
}

impl Disposition {
    /// How this acceptance reads in a refusal.
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.scope {
            Some(scope) => format!("{}[{}] found {:?}", self.condition, scope, self.found),
            None => format!("{} found {:?}", self.condition, self.found),
        }
    }

    /// Whether this acceptance covers a row for `condition` at `scope` reading
    /// `found`.
    #[must_use]
    pub fn covers(&self, condition: &str, scope: &str, found: &str) -> bool {
        self.condition == condition
            && self.scope.as_ref().is_none_or(|s| s == scope)
            && self.found.trim() == found.trim()
    }
}

/// A file of recorded acceptances.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dispositions {
    /// The format token.
    pub schema: String,
    /// The acceptances.
    #[serde(default, rename = "disposition")]
    pub dispositions: Vec<Disposition>,
}

impl Dispositions {
    /// Parse a dispositions file and check its schema token.
    ///
    /// # Errors
    /// [`DispositionError::Parse`] on a malformed file,
    /// [`DispositionError::Schema`] on an unreadable format token.
    pub fn parse(text: &str) -> Result<Dispositions, DispositionError> {
        let file: Dispositions = toml::from_str(text)?;
        if file.schema != DISPOSITIONS_SCHEMA {
            return Err(DispositionError::Schema {
                found: file.schema.clone(),
                expected: DISPOSITIONS_SCHEMA,
            });
        }
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(scope: Option<&str>) -> Disposition {
        Disposition {
            condition: "governor-pinned".to_string(),
            scope: scope.map(str::to_string),
            found: "unreadable".to_string(),
            why: "no frequency driver here".to_string(),
        }
    }

    #[test]
    fn a_scoped_acceptance_covers_only_its_scope() {
        let d = one(Some("cpu0"));
        assert!(d.covers("governor-pinned", "cpu0", "unreadable"));
        assert!(!d.covers("governor-pinned", "cpu1", "unreadable"));
    }

    #[test]
    fn an_unscoped_acceptance_covers_every_scope() {
        let d = one(None);
        assert!(d.covers("governor-pinned", "cpu0", "unreadable"));
        assert!(d.covers("governor-pinned", "cpu7", "unreadable"));
    }

    #[test]
    fn an_acceptance_does_not_cover_a_different_reading() {
        let d = one(None);
        assert!(!d.covers("governor-pinned", "cpu0", "powersave"));
    }

    #[test]
    fn an_acceptance_does_not_cover_a_different_condition() {
        let d = one(None);
        assert!(!d.covers("smt-policy", "host", "unreadable"));
    }

    #[test]
    fn a_wrong_schema_token_refuses() {
        let text = "schema = \"something-else\"\n";
        assert!(matches!(
            Dispositions::parse(text),
            Err(DispositionError::Schema { .. })
        ));
    }

    #[test]
    fn a_file_round_trips() {
        let text = format!(
            "schema = {DISPOSITIONS_SCHEMA:?}\n\n\
             [[disposition]]\n\
             condition = \"governor-pinned\"\n\
             found = \"unreadable\"\n\
             why = \"no frequency driver here\"\n"
        );
        let parsed = Dispositions::parse(&text).expect("parses");
        assert_eq!(parsed.dispositions.len(), 1);
        assert_eq!(parsed.dispositions[0].scope, None);
    }
}
