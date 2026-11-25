// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet};

use process_proto::events::ParkedOutcome;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error, ser::SerializeStruct};
use sha2::{Digest, Sha256};

pub const FAILURE_DETAILS_LIMIT_BYTES: usize = 4096;
pub const FAILURE_DETAILS_PREFIX_BYTES: usize = 1024;

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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureDetails(String);

impl FailureDetails {
    #[must_use]
    pub fn of(value: &serde_json::Value) -> Option<Self> {
        if value.is_null() {
            return None;
        }
        let text = value.to_string();
        if text.len() <= FAILURE_DETAILS_LIMIT_BYTES {
            return Some(Self(text));
        }
        let mut end = FAILURE_DETAILS_PREFIX_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Some(Self(
            serde_json::json!({"truncated_bytes": text.len(), "prefix": &text[..end]}).to_string(),
        ))
    }

    fn unreadable(error: &serde_json::Error) -> Self {
        Self(serde_json::json!({ "unreadable": error.to_string() }).to_string())
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

impl Serialize for FailureDetails {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for FailureDetails {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.is_empty() {
            return Err(D::Error::custom("assertion failure details are empty"));
        }
        Ok(Self(text))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct AssertionOutcome {
    pub kind: AssertionKind,
    pub message: String,
    pub location: String,
    pub passed: bool,
    pub failed: bool,
    pub holds: Option<bool>,
    #[serde(default)]
    pub failure_details: Option<FailureDetails>,
}

impl Serialize for AssertionOutcome {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let omit_details = serializer.is_human_readable() && self.failure_details.is_none();
        let mut state =
            serializer.serialize_struct("AssertionOutcome", if omit_details { 6 } else { 7 })?;
        state.serialize_field("kind", &self.kind)?;
        state.serialize_field("message", &self.message)?;
        state.serialize_field("location", &self.location)?;
        state.serialize_field("passed", &self.passed)?;
        state.serialize_field("failed", &self.failed)?;
        state.serialize_field("holds", &self.holds)?;
        if omit_details {
            state.skip_field("failure_details")?;
        } else {
            state.serialize_field("failure_details", &self.failure_details)?;
        }
        state.end()
    }
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
        let first_violation = !self.violated() && other.violated();
        self.passed |= other.passed;
        self.failed |= other.failed;
        if other.holds.is_some() {
            self.holds = other.holds;
        }
        if first_violation {
            self.failure_details.clone_from(&other.failure_details);
        }
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
    pub fn violation_details(&self) -> BTreeMap<String, FailureDetails> {
        self.0
            .iter()
            .filter(|(_, outcome)| outcome.violated())
            .filter_map(|(id, outcome)| Some((id.clone(), outcome.failure_details.clone()?)))
            .collect()
    }

    #[must_use]
    pub fn never_satisfied(&self) -> BTreeSet<String> {
        self.0
            .iter()
            .filter(|(_, outcome)| {
                matches!(
                    outcome.kind,
                    AssertionKind::Sometimes | AssertionKind::Reachable
                ) && !outcome.satisfied()
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    #[must_use]
    pub fn key(&self) -> AssertionSet {
        AssertionSet::of(self.key_ids())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
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
    pub park: Option<ParkReport>,
    pub park_read: Option<ParkReport>,
    pub parked_pause: Option<ParkedPauseReport>,
    pub role_pause: Option<RolePauseReport>,
    pub undecided_keys: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParkReport {
    pub site: u64,
    pub edges: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParkedPauseReport {
    pub node: u16,
    #[serde(with = "parked_outcome_name")]
    pub outcome: ParkedOutcome,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RolePauseReport {
    pub node: Option<u16>,
}

pub mod parked_outcome_name {
    use process_proto::events::ParkedOutcome;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(
        outcome: &ParkedOutcome,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(outcome.name())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ParkedOutcome, D::Error> {
        let name = String::deserialize(deserializer)?;
        ParkedOutcome::from_name(&name)
            .ok_or_else(|| D::Error::custom(format!("unknown parked pause outcome {name:?}")))
    }
}

#[derive(Deserialize)]
struct RawEvent {
    antithesis_assert: Option<RawAssert>,
    antithesis_setup: Option<serde_json::Value>,
    harmony_attribution: Option<RawAttribution>,
    harmony_park: Option<ParkReport>,
    harmony_park_read: Option<ParkReport>,
    harmony_parked_pause: Option<ParkedPauseReport>,
    harmony_role_pause: Option<RolePauseReport>,
    harmony_check: Option<RawCheck>,
}

#[derive(Deserialize)]
struct RawCheck {
    undecided_keys: u64,
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

#[derive(Deserialize)]
struct RawDetailsEvent {
    antithesis_assert: RawDetails,
}

#[derive(Deserialize)]
struct RawDetails {
    #[serde(default)]
    details: serde_json::Value,
}

fn failure_details(bytes: &[u8]) -> Option<FailureDetails> {
    match serde_json::from_slice::<RawDetailsEvent>(bytes) {
        Ok(event) => FailureDetails::of(&event.antithesis_assert.details),
        Err(error) => Some(FailureDetails::unreadable(&error)),
    }
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
        let mut outcome = AssertionOutcome {
            kind,
            message,
            location,
            passed,
            failed,
            holds: (kind == AssertionKind::Sometimes && record.hit).then_some(record.condition),
            failure_details: None,
        };
        if outcome.violated() {
            outcome.failure_details = failure_details(bytes);
        }
        Some((id, outcome))
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
        park: raw.harmony_park,
        park_read: raw.harmony_park_read,
        parked_pause: raw.harmony_parked_pause,
        role_pause: raw.harmony_role_pause,
        undecided_keys: raw.harmony_check.map(|check| check.undecided_keys),
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
        let undecided = decode_json_event(
            br#"{"harmony_attribution":{"pid":9},"harmony_check":{"undecided_keys":3}}"#,
        )
        .unwrap();
        assert_eq!(undecided.undecided_keys, Some(3));
        assert_eq!(undecided.pid, Some(9));
    }

    #[test]
    fn a_parked_pause_line_carries_its_node_and_every_outcome_name() {
        for outcome in ParkedOutcome::ALL {
            let line = format!(
                r#"{{"harmony_parked_pause":{{"node":2,"outcome":"{}"}}}}"#,
                outcome.name()
            );
            let event = decode_json_event(line.as_bytes()).unwrap();
            assert_eq!(
                event.parked_pause,
                Some(ParkedPauseReport { node: 2, outcome })
            );
            assert_eq!(
                serde_json::to_string(&event.parked_pause.unwrap()).unwrap(),
                format!(r#"{{"node":2,"outcome":"{}"}}"#, outcome.name())
            );
        }
        assert!(
            decode_json_event(br#"{"harmony_parked_pause":{"node":2,"outcome":"fire"}}"#).is_none()
        );
    }

    #[test]
    fn a_role_pause_line_carries_its_node_or_null() {
        assert_eq!(
            decode_json_event(br#"{"harmony_role_pause":{"node":1}}"#)
                .unwrap()
                .role_pause,
            Some(RolePauseReport { node: Some(1) })
        );
        assert_eq!(
            decode_json_event(br#"{"harmony_role_pause":{"node":null}}"#)
                .unwrap()
                .role_pause,
            Some(RolePauseReport { node: None })
        );
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
        assert!(assertions.never_satisfied().is_empty());
        let (reach, declared) = decode(&record("reachability", true, false, false));
        assertions.record(reach.clone(), declared);
        assert_eq!(assertions.never_satisfied(), BTreeSet::from([reach]));
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

    #[test]
    fn holds_follows_the_latest_hit_record_while_passed_stays_set() {
        let mut assertions = Assertions::default();
        let (id, declared) = decode(&record("sometimes", true, false, false));
        let (_, true_now) = decode(&record("sometimes", true, true, true));
        let (_, false_now) = decode(&record("sometimes", true, true, false));
        let holds = |assertions: &Assertions| {
            let outcome = &assertions.0[&id];
            (outcome.passed, outcome.holds)
        };
        assertions.record(id.clone(), declared.clone());
        assert_eq!(holds(&assertions), (false, None));
        assertions.record(id.clone(), true_now.clone());
        assert_eq!(holds(&assertions), (true, Some(true)));
        assertions.record(id.clone(), declared.clone());
        assert_eq!(holds(&assertions), (true, Some(true)));
        assertions.record(id.clone(), false_now.clone());
        assert_eq!(holds(&assertions), (true, Some(false)));
        assert_eq!(assertions.key(), AssertionSet::of([id.as_str()]));
        let mut later = Assertions::default();
        later.record(id.clone(), true_now);
        assertions.merge(&later);
        assert_eq!(holds(&assertions), (true, Some(true)));
        let mut cleared = Assertions::default();
        cleared.record(id.clone(), false_now);
        assertions.merge(&cleared);
        assert_eq!(holds(&assertions), (true, Some(false)));
    }

    fn detailed(condition: bool, details: &str) -> Vec<u8> {
        format!(
            r#"{{"antithesis_assert":{{"assert_type":"always","message":"m","id":"m","hit":true,"must_hit":true,"condition":{condition},"details":{details}}}}}"#
        )
        .into_bytes()
    }

    fn parsed(details: &FailureDetails) -> serde_json::Value {
        serde_json::from_str(details.text()).unwrap()
    }

    fn nested(depth: usize) -> String {
        format!("{}1{}", "[".repeat(depth), "]".repeat(depth))
    }

    #[test]
    fn a_failed_record_keeps_its_details_and_a_passing_one_keeps_none() {
        let (_, failed) = decode(&detailed(
            false,
            r#"{"node":"n1","highest":482,"answer":333}"#,
        ));
        let details = failed.failure_details.expect("details");
        assert_eq!(
            parsed(&details),
            serde_json::json!({"node": "n1", "highest": 482, "answer": 333})
        );
        assert_eq!(
            details.text(),
            r#"{"answer":333,"highest":482,"node":"n1"}"#
        );
        let (_, passed) = decode(&detailed(true, r#"{"node":"n1"}"#));
        assert_eq!(passed.failure_details, None);
        let (_, null) = decode(&detailed(false, "null"));
        assert_eq!(null.failure_details, None);
        let (_, sometimes) = decode(&record("sometimes", true, true, false));
        assert_eq!(sometimes.failure_details, None);
        let (_, absent) = decode(
            br#"{"antithesis_assert":{"assert_type":"always","id":"m","hit":true,"condition":false}}"#,
        );
        assert!(absent.violated());
        assert_eq!(absent.failure_details, None);
    }

    #[test]
    fn details_too_deep_to_read_still_record_the_violation() {
        let (_, failed) = decode(&detailed(false, &format!(r#"{{"deep":{}}}"#, nested(130))));
        assert!(failed.violated());
        let details = parsed(&failed.failure_details.expect("details"));
        assert!(
            details["unreadable"]
                .as_str()
                .unwrap()
                .contains("recursion limit")
        );
    }

    #[test]
    fn merging_keeps_the_details_of_the_first_failure() {
        let mut assertions = Assertions::default();
        let (id, declared) = decode(&record("always", true, false, false));
        let (_, first) = decode(&detailed(false, r#"{"try":1}"#));
        let (_, second) = decode(&detailed(false, r#"{"try":2}"#));
        assertions.record(id.clone(), declared);
        assertions.record(id.clone(), first);
        assertions.record(id.clone(), second.clone());
        let mut later = Assertions::default();
        later.record(id.clone(), second.clone());
        assertions.merge(&later);
        assert_eq!(
            assertions.violation_details(),
            BTreeMap::from([(
                id.clone(),
                FailureDetails::of(&serde_json::json!({"try": 1})).unwrap()
            )])
        );
        let mut bare_first = Assertions::default();
        bare_first.record(id.clone(), decode(&detailed(false, "null")).1);
        bare_first.record(id.clone(), second);
        assert_eq!(bare_first.violation_details(), BTreeMap::new());
    }

    #[test]
    fn oversize_details_are_replaced_by_a_counted_prefix() {
        let large = serde_json::json!({ "log": "\u{e9}".repeat(FAILURE_DETAILS_LIMIT_BYTES) });
        let serialized = large.to_string();
        let details = parsed(&FailureDetails::of(&large).unwrap());
        assert_eq!(details["truncated_bytes"], serialized.len());
        let prefix = details["prefix"].as_str().unwrap();
        assert!(prefix.len() <= FAILURE_DETAILS_PREFIX_BYTES);
        assert!(prefix.len() > FAILURE_DETAILS_PREFIX_BYTES - 4);
        assert!(serialized.starts_with(prefix));
        let small = serde_json::json!({ "log": "x".repeat(16) });
        assert_eq!(parsed(&FailureDetails::of(&small).unwrap()), small);
    }

    #[test]
    fn details_round_trip_as_json_and_as_postcard_without_changing_their_text() {
        let (id, failed) = decode(&detailed(
            false,
            &format!(
                r#"{{"node":"n1","ok":[1,2.5,null,37e-23],"deep":{}}}"#,
                nested(124)
            ),
        ));
        let text = failed
            .failure_details
            .clone()
            .expect("details")
            .text()
            .to_owned();
        let (other, plain) = decode(&record("always", true, true, true));
        let assertions = Assertions(BTreeMap::from([(id, failed), (other, plain)]));
        let json = serde_json::to_string(&assertions).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value["m"]["failure_details"],
            serde_json::Value::String(text.clone())
        );
        assert!(
            value["id always true"]
                .as_object()
                .unwrap()
                .get("failure_details")
                .is_none()
        );
        let from_json = serde_json::from_str::<Assertions>(&json).unwrap();
        assert_eq!(from_json, assertions);
        let bytes = postcard::to_allocvec(&assertions).unwrap();
        let from_postcard = postcard::from_bytes::<Assertions>(&bytes).unwrap();
        assert_eq!(from_postcard, assertions);
        assert_eq!(
            from_postcard.0["m"]
                .failure_details
                .as_ref()
                .unwrap()
                .text(),
            text
        );
        assert_eq!(postcard::to_allocvec(&from_postcard).unwrap(), bytes);
    }
}
