// SPDX-License-Identifier: AGPL-3.0-or-later

use engine::target::Kind;
use faults_workload::{archive::archive_key, assertion::AssertionKind, target::FaultObservations};
use serde::{Deserialize, Serialize};

pub const LIFECYCLE: [&str; 11] = [
    "hooks_finished",
    "hooks_running",
    "alive",
    "event_ready",
    "event_kill_fires",
    "event_park_fires",
    "checks_finished",
    "checks_running",
    "workload_running",
    "park_held",
    "kill_armed",
];

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Guide {
    pub id: String,
    pub maximize: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Layout {
    pub sometimes: Vec<String>,
    pub invariants: Vec<String>,
    pub guidance: Vec<Guide>,
}

pub fn order_preserving(value: i64) -> u64 {
    (value as u64) ^ (1 << 63)
}

impl Layout {
    pub fn from_declarations(root: &FaultObservations) -> Self {
        let mut out = Layout::default();
        for (id, outcome) in &root.assertions.0 {
            match outcome.kind {
                AssertionKind::Sometimes | AssertionKind::Reachable => {
                    out.sometimes.push(id.clone())
                }
                AssertionKind::Always
                | AssertionKind::AlwaysOrUnreachable
                | AssertionKind::Unreachable => out.invariants.push(id.clone()),
            }
        }
        out.guidance = root
            .guidance
            .0
            .iter()
            .map(|(id, g)| Guide {
                id: id.clone(),
                maximize: g.maximize,
            })
            .collect();
        out
    }

    pub fn kinds(&self) -> Vec<Kind> {
        let mut kinds = vec![Kind::Identity; LIFECYCLE.len() + self.sometimes.len()];
        kinds.extend(self.guidance.iter().map(|g| {
            if g.maximize {
                Kind::Higher
            } else {
                Kind::Lower
            }
        }));
        kinds
    }

    pub fn sometimes_index(&self, at: usize) -> usize {
        LIFECYCLE.len() + at
    }

    pub fn scalars(&self, o: &FaultObservations, out: &mut Vec<u64>) {
        let key = archive_key(o);
        out.clear();
        out.extend([
            key.hooks_finished,
            key.hooks_running,
            key.alive,
            key.event_ready,
            key.event_kill_fires,
            key.event_park_fires,
            key.checks_finished,
            u64::from(key.checks_running),
            u64::from(key.workload_running),
            u64::from(key.park_held),
            u64::from(key.kill_armed),
        ]);
        out.extend(self.sometimes.iter().map(|id| {
            u64::from(
                o.assertions
                    .0
                    .get(id)
                    .is_some_and(|outcome| outcome.feeds_the_key()),
            )
        }));
        out.extend(self.guidance.iter().map(|g| {
            match o.guidance.0.get(&g.id).and_then(|outcome| outcome.best) {
                Some(value) => order_preserving(value),
                None if g.maximize => 0,
                None => u64::MAX,
            }
        }));
    }

    pub fn array(&self, o: &FaultObservations, out: &mut Vec<u8>) {
        out.clear();
        let bits = self.sometimes.len() + self.invariants.len();
        out.resize(bits.div_ceil(8), 0);
        let passed = self
            .sometimes
            .iter()
            .map(|id| o.assertions.0.get(id).is_some_and(|a| a.feeds_the_key()));
        let violated = self
            .invariants
            .iter()
            .map(|id| o.assertions.0.get(id).is_some_and(|a| a.violated()));
        for (bit, set) in passed.chain(violated).enumerate() {
            if set {
                out[bit / 8] |= 1 << (bit % 8);
            }
        }
        out.extend(o.state.values().map(|&v| bit_length(v)));
    }
}

fn bit_length(v: i64) -> u8 {
    if v <= 0 {
        0
    } else {
        (64 - v.leading_zeros()) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faults_workload::assertion::{AssertionOutcome, GuidanceOutcome};

    fn outcome(kind: AssertionKind, passed: bool, failed: bool) -> AssertionOutcome {
        AssertionOutcome {
            kind,
            message: String::new(),
            location: String::new(),
            passed,
            failed,
        }
    }

    fn guide(maximize: bool, best: Option<i64>) -> GuidanceOutcome {
        GuidanceOutcome {
            maximize,
            message: String::new(),
            location: String::new(),
            best,
        }
    }

    fn declared() -> FaultObservations {
        let mut o = FaultObservations::default();
        for (id, kind) in [
            ("b reach", AssertionKind::Reachable),
            ("a sometimes", AssertionKind::Sometimes),
            ("c always", AssertionKind::Always),
            ("d never", AssertionKind::Unreachable),
        ] {
            o.assertions.record(id.into(), outcome(kind, false, false));
        }
        o.guidance.record("lag".into(), guide(true, None));
        o.guidance.record("gap".into(), guide(false, None));
        o
    }

    #[test]
    fn declarations_fix_a_sorted_layout_with_kinds_by_direction() {
        let layout = Layout::from_declarations(&declared());
        assert_eq!(layout.sometimes, ["a sometimes", "b reach"]);
        assert_eq!(layout.invariants, ["c always", "d never"]);
        assert_eq!(
            layout
                .guidance
                .iter()
                .map(|g| g.id.as_str())
                .collect::<Vec<_>>(),
            ["gap", "lag"]
        );
        let kinds = layout.kinds();
        assert_eq!(kinds.len(), LIFECYCLE.len() + 4);
        assert!(kinds[..LIFECYCLE.len() + 2].iter().all(|k| k.is_identity()));
        assert_eq!(kinds[LIFECYCLE.len() + 2..], [Kind::Lower, Kind::Higher]);
    }

    #[test]
    fn scalars_follow_passes_and_guidance_in_numeric_order() {
        let layout = Layout::from_declarations(&declared());
        let mut root = Vec::new();
        layout.scalars(&declared(), &mut root);
        assert!(
            root[LIFECYCLE.len()..LIFECYCLE.len() + 2]
                .iter()
                .all(|v| *v == 0)
        );
        assert_eq!(root[LIFECYCLE.len() + 2], u64::MAX);
        assert_eq!(root[LIFECYCLE.len() + 3], 0);
        let mut later = declared();
        later.alive = 0b101;
        later.hooks_started = 20;
        later.hooks_finished = 2;
        later.assertions.record(
            "b reach".into(),
            outcome(AssertionKind::Reachable, true, false),
        );
        later.assertions.record(
            "c always".into(),
            outcome(AssertionKind::Always, true, true),
        );
        later.guidance.record("lag".into(), guide(true, Some(-3)));
        later.guidance.record("gap".into(), guide(false, Some(7)));
        let mut scalars = Vec::new();
        layout.scalars(&later, &mut scalars);
        assert_eq!(scalars[..3], [2, 8, 0b101]);
        assert_eq!(scalars[layout.sometimes_index(0)], 0);
        assert_eq!(scalars[layout.sometimes_index(1)], 1);
        assert_eq!(scalars[LIFECYCLE.len() + 2], order_preserving(7));
        assert_eq!(scalars[LIFECYCLE.len() + 3], order_preserving(-3));
        assert!(order_preserving(-3) > 0 && order_preserving(-3) < order_preserving(0));
        assert!(order_preserving(i64::MIN) < order_preserving(i64::MAX));
        later.edge_digest = 9;
        later.edge_crossings = 4;
        later.state = [("n1 lag", 5), ("n0 role", 1), ("n0 lag", 0), ("n2 lag", -2)]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect();
        let mut array = Vec::new();
        layout.array(&later, &mut array);
        assert_eq!(array, [0b0110, 0, 1, 3, 0]);
        assert_eq!(bit_length(i64::MAX), 63);
    }
}
