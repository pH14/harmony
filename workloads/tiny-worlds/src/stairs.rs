// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Key;
use searcher::search::archive::ArchiveEntryReport;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub length: u8,
    pub spacing: u8,
    pub climb: u8,
    pub pattern: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub position: u8,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !(3..=31).contains(&self.length)
            || !(2..self.length).contains(&self.spacing)
            || !(1..self.spacing).contains(&self.climb)
        {
            return Err(
                "stairs needs length 3..=31, spacing 2..length and climb 1..spacing".into(),
            );
        }
        Ok(())
    }

    pub fn action(&self, position: u8) -> u8 {
        crate::pattern_action(self.pattern, position)
    }

    pub fn stair(&self, position: u8) -> bool {
        position > 0 && position < self.length && position.is_multiple_of(self.spacing)
    }

    pub fn landing(&self, position: u8) -> u8 {
        if position < self.spacing {
            0
        } else {
            position / self.spacing * self.spacing + self.climb
        }
    }

    pub fn initial(&self) -> State {
        State { position: 0 }
    }

    pub fn state_is_bounded(&self, s: State) -> bool {
        self.validate().is_ok()
            && (self.goal(s)
                || s.position < self.length
                    && !self.stair(s.position)
                    && self.landing(s.position) <= s.position)
    }

    pub fn goal(&self, s: State) -> bool {
        s.position == self.length
    }

    pub fn step(&self, s: State, action: u8) -> State {
        self.walk(s, action).0
    }

    pub fn forced_steps(&self, s: State, action: u8) -> u8 {
        self.walk(s, action).1
    }

    pub fn cut_short(&self, s: State, action: u8) -> bool {
        let (after, forced) = self.walk(s, action);
        forced > 0 && forced < self.climb && self.goal(after)
    }

    fn walk(&self, s: State, action: u8) -> (State, u8) {
        if action > 3 || !self.state_is_bounded(s) || self.goal(s) {
            return (s, 0);
        }
        if action != self.action(s.position) {
            return (
                State {
                    position: self.landing(s.position),
                },
                0,
            );
        }
        let position = s.position + 1;
        if !self.stair(position) {
            return (State { position }, 0);
        }
        let forced = self.climb.min(self.length - position);
        (
            State {
                position: position + forced,
            },
            forced,
        )
    }

    pub fn key(&self, s: State) -> Key {
        Key {
            stock: 0,
            place: u16::from(s.position),
            context: 0,
            charge: 0,
            health: 0,
            goal: self.goal(s),
            tier: u16::from(s.position / self.spacing),
        }
    }

    pub fn reachable(&self) -> Result<bool, String> {
        self.validate()?;
        crate::reachable(self.initial(), |s| self.goal(s), |s, a| self.step(s, a))
    }

    pub fn record_cost(&self, actions: &[u8]) -> (u64, bool) {
        let mut s = self.initial();
        let mut cost = 0;
        let mut cut_short = false;
        for &action in actions {
            cost += 1 + u64::from(self.forced_steps(s, action));
            cut_short |= self.cut_short(s, action);
            s = self.step(s, action);
        }
        (cost, cut_short)
    }

    pub fn report<K, M>(&self, entries: &[ArchiveEntryReport<u8, K, M>]) -> serde_json::Value {
        let (mut forced, mut cut_short, mut charged) = (0_u64, 0_u64, 0_u64);
        for entry in entries {
            let (record, cut) = self.record_cost(&entry.input.actions);
            forced += u64::from(record != entry.input.actions.len() as u64);
            cut_short += u64::from(cut);
            charged += u64::from(
                entry
                    .shrink_total
                    .is_some_and(|total| total.primary == record),
            );
        }
        serde_json::json!({
            "entries": entries.len(),
            "entries_with_forced_steps": forced,
            "entries_cut_short": cut_short,
            "entries_charged_record_cost": charged,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            length: 13,
            spacing: 5,
            climb: 2,
            pattern: crate::test_seed(),
        }
    }

    fn climb_to(w: &Config, target: u8) -> State {
        let mut s = w.initial();
        while s.position < target {
            s = w.step(s, w.action(s.position));
        }
        s
    }

    #[test]
    fn an_action_that_reaches_a_stair_runs_the_forced_climb_within_the_action() {
        let w = config();
        let below = climb_to(&w, 4);
        assert_eq!(below.position, 4);
        assert_eq!(w.forced_steps(below, w.action(4)), 2);
        let landed = w.step(below, w.action(4));
        assert_eq!(landed.position, 7);
        assert!(!w.cut_short(below, w.action(4)));
        for skipped in 5..7 {
            assert!(!w.state_is_bounded(State { position: skipped }));
        }
        assert_eq!(w.key(landed).tier, 1);
        assert_eq!(w.forced_steps(w.initial(), w.action(0)), 0);
    }

    #[test]
    fn a_climb_that_passes_the_goal_stops_there_and_is_cut_short() {
        let w = config();
        let below = climb_to(&w, 9);
        assert_eq!(below.position, 9);
        let action = w.action(9);
        assert_eq!(w.step(below, action).position, 12);
        assert!(!w.cut_short(below, action));
        let exact = Config { length: 12, ..w };
        assert_eq!(exact.forced_steps(below, action), 2);
        assert!(exact.goal(exact.step(below, action)));
        assert!(!exact.cut_short(below, action));
        let short = Config { length: 11, ..w };
        assert_eq!(short.forced_steps(below, action), 1);
        assert!(short.goal(short.step(below, action)));
        assert!(short.cut_short(below, action));
        for w in [w, exact, short] {
            assert!(w.reachable().unwrap());
        }
    }

    #[test]
    fn a_wrong_action_slides_back_to_the_last_landing() {
        let w = config();
        let s = climb_to(&w, 9);
        let wrong = (0..4).find(|&a| a != w.action(9)).unwrap();
        assert_eq!(w.step(s, wrong).position, 7);
        assert_eq!(w.forced_steps(s, wrong), 0);
        let s = climb_to(&w, 2);
        let wrong = (0..4).find(|&a| a != w.action(2)).unwrap();
        assert_eq!(w.step(s, wrong).position, 0);
    }

    fn workload(w: Config) -> crate::Workload {
        crate::Workload {
            keying: crate::Hand::<false>,
            config: crate::worlds::World::Stairs(w),
            broken: false,
            scale: None,
        }
    }

    #[test]
    fn execution_work_counts_the_forced_steps_and_the_archive_charges_one_transition() {
        use searcher::search::campaign::{Evaluation, TargetExecution};
        let w = config();
        let workload = workload(w);
        let mut target = workload.new_target().unwrap();
        let mut milestones = false;
        for position in 0..4 {
            workload
                .apply_action(&mut target, &w.action(position), &mut milestones)
                .unwrap();
        }
        assert_eq!(workload.execution_work(&target), 4);
        let action = w.action(4);
        workload
            .apply_action(&mut target, &action, &mut milestones)
            .unwrap();
        assert_eq!(workload.execution_work(&target), 7);
        let observations = workload.rollout_observations(&target);
        assert_eq!(workload.action_cost(&action, &observations), 1);
    }

    #[test]
    fn a_campaign_climbs_to_a_cut_short_goal_and_replays() {
        let w = Config {
            length: 11,
            ..config()
        };
        let report = crate::run(&workload(w), crate::test_seed(), 20_000, true).unwrap();
        assert_eq!(report["verified"], true);
        assert_eq!(report["success"], true, "{report}");
        let stairs = &report["stairs"];
        let entries = stairs["entries"].as_u64().unwrap();
        let forced = stairs["entries_with_forced_steps"].as_u64().unwrap();
        assert!(forced > 0 && forced < entries, "{stairs}");
        assert_eq!(
            stairs["entries_charged_record_cost"].as_u64(),
            Some(entries - forced),
            "{stairs}"
        );
    }

    #[test]
    fn strict_schema_and_bounds() {
        let w = config();
        assert!(Config { length: 2, ..w }.validate().is_err());
        assert!(Config { length: 32, ..w }.validate().is_err());
        assert!(Config { spacing: 13, ..w }.validate().is_err());
        assert!(Config { climb: 5, ..w }.validate().is_err());
        assert!(Config { climb: 0, ..w }.validate().is_err());
        for position in [5, 6, 14] {
            assert!(!w.state_is_bounded(State { position }));
        }
        let mut v = serde_json::to_value(w).unwrap();
        v["unused"] = true.into();
        assert!(serde_json::from_value::<Config>(v).is_err());
    }
}
