// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use searcher::search::campaign::{
    CampaignAdmissionDecision, CampaignSpliceRecord, CampaignStreamRecord,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub length: u8,
    pub pattern: u32,
    pub attack: u8,
    pub shifted: bool,
    pub upgrade_required: bool,
    pub ranked_upgrade: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub position: u8,
    pub lane: u8,
    pub phase: u8,
    pub goal: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Trace {
    pub sequence: u64,
    pub action: u8,
    pub candidate: bool,
    pub objective: bool,
    pub before: State,
    pub after: State,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(2..=16).contains(&self.length) || self.attack > 3 {
            return Err("route length must be 2..=16 and attack 0..=3".into());
        }
        if self.route_action(0) == self.align_action() {
            return Err("alignment must differ from the route's first action".into());
        }
        Ok(())
    }
    pub fn route_action(&self, position: u8) -> u8 {
        crate::pattern_action(self.pattern.into(), position)
    }
    pub fn align_action(&self) -> u8 {
        (self.attack + 1) % 4
    }
    pub fn initial(&self) -> State {
        State {
            position: 0,
            lane: 0,
            phase: 0,
            goal: false,
        }
    }
    pub fn valid_state(&self, s: State) -> bool {
        s.position <= self.length
            && s.lane <= 1
            && s.phase <= 2
            && (s.lane == 0 || (self.shifted && s.position == 0 && s.phase == 2))
            && (self.upgrade_required || s.phase == 0)
            && s.goal == (s.position == self.length && (!self.upgrade_required || s.phase == 2))
    }
    pub fn goal(&self, s: State) -> bool {
        self.valid_state(s) && s.goal
    }
    pub fn step(&self, mut s: State, action: u8) -> State {
        if action > 3 || !self.valid_state(s) || s.goal {
            return s;
        }
        if s.lane == 1 {
            if action == self.align_action() {
                s.lane = 0;
            }
            return s;
        }
        if s.position == self.length {
            if action == self.attack {
                s.position = 0;
                s.phase = 1;
            }
            return s;
        }
        if s.position == 0 && s.phase == 1 && action == self.attack {
            s.phase = 2;
            s.lane = u8::from(self.shifted);
            return s;
        }
        if action == self.route_action(s.position) {
            s.position += 1;
            s.goal = s.position == self.length && (!self.upgrade_required || s.phase == 2);
        } else {
            s.position = 0;
        }
        s
    }
    pub fn key(&self, s: State) -> Key {
        Key {
            stock: 0,
            place: 2 * u16::from(s.position) + u16::from(s.lane),
            context: 0,
            charge: s.phase,
            health: 0,
            goal: self.goal(s),
            tier: u16::from(self.ranked_upgrade && s.phase == 2),
        }
    }
    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        crate::reachable(self.initial(), |s| self.goal(s), |s, a| self.step(s, a))
    }
}

pub fn summarize(
    trace: &[Trace],
    stream: &[u8],
    length: u8,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if trace.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    let mut by_sequence = std::collections::BTreeMap::<u64, Vec<&Trace>>::new();
    for t in trace {
        by_sequence.entry(t.sequence).or_default().push(t);
    }
    let mut states = std::collections::BTreeMap::<u64, State>::new();
    let mut first_arrivals = std::collections::BTreeMap::new();
    let mut transfers = Vec::new();
    let mut first_objective_path = None;
    let mut first_acquisition = None;
    let mut first_alignment = None;
    let mut first_endpoint = None;
    let mut work = 0;
    for record in crate::stream_records(stream)? {
        let CampaignStreamRecord::Job(record) = record else {
            continue;
        };
        work += record.execution_work;
        let sequence = record.sequence;
        let Some(actions) = by_sequence.remove(&sequence) else {
            if record.execution_work != 0 {
                return Err("route job missing trace".into());
            }
            continue;
        };
        let decisions = &record.decisions;
        if actions
            .iter()
            .map(|a| usize::from(a.candidate) + usize::from(a.objective))
            .sum::<usize>()
            != decisions.len()
        {
            return Err("route trace and decisions differ".into());
        }
        let parent_id = record.parent_id;
        if let Some(held) = states.get(&parent_id) {
            if *held != actions[0].before {
                return Err("route parent state mismatch".into());
            }
        } else {
            states.insert(parent_id, actions[0].before);
        }
        let (donor, leaf) = match record.splice {
            Some(CampaignSpliceRecord::Tail {
                donor_id, leaf_id, ..
            }) => (
                states.get(&donor_id).copied(),
                states.get(&leaf_id).copied(),
            ),
            _ => (None, None),
        };
        let path = crate::path_name(record.selector.path);
        let before_objective = first_objective_path.is_none();
        let upgraded_advance = actions
            .iter()
            .any(|a| a.before.phase == 2 && a.after.position > a.before.position);
        if before_objective
            && path == "continuation"
            && actions[0].before.phase == 2
            && upgraded_advance
        {
            transfers.push(serde_json::json!({"sequence":sequence,"work":work,"parent":actions[0].before,
                "donor":donor,"leaf":leaf,"after":actions.last().unwrap().after,
                "non_upgraded_donor":donor.is_some_and(|d| d.phase < 2) && leaf.is_some_and(|l| l.phase < 2),
                "actions":actions.iter().map(|a| a.action).collect::<Vec<_>>()}));
        }
        let mut decisions = decisions.iter();
        for a in &actions {
            if before_objective
                && a.before.phase != 2
                && a.after.phase == 2
                && first_acquisition.is_none()
            {
                first_acquisition =
                    Some(serde_json::json!({"sequence":sequence,"work":work,"lane":a.after.lane}));
            }
            if before_objective && a.after.position == length && first_endpoint.is_none() {
                first_endpoint =
                    Some(serde_json::json!({"sequence":sequence,"work":work,"path":path}));
            }
            if before_objective
                && a.before.lane == 1
                && a.after.lane == 0
                && first_alignment.is_none()
            {
                first_alignment =
                    Some(serde_json::json!({"sequence":sequence,"work":work,"path":path}));
            }
            if before_objective && a.after.phase == 2 {
                first_arrivals
                    .entry(format!("{}:{}", a.after.position, a.after.lane))
                    .or_insert(serde_json::json!({"path":path,"sequence":sequence,"work":work}));
            }
            if a.objective && first_objective_path.is_none() {
                first_objective_path = Some(path.to_string());
            }
            if a.objective
                && *decisions.next().ok_or("missing objective decision")?
                    != CampaignAdmissionDecision::Objective
            {
                return Err("route objective decision mismatch".into());
            }
            if !a.candidate {
                continue;
            }
            if let CampaignAdmissionDecision::Retained { id } =
                decisions.next().ok_or("missing route admission")?
            {
                states.insert(*id, a.after);
            }
        }
    }
    if !by_sequence.is_empty() {
        return Err("unmatched route trace".into());
    }
    Ok(
        serde_json::json!({"first_upgraded_arrivals":first_arrivals,"upgraded_continuation_transfers":transfers,
        "first_objective_path":first_objective_path,"first_endpoint":first_endpoint,"first_acquisition":first_acquisition,"first_alignment":first_alignment}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        Config {
            length: 6,
            pattern: 0x92492490,
            attack: 2,
            shifted: true,
            upgrade_required: true,
            ranked_upgrade: false,
        }
    }
    fn tape(w: &Config, s: State, actions: &[u8]) -> State {
        actions.iter().fold(s, |s, &a| w.step(s, a))
    }
    #[test]
    fn learned_route_requires_upgrade_and_compatible_position() {
        let w = config();
        assert!(w.reachable().unwrap());
        let route: Vec<_> = (0..w.length).map(|i| w.route_action(i)).collect();
        let blocked = tape(&w, w.initial(), &route);
        assert_eq!(blocked.position, w.length);
        assert!(!w.goal(blocked));
        let scouted = w.step(blocked, w.attack);
        let shifted = w.step(scouted, w.attack);
        assert_eq!((shifted.position, shifted.phase, shifted.lane), (0, 2, 1));
        assert!(!w.goal(tape(&w, shifted, &route)));
        let aligned = w.step(shifted, w.align_action());
        assert!(w.goal(tape(&w, aligned, &route)));
        assert_ne!(w.key(aligned).place, w.key(shifted).place);
        assert_eq!(w.key(aligned).place, w.key(w.initial()).place);
        assert!(w.key(aligned).charge > w.key(w.initial()).charge);
        assert_eq!(w.step(w.initial(), w.attack).phase, 0);
    }
    #[test]
    fn bounded_states_and_strict_schema() {
        let w = config();
        for length in [0, 1, 17] {
            assert!(Config { length, ..w }.validate().is_err());
        }
        assert!(Config { attack: 4, ..w }.validate().is_err());
        assert!(Config { pattern: 3, ..w }.validate().is_err());
        assert!(!w.valid_state(State {
            position: 1,
            lane: 1,
            phase: 2,
            goal: false
        }));
        assert!(!w.valid_state(State {
            goal: true,
            ..w.initial()
        }));
        let mut v = serde_json::to_value(w).unwrap();
        v["unused"] = true.into();
        assert!(serde_json::from_value::<Config>(v).is_err());
        let no_upgrade = Config {
            upgrade_required: false,
            ..w
        };
        let route: Vec<_> = (0..w.length).map(|i| w.route_action(i)).collect();
        assert!(no_upgrade.goal(tape(&no_upgrade, no_upgrade.initial(), &route)));
    }
    #[test]
    fn campaign_trace_joins_executed_jobs_and_replays() {
        for shifted in [false, true] {
            let w = Config {
                shifted,
                ..config()
            };
            let workload = crate::Workload {
                config: crate::worlds::World::Route(w),
                broken: false,
            };
            let report = crate::run(&workload, crate::test_seed(), 4000, true).unwrap();
            assert_eq!(report["verified"], true);
            assert!(report["route_evidence"]["first_upgraded_arrivals"].is_object());
            let transfers = report["route_evidence"]["upgraded_continuation_transfers"]
                .as_array()
                .unwrap();
            for transfer in transfers {
                let parent: State = serde_json::from_value(transfer["parent"].clone()).unwrap();
                let donor: State = serde_json::from_value(transfer["donor"].clone()).unwrap();
                let leaf: State = serde_json::from_value(transfer["leaf"].clone()).unwrap();
                let after: State = serde_json::from_value(transfer["after"].clone()).unwrap();
                let actions: Vec<u8> = serde_json::from_value(transfer["actions"].clone()).unwrap();
                assert_eq!((parent.position, parent.lane), (donor.position, donor.lane));
                assert_eq!(parent.phase, 2);
                assert_eq!(
                    transfer["non_upgraded_donor"],
                    donor.phase < 2 && leaf.phase < 2
                );
                assert_eq!(tape(&w, donor, &actions), leaf);
                assert_eq!(tape(&w, parent, &actions), after);
            }
            let trace: Vec<Trace> =
                serde_json::from_value(report["evidence"]["route_trace"].clone()).unwrap();
            assert!(!trace.is_empty());
            for t in trace {
                assert_eq!(w.step(t.before, t.action), t.after);
            }
            assert!(
                report["evidence"]["observations"].as_u64().unwrap()
                    <= report["work"].as_u64().unwrap()
            );
        }
    }
    #[test]
    fn trace_join_rejects_missing_admissions_and_wrong_parent_states() {
        let w = config();
        let first = Trace {
            sequence: 1,
            action: w.route_action(0),
            candidate: true,
            objective: false,
            before: w.initial(),
            after: w.step(w.initial(), w.route_action(0)),
        };
        let mut record = serde_json::json!({"event":"job","sequence":1,"worker":0,"parent_id":0,
            "mutation_seed":0,"execution_work":1,"result_sha256":"","decisions":[],
            "mixture_weight":0,"splice_weight":0,"selector":{"path":"tiers"}});
        assert!(
            summarize(
                std::slice::from_ref(&first),
                &serde_json::to_vec(&record).unwrap(),
                6
            )
            .is_err()
        );
        record["decisions"] = serde_json::json!([{"decision":"retained","id":1}]);
        assert!(
            summarize(
                std::slice::from_ref(&first),
                &serde_json::to_vec(&record).unwrap(),
                6
            )
            .is_ok()
        );
        let second = Trace {
            sequence: 2,
            ..first.clone()
        };
        let mut stream = serde_json::to_vec(&record).unwrap();
        record["sequence"] = 2.into();
        record["parent_id"] = 1.into();
        stream.push(b'\n');
        stream.extend(serde_json::to_vec(&record).unwrap());
        assert!(
            summarize(&[first, second], &stream, 6)
                .unwrap_err()
                .to_string()
                .contains("parent state mismatch")
        );
    }
    #[test]
    fn trace_join_rejects_wrong_objective_event_order() {
        let w = config();
        let trace = Trace {
            sequence: 1,
            action: 0,
            candidate: false,
            objective: true,
            before: w.initial(),
            after: w.initial(),
        };
        let record = serde_json::json!({"event":"job","sequence":1,"worker":0,"parent_id":0,
            "mutation_seed":0,"execution_work":1,"result_sha256":"",
            "decisions":[{"decision":"retained","id":1}],
            "mixture_weight":0,"splice_weight":0,"selector":{"path":"tiers"}});
        assert!(
            summarize(&[trace], &serde_json::to_vec(&record).unwrap(), 6)
                .unwrap_err()
                .to_string()
                .contains("objective decision mismatch")
        );
    }
    #[test]
    fn ranked_upgrade_raises_the_tier_and_reports_both_trips() {
        let w = Config {
            shifted: false,
            ranked_upgrade: true,
            ..config()
        };
        let route: Vec<_> = (0..w.length).map(|i| w.route_action(i)).collect();
        let blocked = tape(&w, w.initial(), &route);
        let upgraded = tape(&w, blocked, &[w.attack, w.attack]);
        assert_eq!((upgraded.position, upgraded.phase), (0, 2));
        assert_eq!(w.key(blocked).tier, 0);
        assert_eq!(w.key(upgraded).tier, 1);
        assert_eq!(w.key(upgraded).place, w.key(w.initial()).place);
        assert_eq!(
            Config {
                ranked_upgrade: false,
                ..w
            }
            .key(upgraded)
            .tier,
            0
        );
        let workload = crate::Workload {
            config: crate::worlds::World::Route(w),
            broken: false,
        };
        let report = crate::run(&workload, crate::test_seed(), 4000, true).unwrap();
        assert_eq!(report["verified"], true);
        let evidence = &report["route_evidence"];
        if let Some(objective) = report["first_objective_work"].as_u64() {
            let endpoint = evidence["first_endpoint"]["work"].as_u64().unwrap();
            let acquisition = evidence["first_acquisition"]["work"].as_u64().unwrap();
            assert!(endpoint <= acquisition && acquisition <= objective);
        }
    }
}
