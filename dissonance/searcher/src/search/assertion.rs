// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cmp::Ordering,
    collections::BTreeMap,
    error::Error,
    fmt::{self, Debug},
    marker::PhantomData,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::search::{
    archive::{ArchiveKey, Input, MAX_ENTRIES_PER_KEY},
    plan::{self, Stage, StageProgress},
};

pub enum Declaration<V, S: Stage<State: Sized> + 'static, T> {
    Always {
        name: &'static str,
        holds: fn(&V) -> bool,
    },
    SometimesAll {
        name: &'static str,
        plan: fn(&V) -> &'static [S],
        state: fn(&V) -> S::State,
    },
    SometimesEach {
        name: &'static str,
        values: fn(&V) -> T,
    },
    Value {
        name: &'static str,
        value: fn(&V) -> u64,
    },
}

impl<V, S: Stage<State: Sized> + 'static, T> Declaration<V, S, T> {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Always { name, .. }
            | Self::SometimesAll { name, .. }
            | Self::SometimesEach { name, .. }
            | Self::Value { name, .. } => name,
        }
    }

    #[must_use]
    pub fn form(&self) -> &'static str {
        match self {
            Self::Always { .. } => "always",
            Self::SometimesAll { .. } => "sometimes_all",
            Self::SometimesEach { .. } => "sometimes_each",
            Self::Value { .. } => "value",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Strategy {
    pub stage: Option<&'static str>,
    pub spread: &'static [&'static str],
    pub prefer: &'static [&'static [&'static str]],
}

pub trait Assertions: 'static {
    type View: Copy + Ord + Debug + Send + Sync + Serialize + DeserializeOwned;
    type Stage: Stage<State: Sized> + 'static;
    type Values: Copy + Ord + Debug + Default + Serialize + DeserializeOwned;
    type Identity: Copy + Ord + Debug + Serialize + DeserializeOwned;
    type Lineage: Clone + Default + Serialize + DeserializeOwned;

    const DECLARATIONS: &'static [Declaration<Self::View, Self::Stage, Self::Values>];

    const STRATEGY: Strategy;

    fn identity(view: &Self::View) -> Self::Identity;

    fn complete(view: Self::View, parent: Option<(Self::View, &Self::Lineage)>) -> Self::View;

    fn record(lineage: &mut Self::Lineage, view: Self::View);

    fn capacity() -> usize {
        MAX_ENTRIES_PER_KEY
    }
}

pub type AssertionPlace<W> = (u8, <W as Assertions>::Values);

type Declared<W> =
    Declaration<<W as Assertions>::View, <W as Assertions>::Stage, <W as Assertions>::Values>;

pub struct AssertionKey<W: Assertions> {
    view: W::View,
    declarations: PhantomData<fn() -> W>,
}

impl<W: Assertions> AssertionKey<W> {
    #[must_use]
    pub fn new(view: W::View) -> Self {
        Self {
            view,
            declarations: PhantomData,
        }
    }

    #[must_use]
    pub fn view(self) -> W::View {
        self.view
    }

    #[must_use]
    pub fn policy_identifier() -> String {
        let declared = W::DECLARATIONS
            .iter()
            .map(|declaration| (declaration.form(), declaration.name()))
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&(W::STRATEGY, declared)).unwrap_or_default();
        let digest = Sha256::digest(bytes);
        format!(
            "assertion_key_{}",
            digest[..8]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
    }

    #[must_use]
    pub fn unresolved() -> Vec<&'static str> {
        let stage = W::STRATEGY
            .stage
            .filter(|name| Self::declared("sometimes_all", name).is_none());
        let spread = W::STRATEGY
            .spread
            .iter()
            .copied()
            .filter(|name| Self::declared("sometimes_each", name).is_none());
        let prefer = W::STRATEGY
            .prefer
            .iter()
            .flat_map(|names| names.iter().copied())
            .filter(|name| Self::declared("value", name).is_none());
        stage.into_iter().chain(spread).chain(prefer).collect()
    }

    fn declared(form: &str, name: &str) -> Option<&'static Declared<W>> {
        W::DECLARATIONS
            .iter()
            .find(|declaration| declaration.form() == form && declaration.name() == name)
    }

    pub fn places(self) -> impl Iterator<Item = AssertionPlace<W>> {
        W::STRATEGY
            .spread
            .iter()
            .filter_map(move |name| match Self::declared("sometimes_each", name) {
                Some(Declaration::SometimesEach { values, .. }) => Some(values(&self.view)),
                _ => None,
            })
            .enumerate()
            .map(|(index, values)| (u8::try_from(index).unwrap_or(u8::MAX), values))
    }

    pub fn violations(self) -> impl Iterator<Item = &'static str> {
        W::DECLARATIONS
            .iter()
            .filter_map(move |declaration| match declaration {
                Declaration::Always { name, holds } if !holds(&self.view) => Some(*name),
                _ => None,
            })
    }
}

impl<W: Assertions> Clone for AssertionKey<W> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<W: Assertions> Copy for AssertionKey<W> {}

impl<W: Assertions> PartialEq for AssertionKey<W> {
    fn eq(&self, other: &Self) -> bool {
        self.view == other.view
    }
}

impl<W: Assertions> Eq for AssertionKey<W> {}

impl<W: Assertions> PartialOrd for AssertionKey<W> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<W: Assertions> Ord for AssertionKey<W> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.view.cmp(&other.view)
    }
}

impl<W: Assertions> Debug for AssertionKey<W> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.view.fmt(formatter)
    }
}

impl<W: Assertions> Serialize for AssertionKey<W> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.view.serialize(serializer)
    }
}

impl<'de, W: Assertions> Deserialize<'de> for AssertionKey<W> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        W::View::deserialize(deserializer).map(Self::new)
    }
}

impl<W: Assertions> ArchiveKey for AssertionKey<W> {
    type Place = AssertionPlace<W>;
    type Progress = StageProgress;
    type Identity = W::Identity;

    fn place(self) -> Self::Place {
        self.places().next().unwrap_or_default()
    }

    fn progress(self) -> Self::Progress {
        match W::STRATEGY
            .stage
            .and_then(|name| Self::declared("sometimes_all", name))
        {
            Some(Declaration::SometimesAll { plan, state, .. }) => {
                plan::locate(plan(&self.view), &state(&self.view), None)
            }
            _ => StageProgress::default(),
        }
    }

    fn identity(self) -> Self::Identity {
        W::identity(&self.view)
    }

    fn capacity() -> usize {
        W::capacity()
    }

    fn preferences() -> usize {
        W::STRATEGY.prefer.len()
    }

    fn preference_cmp(self, preference: usize, other: Self) -> Ordering {
        W::STRATEGY
            .prefer
            .get(preference)
            .into_iter()
            .flat_map(|names| names.iter())
            .filter_map(|name| match Self::declared("value", name) {
                Some(Declaration::Value { value, .. }) => {
                    Some(value(&self.view).cmp(&value(&other.view)))
                }
                _ => None,
            })
            .find(|ordering| ordering.is_ne())
            .unwrap_or(Ordering::Equal)
    }

    type Lineage = W::Lineage;

    fn complete(self, parent: Option<(Self, &Self::Lineage)>) -> Self {
        Self::new(W::complete(
            self.view,
            parent.map(|(key, lineage)| (key.view, lineage)),
        ))
    }

    fn record(lineage: &mut Self::Lineage, key: Self) {
        W::record(lineage, key.view);
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Finding<A: Ord> {
    pub sequence: u64,
    pub actions: u64,
    pub input: Input<A>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Findings<A: Ord>(BTreeMap<String, Finding<A>>);

impl<A: Ord> Default for Findings<A> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}

impl<A: Ord> Findings<A> {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn get(&self, declaration: &str) -> Option<&Finding<A>> {
        self.0.get(declaration)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Finding<A>)> {
        self.0
            .iter()
            .map(|(declaration, finding)| (declaration.as_str(), finding))
    }

    #[must_use]
    pub fn is_new(&self, declaration: &str) -> bool {
        !self.0.contains_key(declaration)
    }

    pub fn record(
        &mut self,
        declaration: &str,
        sequence: u64,
        input: impl FnOnce() -> Result<Input<A>, Box<dyn Error>>,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(finding) = self.0.get_mut(declaration) {
            finding.actions = finding.actions.saturating_add(1);
            return Ok(());
        }
        self.0.insert(
            declaration.to_owned(),
            Finding {
                sequence,
                actions: 1,
                input: input()?,
            },
        );
        Ok(())
    }

    pub fn merge_new(&mut self, other: &Self)
    where
        A: Clone,
    {
        for (declaration, finding) in &other.0 {
            self.0
                .entry(declaration.clone())
                .or_insert_with(|| finding.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    struct Reach(u16);

    impl Stage for Reach {
        type State = u16;

        fn name(self) -> &'static str {
            "reach"
        }

        fn objective(self, state: &u16, _carried: Option<u16>) -> u16 {
            *state
        }

        fn done(self, state: &u16, _objective: u16) -> bool {
            *state >= self.0
        }
    }

    const PLAN: [Reach; 2] = [Reach(4), Reach(9)];

    #[derive(
        Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize,
    )]
    struct View {
        distance: u16,
        room: u8,
        speed: u8,
        inherited: u16,
    }

    struct Walk;

    impl Assertions for Walk {
        type View = View;
        type Stage = Reach;
        type Values = u8;
        type Identity = u8;
        type Lineage = Vec<u8>;

        const DECLARATIONS: &'static [Declaration<View, Reach, u8>] = &[
            Declaration::SometimesAll {
                name: "reach",
                plan: |_| &PLAN,
                state: |view| view.distance.max(view.inherited),
            },
            Declaration::SometimesEach {
                name: "room",
                values: |view| view.room,
            },
            Declaration::Value {
                name: "top speed",
                value: |view| u64::from(view.speed),
            },
            Declaration::Value {
                name: "distance",
                value: |view| u64::from(view.distance),
            },
            Declaration::Always {
                name: "the room is below 8",
                holds: |view| view.room < 8,
            },
            Declaration::Always {
                name: "the distance is at most 20",
                holds: |view| view.distance <= 20,
            },
            Declaration::SometimesEach {
                name: "speed",
                values: |view| view.speed,
            },
        ];

        const STRATEGY: Strategy = Strategy {
            stage: Some("reach"),
            spread: &["room", "speed"],
            prefer: &[&["top speed", "distance"]],
        };

        fn identity(view: &View) -> u8 {
            view.speed / 4
        }

        fn complete(view: View, parent: Option<(View, &Vec<u8>)>) -> View {
            View {
                inherited: parent.map_or(0, |(parent, _)| parent.distance.max(parent.inherited)),
                ..view
            }
        }

        fn record(lineage: &mut Vec<u8>, view: View) {
            lineage.push(view.room);
        }
    }

    fn key(distance: u16, room: u8, speed: u8) -> AssertionKey<Walk> {
        AssertionKey::new(View {
            distance,
            room,
            speed,
            inherited: 0,
        })
    }

    #[test]
    fn the_plan_is_the_progress_and_each_coverage_declaration_has_its_own_cells() {
        assert_eq!(key(2, 1, 0).progress(), (0, 2));
        assert_eq!(key(5, 1, 0).progress(), (1, 5));
        assert_eq!(key(12, 1, 0).progress(), (1, 0));
        assert_eq!(key(5, 3, 7).place(), (0, 3));
        assert_eq!(key(5, 3, 7).places().collect::<Vec<_>>(), [(0, 3), (1, 7)]);
        assert_eq!(key(5, 3, 7).identity(), 1);
        assert_eq!(AssertionKey::<Walk>::capacity(), MAX_ENTRIES_PER_KEY);
    }

    #[test]
    fn a_prefer_line_is_one_preference_ordered_by_its_values_in_turn() {
        assert_eq!(AssertionKey::<Walk>::preferences(), 1);
        assert_eq!(
            key(5, 3, 9).preference_cmp(0, key(5, 3, 2)),
            Ordering::Greater
        );
        assert_eq!(key(4, 3, 9).preference_cmp(0, key(5, 3, 9)), Ordering::Less);
        assert_eq!(
            key(5, 3, 9).preference_cmp(1, key(5, 3, 2)),
            Ordering::Equal
        );
    }

    struct Unplanned;

    impl Assertions for Unplanned {
        type View = View;
        type Stage = Reach;
        type Values = u8;
        type Identity = u8;
        type Lineage = Vec<u8>;

        const DECLARATIONS: &'static [Declaration<View, Reach, u8>] = Walk::DECLARATIONS;

        const STRATEGY: Strategy = Strategy {
            stage: None,
            spread: &["speed", "the room is below 8"],
            prefer: &[&["room"]],
        };

        fn identity(view: &View) -> u8 {
            Walk::identity(view)
        }

        fn complete(view: View, parent: Option<(View, &Vec<u8>)>) -> View {
            Walk::complete(view, parent)
        }

        fn record(lineage: &mut Vec<u8>, view: View) {
            Walk::record(lineage, view);
        }
    }

    #[test]
    fn the_strategy_decides_each_declaration_s_part_and_names_the_policy() {
        let unplanned = AssertionKey::<Unplanned>::new(key(5, 3, 7).view());
        assert_eq!(unplanned.progress(), (0, 0));
        assert_eq!(unplanned.places().collect::<Vec<_>>(), [(0, 7)]);
        assert_eq!(
            unplanned.preference_cmp(0, AssertionKey::new(key(5, 1, 7).view())),
            Ordering::Equal
        );
        assert_eq!(
            AssertionKey::<Unplanned>::unresolved(),
            ["the room is below 8", "room"]
        );
        assert!(AssertionKey::<Walk>::unresolved().is_empty());
        let walk = AssertionKey::<Walk>::policy_identifier();
        assert!(walk.starts_with("assertion_key_"));
        assert_eq!(walk.len(), "assertion_key_".len() + 16);
        assert_eq!(walk, AssertionKey::<Walk>::policy_identifier());
        assert_ne!(walk, AssertionKey::<Unplanned>::policy_identifier());
    }

    #[test]
    fn an_always_has_no_place_and_names_its_violations() {
        let violating = key(30, 9, 0);
        assert_eq!(
            violating.violations().collect::<Vec<_>>(),
            ["the room is below 8", "the distance is at most 20"]
        );
        assert_eq!(key(3, 2, 0).violations().count(), 0);
        assert_eq!(violating.place(), (0, 9));
        assert_eq!(Walk::DECLARATIONS[4].name(), "the room is below 8");
        assert_eq!(Walk::DECLARATIONS[4].form(), "always");
    }

    #[test]
    fn completion_and_lineage_go_through_the_workload() {
        let parent = key(6, 1, 0);
        let child = key(1, 2, 0).complete(Some((parent, &Vec::new())));
        assert_eq!(child.progress(), (1, 6));
        let mut lineage = Vec::new();
        AssertionKey::record(&mut lineage, child);
        assert_eq!(lineage, [2]);
        let json = serde_json::to_string(&child).expect("serialize");
        assert_eq!(json, serde_json::to_string(&child.view()).expect("view"));
        assert_eq!(
            serde_json::from_str::<AssertionKey<Walk>>(&json).expect("deserialize"),
            child
        );
    }

    #[test]
    fn a_finding_keeps_the_first_tape_and_counts_later_actions() {
        let mut findings = Findings::<u8>::default();
        assert!(findings.is_new("p"));
        findings
            .record("p", 3, || {
                Ok(Input {
                    actions: vec![1, 2],
                })
            })
            .expect("first");
        findings
            .record("p", 5, || Err("a known finding needs no tape".into()))
            .expect("second");
        let finding = findings.get("p").expect("finding");
        assert_eq!(
            (
                finding.sequence,
                finding.actions,
                finding.input.actions.clone()
            ),
            (3, 2, vec![1, 2])
        );
        assert!(!findings.is_new("p"));
        assert_eq!(findings.iter().count(), 1);
        let mut resumed = Findings::<u8>::default();
        resumed
            .record("q", 1, || Ok(Input { actions: vec![4] }))
            .expect("resumed");
        resumed.merge_new(&findings);
        resumed.merge_new(&findings);
        assert_eq!(resumed.iter().count(), 2);
        assert_eq!(resumed.get("p"), findings.get("p"));
        let json = serde_json::to_value(&findings).expect("serialize");
        assert_eq!(json["p"]["sequence"], 3);
    }
}
