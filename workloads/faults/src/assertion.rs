// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum AssertionKind {
    Always,
    AlwaysOrUnreachable,
    Sometimes,
    Reachable,
    Unreachable,
}

impl AssertionKind {
    fn from_record(assert_type: &str, must_hit: bool) -> Option<Self> {
        match (assert_type, must_hit) {
            ("always", true) => Some(Self::Always),
            ("always", false) => Some(Self::AlwaysOrUnreachable),
            ("sometimes", _) => Some(Self::Sometimes),
            ("reachability", true) => Some(Self::Reachable),
            ("reachability", false) => Some(Self::Unreachable),
            _ => None,
        }
    }

    #[must_use]
    pub fn must_be_satisfied(self) -> bool {
        matches!(self, Self::Always | Self::Sometimes | Self::Reachable)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AssertionOutcome {
    pub kind: AssertionKind,
    pub message: String,
    pub location: String,
    pub passed: bool,
    pub failed: bool,
}

impl AssertionOutcome {
    #[must_use]
    pub fn satisfied(&self) -> bool {
        match self.kind {
            AssertionKind::Sometimes | AssertionKind::Reachable => self.passed,
            AssertionKind::Always => self.passed && !self.failed,
            AssertionKind::AlwaysOrUnreachable | AssertionKind::Unreachable => !self.violated(),
        }
    }

    #[must_use]
    pub fn violated(&self) -> bool {
        match self.kind {
            AssertionKind::Always | AssertionKind::AlwaysOrUnreachable => self.failed,
            AssertionKind::Unreachable => self.passed || self.failed,
            AssertionKind::Sometimes | AssertionKind::Reachable => false,
        }
    }

    #[must_use]
    pub fn feeds_the_key(&self) -> bool {
        matches!(
            self.kind,
            AssertionKind::Sometimes | AssertionKind::Reachable
        ) && self.passed
    }

    pub fn merge(&mut self, other: &Self) {
        self.passed |= other.passed;
        self.failed |= other.failed;
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Assertions(pub BTreeMap<String, AssertionOutcome>);

impl Assertions {
    pub fn record(&mut self, id: String, outcome: AssertionOutcome) {
        match self.0.get_mut(&id) {
            Some(existing) => existing.merge(&outcome),
            None => {
                self.0.insert(id, outcome);
            }
        }
    }

    pub fn merge(&mut self, other: &Self) {
        for (id, outcome) in &other.0 {
            match self.0.get_mut(id) {
                Some(existing) => existing.merge(outcome),
                None => {
                    self.0.insert(id.clone(), outcome.clone());
                }
            }
        }
    }

    #[must_use]
    pub fn key_ids(&self) -> BTreeSet<&str> {
        self.0
            .iter()
            .filter(|(_, outcome)| outcome.feeds_the_key())
            .map(|(id, _)| id.as_str())
            .collect()
    }

    #[must_use]
    pub fn violations(&self) -> BTreeSet<String> {
        self.0
            .iter()
            .filter(|(_, outcome)| outcome.violated())
            .map(|(id, _)| id.clone())
            .collect()
    }

    #[must_use]
    pub fn key(&self) -> AssertionSet {
        AssertionSet::of(self.key_ids())
    }
}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct AssertionSet {
    pub count: u32,
    pub digest: u128,
}

impl AssertionSet {
    #[must_use]
    pub fn of<'a>(ids: impl IntoIterator<Item = &'a str>) -> Self {
        let mut ids: Vec<&str> = ids.into_iter().collect();
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            return Self::default();
        }
        let mut hasher = Sha256::new();
        for id in &ids {
            hasher.update((id.len() as u64).to_le_bytes());
            hasher.update(id.as_bytes());
        }
        let digest = hasher.finalize();
        let mut low = [0_u8; 16];
        low.copy_from_slice(&digest[..16]);
        Self {
            count: u32::try_from(ids.len()).unwrap_or(u32::MAX),
            digest: u128::from_le_bytes(low),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JsonEvent {
    pub assertion: Option<(String, AssertionOutcome)>,
    pub setup_complete: bool,
    pub pid: Option<u64>,
}

#[derive(Deserialize)]
struct RawEvent {
    antithesis_assert: Option<RawAssert>,
    antithesis_setup: Option<serde_json::Value>,
    harmony_attribution: Option<RawAttribution>,
}

#[derive(Deserialize)]
struct RawAssert {
    id: Option<String>,
    message: Option<String>,
    assert_type: String,
    #[serde(default)]
    condition: bool,
    hit: bool,
    #[serde(default = "must_hit_default")]
    must_hit: bool,
    location: Option<RawLocation>,
}

fn must_hit_default() -> bool {
    true
}

#[derive(Deserialize)]
struct RawLocation {
    file: Option<String>,
    function: Option<String>,
    begin_line: Option<u64>,
}

#[derive(Deserialize)]
struct RawAttribution {
    pid: Option<u64>,
}

#[must_use]
pub fn decode_json_event(bytes: &[u8]) -> Option<JsonEvent> {
    let raw: RawEvent = serde_json::from_slice(bytes).ok()?;
    let assertion = raw.antithesis_assert.and_then(|record| {
        let kind = AssertionKind::from_record(&record.assert_type, record.must_hit)?;
        let message = record.message.unwrap_or_default();
        let id = record
            .id
            .filter(|id| !id.is_empty())
            .unwrap_or(message.clone());
        if id.is_empty() {
            return None;
        }
        let location = record
            .location
            .map(|location| {
                format!(
                    "{}:{} {}",
                    location.file.unwrap_or_default(),
                    location.begin_line.unwrap_or_default(),
                    location.function.unwrap_or_default()
                )
            })
            .unwrap_or_default();
        let (passed, failed) = if record.hit {
            (record.condition, !record.condition)
        } else {
            (false, false)
        };
        let (passed, failed) = match kind {
            AssertionKind::Reachable | AssertionKind::Unreachable => (record.hit, false),
            _ => (passed, failed),
        };
        Some((
            id,
            AssertionOutcome {
                kind,
                message,
                location,
                passed,
                failed,
            },
        ))
    });
    let setup_complete = raw.antithesis_setup.is_some_and(|setup| {
        setup
            .get("status")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|status| status == "complete")
    });
    Some(JsonEvent {
        assertion,
        setup_complete,
        pid: raw
            .harmony_attribution
            .and_then(|attribution| attribution.pid),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(assert_type: &str, must_hit: bool, hit: bool, condition: bool) -> Vec<u8> {
        format!(
            r#"{{"harmony_attribution":{{"rip":"0x1","pid":42,"comm_hex":"61"}},"antithesis_assert":{{"hit":{hit},"must_hit":{must_hit},"assert_type":"{assert_type}","display_type":"x","message":"m {assert_type}","condition":{condition},"id":"id {assert_type} {must_hit}","location":{{"class":"c","function":"f","file":"a.c","begin_line":7,"begin_column":0}},"details":null}}}}"#
        )
        .into_bytes()
    }

    fn decode(bytes: &[u8]) -> (String, AssertionOutcome) {
        decode_json_event(bytes)
            .and_then(|event| event.assertion)
            .expect("an assertion")
    }

    #[test]
    fn the_five_kinds_follow_antithesis_semantics() {
        let (id, always_false) = decode(&record("always", true, true, false));
        assert_eq!(id, "id always true");
        assert_eq!(always_false.kind, AssertionKind::Always);
        assert!(always_false.violated());
        assert_eq!(always_false.location, "a.c:7 f");
        let (_, always_true) = decode(&record("always", true, true, true));
        assert!(always_true.satisfied() && !always_true.violated());
        let (_, declared) = decode(&record("always", true, false, false));
        assert!(!declared.violated() && !declared.satisfied());
        let (_, or_unreachable) = decode(&record("always", false, false, false));
        assert_eq!(or_unreachable.kind, AssertionKind::AlwaysOrUnreachable);
        assert!(or_unreachable.satisfied());
        let (_, sometimes_false) = decode(&record("sometimes", true, true, false));
        assert!(!sometimes_false.satisfied() && !sometimes_false.violated());
        assert!(!sometimes_false.feeds_the_key());
        let (_, sometimes_true) = decode(&record("sometimes", true, true, true));
        assert!(sometimes_true.feeds_the_key());
        let (_, reached) = decode(&record("reachability", true, true, false));
        assert_eq!(reached.kind, AssertionKind::Reachable);
        assert!(reached.feeds_the_key());
        let (_, unreachable) = decode(&record("reachability", false, true, true));
        assert_eq!(unreachable.kind, AssertionKind::Unreachable);
        assert!(unreachable.violated());
        let (_, unreachable_declared) = decode(&record("reachability", false, false, false));
        assert!(!unreachable_declared.violated());
    }

    #[test]
    fn attribution_setup_and_malformed_records() {
        let event = decode_json_event(&record("sometimes", true, true, true)).unwrap();
        assert_eq!(event.pid, Some(42));
        let setup =
            decode_json_event(br#"{"antithesis_setup":{"status":"complete","details":null}}"#)
                .unwrap();
        assert!(setup.setup_complete && setup.assertion.is_none());
        assert!(decode_json_event(b"not json").is_none());
        let unknown =
            decode_json_event(br#"{"antithesis_assert":{"assert_type":"odd","hit":true}}"#)
                .unwrap();
        assert!(unknown.assertion.is_none());
        let other = decode_json_event(br#"{"workload_event":{"any":1}}"#).unwrap();
        assert_eq!(other, JsonEvent::default());
    }

    #[test]
    fn the_message_names_an_assertion_without_an_id() {
        let (id, _) = decode(br#"{"antithesis_assert":{"assert_type":"sometimes","hit":true,"condition":true,"message":"only a message"}}"#);
        assert_eq!(id, "only a message");
    }

    #[test]
    fn outcomes_merge_and_the_key_is_the_satisfied_set() {
        let mut assertions = Assertions::default();
        let (id, first) = decode(&record("sometimes", true, true, false));
        assertions.record(id.clone(), first);
        assert_eq!(assertions.key(), AssertionSet::default());
        let (_, second) = decode(&record("sometimes", true, true, true));
        assertions.record(id.clone(), second);
        let (always, broken) = decode(&record("always", true, true, false));
        assertions.record(always.clone(), broken);
        assert_eq!(assertions.key().count, 1);
        assert_eq!(assertions.key(), AssertionSet::of([id.as_str()]));
        assert_eq!(assertions.violations(), BTreeSet::from([always]));
        let many: Vec<String> = (0..100).map(|index| format!("site {index}")).collect();
        let wide = AssertionSet::of(many.iter().map(String::as_str));
        assert_eq!(wide.count, 100);
        assert_ne!(
            wide,
            AssertionSet::of(many[..99].iter().map(String::as_str))
        );
        assert_eq!(
            AssertionSet::of(["b", "a"]),
            AssertionSet::of(["a", "b", "a"])
        );
        assert_ne!(AssertionSet::of(["ab"]), AssertionSet::of(["a", "b"]));
    }
}
