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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GuidanceOutcome {
    pub maximize: bool,
    pub message: String,
    pub location: String,
    pub best: Option<i64>,
}

impl GuidanceOutcome {
    pub fn merge(&mut self, other: &Self) {
        self.best = match (self.best, other.best) {
            (Some(a), Some(b)) if self.maximize => Some(a.max(b)),
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Guidance(pub BTreeMap<String, GuidanceOutcome>);

impl Guidance {
    pub fn record(&mut self, id: String, outcome: GuidanceOutcome) {
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
}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct AssertionSet {
    pub count: u32,
    #[serde(with = "digest_serde")]
    pub digest: u128,
}

mod digest_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(digest: &u128, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            serializer.serialize_str(&digest.to_string())
        } else {
            digest.serialize(serializer)
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<u128, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            let decimal = String::deserialize(deserializer)?;
            let digest: u128 = decimal.parse().map_err(serde::de::Error::custom)?;
            if digest.to_string() != decimal {
                return Err(serde::de::Error::custom(
                    "assertion digest is not canonical decimal",
                ));
            }
            Ok(digest)
        } else {
            u128::deserialize(deserializer)
        }
    }
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
    pub guidance: Option<(String, GuidanceOutcome)>,
    pub setup_complete: bool,
    pub pid: Option<u64>,
    pub park: Option<ParkReport>,
    pub park_read: Option<ParkReport>,
    pub undecided_keys: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParkReport {
    pub site: u64,
    pub edges: u64,
}

#[derive(Deserialize)]
struct RawEvent {
    antithesis_assert: Option<RawAssert>,
    antithesis_guidance: Option<RawGuidance>,
    antithesis_setup: Option<serde_json::Value>,
    harmony_attribution: Option<RawAttribution>,
    harmony_park: Option<ParkReport>,
    harmony_park_read: Option<ParkReport>,
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

fn must_hit_default() -> bool {
    true
}

#[derive(Deserialize)]
struct RawGuidance {
    id: Option<String>,
    message: Option<String>,
    guidance_type: String,
    #[serde(default)]
    maximize: bool,
    hit: bool,
    guidance_data: Option<serde_json::Value>,
    location: Option<RawLocation>,
}

#[derive(Deserialize)]
struct RawOperands {
    left: serde_json::Number,
    right: serde_json::Number,
}

fn integer(number: &serde_json::Number) -> Option<i128> {
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

impl RawOperands {
    fn gap(&self) -> Option<i64> {
        let gap = match (integer(&self.left), integer(&self.right)) {
            (Some(left), Some(right)) => left - right,
            _ => {
                let gap = self.left.as_f64()? - self.right.as_f64()?;
                if gap.is_nan() {
                    return None;
                }
                gap as i128
            }
        };
        Some(gap.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
    }
}

fn location_text(location: Option<RawLocation>) -> String {
    location
        .map(|location| {
            format!(
                "{}:{} {}",
                location.file.unwrap_or_default(),
                location.begin_line.unwrap_or_default(),
                location.function.unwrap_or_default()
            )
        })
        .unwrap_or_default()
}

fn named(id: Option<String>, message: &str) -> Option<String> {
    let id = id
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| message.to_owned());
    (!id.is_empty()).then_some(id)
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
        let id = named(record.id, &message)?;
        let location = location_text(record.location);
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
    let guidance = raw.antithesis_guidance.and_then(|record| {
        if record.guidance_type != "numeric" {
            return None;
        }
        let message = record.message.unwrap_or_default();
        let id = named(record.id, &message)?;
        let best = if record.hit {
            Some(
                serde_json::from_value::<RawOperands>(record.guidance_data?)
                    .ok()?
                    .gap()?,
            )
        } else {
            None
        };
        Some((
            id,
            GuidanceOutcome {
                maximize: record.maximize,
                message,
                location: location_text(record.location),
                best,
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
        guidance,
        setup_complete,
        pid: raw
            .harmony_attribution
            .and_then(|attribution| attribution.pid),
        park: raw.harmony_park,
        park_read: raw.harmony_park_read,
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
    fn the_message_names_an_assertion_without_an_id() {
        let (id, _) = decode(br#"{"antithesis_assert":{"assert_type":"sometimes","hit":true,"condition":true,"message":"only a message"}}"#);
        assert_eq!(id, "only a message");
    }

    fn guide(id: &str, maximize: bool, hit: bool, left: &str, right: &str) -> Vec<u8> {
        let data = if hit {
            format!(r#""guidance_data":{{"left":{left},"right":{right}}},"#)
        } else {
            String::new()
        };
        format!(
            r#"{{"harmony_attribution":{{"pid":5}},"antithesis_guidance":{{{data}"location":{{"class":"c","function":"f","file":"g.go","begin_line":3,"begin_column":1}},"guidance_type":"numeric","message":"{id}","id":"{id}","maximize":{maximize},"hit":{hit}}}}}"#
        )
        .into_bytes()
    }

    fn guidance(bytes: &[u8]) -> (String, GuidanceOutcome) {
        decode_json_event(bytes)
            .and_then(|event| event.guidance)
            .expect("a guidance record")
    }

    #[test]
    fn numeric_guidance_is_left_minus_right_and_declarations_have_no_value() {
        let (id, lag) = guidance(&guide("lag", true, true, "17", "4"));
        assert_eq!(id, "lag");
        assert!(lag.maximize);
        assert_eq!(lag.best, Some(13));
        assert_eq!(lag.location, "g.go:3 f");
        let (_, declared) = guidance(&guide("lag", true, false, "0", "0"));
        assert_eq!(declared.best, None);
        let (_, negative) = guidance(&guide("gap", false, true, "-5", "7"));
        assert!(!negative.maximize);
        assert_eq!(negative.best, Some(-12));
        let (_, unsigned) = guidance(&guide("u", true, true, "18446744073709551615", "0"));
        assert_eq!(unsigned.best, Some(i64::MAX));
        let (_, below) = guidance(&guide("u", true, true, "0", "18446744073709551615"));
        assert_eq!(below.best, Some(i64::MIN));
        let (_, float) = guidance(&guide("f", true, true, "2.5", "0.6"));
        assert_eq!(float.best, Some(1));
        let boolean = decode_json_event(
            br#"{"antithesis_guidance":{"guidance_type":"boolean","message":"b","id":"b","maximize":true,"hit":true,"guidance_data":{"x":true}}}"#,
        )
        .unwrap();
        assert!(boolean.guidance.is_none());
        let broken = decode_json_event(
            br#"{"antithesis_guidance":{"guidance_type":"numeric","message":"m","maximize":true,"hit":true}}"#,
        )
        .unwrap();
        assert!(broken.guidance.is_none());
        let unnamed = decode_json_event(
            br#"{"antithesis_guidance":{"guidance_type":"numeric","maximize":true,"hit":false}}"#,
        )
        .unwrap();
        assert!(unnamed.guidance.is_none());
    }

    #[test]
    fn guidance_keeps_the_best_value_in_its_direction() {
        let mut up = Guidance::default();
        for (hit, left) in [(false, "0"), (true, "3"), (true, "9"), (true, "-2")] {
            let (id, outcome) = guidance(&guide("lag", true, hit, left, "0"));
            up.record(id, outcome);
        }
        assert_eq!(up.0["lag"].best, Some(9));
        let mut down = Guidance::default();
        for left in ["3", "-4", "8"] {
            let (id, outcome) = guidance(&guide("gap", false, true, left, "0"));
            down.record(id, outcome);
        }
        assert_eq!(down.0["gap"].best, Some(-4));
        let mut merged = Guidance::default();
        let (id, declared) = guidance(&guide("lag", true, false, "0", "0"));
        merged.record(id, declared);
        assert_eq!(merged.0["lag"].best, None);
        merged.merge(&up);
        merged.merge(&down);
        assert_eq!(merged.0["lag"].best, Some(9));
        assert_eq!(merged.0["gap"].best, Some(-4));
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
}
