// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt::Debug,
    sync::{Mutex, PoisonError},
};

use searcher::search::assertion::{
    Accumulator, Assertion, AssertionKey, Catalog, Compiled, Declarations, KeyView, Lines, Number,
    Observed, RECORD_SLOTS, Record, Strategy, Wide,
};
use serde::{Deserialize, Serialize};

use crate::target::FaultObservations;

pub const STRATEGY: &str = include_str!("faults.toml");

pub const DECLARATIONS: usize = 18;

const WIDE: i64 = (1 << 62) - 1;
const COUNT: i64 = u16::MAX as i64;
const CONTEXT_FLAGS: u16 = 5;
const MASK_NODES: u16 = 62;
const MASK_LANES: [&str; 2] = ["alive", "event.ready"];

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KeyRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub facts: BTreeMap<String, Vec<String>>,
}

impl KeyRequest {
    #[must_use]
    pub fn strategy(&self) -> &str {
        self.strategy.as_deref().unwrap_or(STRATEGY)
    }
}

fn wide(value: u64) -> i64 {
    i64::try_from(value).map_or(WIDE, |value| value.min(WIDE))
}

fn node_mask(nodes: u16) -> u64 {
    match nodes {
        0 => 0,
        nodes if nodes >= MASK_NODES => WIDE as u64,
        nodes => (1_u64 << nodes) - 1,
    }
}

fn reads(strategy: &str, name: &str) -> bool {
    let part = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.';
    strategy.match_indices(name).any(|(start, _)| {
        let before = strategy[..start].chars().next_back();
        let after = strategy[start + name.len()..].chars().next();
        !before.is_some_and(part) && !after.is_some_and(part)
    })
}

fn masks_fit(strategy: &str, nodes: u16) -> Result<(), Box<dyn Error>> {
    if nodes <= MASK_NODES {
        return Ok(());
    }
    match MASK_LANES.into_iter().find(|lane| reads(strategy, lane)) {
        Some(lane) => Err(format!(
            "the strategy reads the node mask lane {lane}, whose declared value holds at most {MASK_NODES} nodes, so it cannot key {nodes} nodes"
        )
        .into()),
        None => Ok(()),
    }
}

pub const UNOBSERVED: Observed = Observed {
    key: None,
    rejected: None,
    goal: false,
};

#[derive(Deserialize, Serialize)]
#[serde(remote = "Observed")]
pub(crate) struct ObservedDef {
    key: Option<AssertionKey>,
    rejected: Option<u8>,
    goal: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fact {
    name: &'static str,
    terms: &'static [&'static str],
}

static NAMES: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());
static TERMS: Mutex<BTreeSet<&'static [&'static str]>> = Mutex::new(BTreeSet::new());

fn intern(name: &str) -> &'static str {
    let mut names = NAMES.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(known) = names.get(name).copied() {
        return known;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(leaked);
    leaked
}

fn intern_terms(terms: Vec<&'static str>) -> &'static [&'static str] {
    let mut lists = TERMS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(known) = lists.get(terms.as_slice()).copied() {
        return known;
    }
    let leaked: &'static [&'static str] = Box::leak(terms.into_boxed_slice());
    lists.insert(leaked);
    leaked
}

fn built_in_names() -> BTreeSet<&'static str> {
    let mut declared = Vec::new();
    declare(&FaultObservations::default(), 1, &[], &mut declared);
    declared
        .iter()
        .map(|assertion| {
            let name = assertion.name();
            name.split_once('.').map_or(name, |(head, _)| head)
        })
        .collect()
}

fn facts(requested: &BTreeMap<String, Vec<String>>) -> Result<Vec<Fact>, Box<dyn Error>> {
    let built_in = built_in_names();
    let room = RECORD_SLOTS - 1 - DECLARATIONS;
    let signals: usize = requested.values().map(|terms| 1 + terms.len()).sum();
    if signals > room {
        return Err(format!(
            "the key table's facts declare {signals} values and a record holds {room} beside the {DECLARATIONS} built-in values"
        )
        .into());
    }
    requested
        .iter()
        .map(|(name, terms)| -> Result<Fact, Box<dyn Error>> {
            let mut letters = name.chars();
            let word = letters
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                && letters.all(|letter| letter.is_ascii_alphanumeric() || letter == '_');
            if !word {
                return Err(format!(
                    "fact `{name}` is not a name of ASCII letters, digits and underscores starting with a letter or underscore"
                )
                .into());
            }
            if built_in.contains(name.as_str()) {
                return Err(format!("fact `{name}` reuses a built-in value's name").into());
            }
            if terms.is_empty() || terms.len() > 64 {
                return Err(format!("fact `{name}` names {} terms, not 1 to 64", terms.len()).into());
            }
            let mut seen = BTreeSet::new();
            for term in terms {
                if term.is_empty() || term.contains('\'') || term.chars().any(char::is_control) {
                    return Err(format!(
                        "fact `{name}` names the term {term:?}, which a strategy cannot quote"
                    )
                    .into());
                }
                if !seen.insert(term.as_str()) {
                    return Err(format!("fact `{name}` names the term {term:?} twice").into());
                }
            }
            Ok(Fact {
                name: intern(name),
                terms: intern_terms(terms.iter().map(|term| intern(term)).collect()),
            })
        })
        .collect()
}

fn compile(
    strategy: &Strategy,
    nodes: u16,
    facts: &[Fact],
) -> Result<(Compiled, Catalog), Box<dyn Error>> {
    let mut declared = Vec::new();
    declare(&FaultObservations::default(), nodes, facts, &mut declared);
    let catalog = Catalog::from_assertions(declared)?;
    Ok((Compiled::new(strategy, &catalog)?, catalog))
}

fn node_limit(
    strategy: &Strategy,
    nodes: u16,
    facts: &[Fact],
    error: Box<dyn Error>,
) -> Box<dyn Error> {
    match (1..nodes)
        .rev()
        .find(|fewer| compile(strategy, *fewer, facts).is_ok())
    {
        Some(limit) => {
            format!("this strategy keys at most {limit} nodes, not {nodes}: {error}").into()
        }
        None => error,
    }
}

pub fn declare(
    observations: &FaultObservations,
    nodes: u16,
    facts: &[Fact],
    out: &mut impl Declarations,
) {
    let mask = node_mask(nodes);
    let set = observations.assertions.key();
    let value = |name: &'static str, value: i64, max: i64| Assertion::Value {
        name,
        value: Number::new(value.clamp(0, max), 0, max),
    };
    let counter = |name: &'static str, count: u64| value(name, wide(count), WIDE);
    let violated = observations.violations().len();
    let values = [
        value("sometimes.count", i64::from(set.count), COUNT),
        value("sometimes.set", wide((set.digest >> 66) as u64), WIDE),
        value(
            "always.violated",
            i64::try_from(violated).unwrap_or(COUNT),
            COUNT,
        ),
        value("alive", wide(observations.alive & mask), wide(mask)),
        value(
            "event.ready",
            wide(observations.event_ready & mask),
            wide(mask),
        ),
        value("edges", wide(observations.edge_digest >> 2), WIDE),
        counter("edges.crossed", observations.edge_crossings),
        counter("ticks", observations.ticks),
        counter("hooks.started", observations.hooks_started),
        counter("hooks.finished", observations.hooks_finished),
        counter("checks.started", observations.checks_started),
        counter("checks.finished", observations.checks_finished),
        counter("workload.started", observations.workload_started),
        counter("workload.finished", observations.workload_finished),
        counter("event.kill_fires", observations.event_kill_fires),
        counter("event.park_fires", observations.event_park_fires),
        counter("event.park_held", observations.event_park_held),
        counter("event.kill_armed", observations.event_kill_armed),
    ];
    for value in values {
        out.declare(value);
    }
    for fact in facts {
        let holding = fact
            .terms
            .iter()
            .enumerate()
            .filter(|(_, term)| {
                observations
                    .assertions
                    .0
                    .get(**term)
                    .is_some_and(|outcome| outcome.holds)
            })
            .fold(0_u64, |holding, (index, _)| holding | 1 << index);
        out.declare(Assertion::SometimesAll {
            name: fact.name,
            terms: fact.terms,
            holding,
        });
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ContextLanes {
    hooks_running: Option<usize>,
    alive: Option<usize>,
    event_ready: Option<usize>,
    kill_fires: Option<usize>,
    park_fires: Option<usize>,
    checks_finished: Option<usize>,
    workload_running: Option<usize>,
}

impl ContextLanes {
    fn find(strategy: &Strategy) -> Result<Self, Box<dyn Error>> {
        let lines = strategy.lines()?;
        let items = |lines: &Option<Lines>| -> Vec<String> {
            lines.as_ref().map_or_else(Vec::new, |lines| {
                lines
                    .items()
                    .into_iter()
                    .map(|item| item.trim().to_owned())
                    .collect()
            })
        };
        let spread = items(&lines.spread);
        let identity = items(&lines.identity);
        let lane = |items: &[String], name: &str| items.iter().position(|item| item == name);
        Ok(Self {
            hooks_running: lane(&spread, "hooks_running"),
            alive: lane(&identity, "alive"),
            event_ready: lane(&spread, "event.ready"),
            kill_fires: lane(&spread, "kill_fires"),
            park_fires: lane(&spread, "park_fires"),
            checks_finished: lane(&spread, "checks_finished"),
            workload_running: lane(&spread, "workload_running"),
        })
    }

    fn masks(&self) -> Vec<&'static str> {
        [("alive", self.alive), ("event.ready", self.event_ready)]
            .into_iter()
            .filter_map(|(name, lane)| lane.map(|_| name))
            .collect()
    }

    fn check(&self, nodes: u16) -> Result<(), Box<dyn Error>> {
        let masks = self.masks();
        let Some(count) = u16::try_from(masks.len()).ok().filter(|count| *count > 0) else {
            return Ok(());
        };
        let limit = (64 - CONTEXT_FLAGS) / count;
        if nodes > limit {
            return Err(format!(
                "the duration context packs the node mask lane {} and {CONTEXT_FLAGS} flags into 64 bits, so this strategy holds at most {limit} nodes, not {nodes}",
                masks.join(" and ")
            )
            .into());
        }
        Ok(())
    }
}

pub struct FaultKeys {
    compiled: Compiled,
    catalog: Catalog,
    nodes: u16,
    facts: Vec<Fact>,
    lanes: ContextLanes,
}

impl Debug for FaultKeys {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FaultKeys")
            .field("policy", &self.compiled.policy_identifier())
            .field("nodes", &self.nodes)
            .finish_non_exhaustive()
    }
}

impl FaultKeys {
    pub fn new(strategy: &str, nodes: u16) -> Result<Self, Box<dyn Error>> {
        Self::with_facts(strategy, &BTreeMap::new(), nodes)
    }

    pub fn requested(request: &KeyRequest, nodes: u16) -> Result<Self, Box<dyn Error>> {
        Self::with_facts(request.strategy(), &request.facts, nodes)
    }

    fn with_facts(
        strategy: &str,
        facts: &BTreeMap<String, Vec<String>>,
        nodes: u16,
    ) -> Result<Self, Box<dyn Error>> {
        let facts = self::facts(facts)?;
        let parsed = Strategy::parse(strategy)?;
        let lanes = ContextLanes::find(&parsed)?;
        lanes.check(nodes)?;
        masks_fit(strategy, nodes)?;
        if let Some(stage) = parsed
            .stage
            .iter()
            .find(|stage| stage.spread.is_some() || stage.identity.is_some())
        {
            return Err(format!(
                "stage `{}` gives its own `spread` or `identity`, but the duration context reads the shared lines; give them at the top level or under `[every_stage]`",
                stage.name.as_deref().unwrap_or(&stage.goal)
            )
            .into());
        }
        if parsed.lines()?.reject.is_some()
            || parsed.stage.iter().any(|stage| stage.reject.is_some())
        {
            return Err("the faults workload reads no reject line".into());
        }
        let (compiled, catalog) = compile(&parsed, nodes, &facts)
            .map_err(|error| node_limit(&parsed, nodes, &facts, error))?;
        Ok(Self {
            lanes,
            compiled,
            catalog,
            nodes,
            facts,
        })
    }

    #[must_use]
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    #[must_use]
    pub fn compiled(&self) -> &Compiled {
        &self.compiled
    }

    #[must_use]
    pub fn record(&self, observations: &FaultObservations) -> Record {
        let mut record = Record::default();
        declare(observations, self.nodes, &self.facts, &mut record);
        record
    }

    #[must_use]
    pub fn key(&self, observations: &FaultObservations) -> Option<AssertionKey> {
        self.compiled.key(&self.record(observations))
    }

    pub fn observe(
        &self,
        observations: &FaultObservations,
        accumulator: &mut Accumulator,
        action_end: bool,
    ) -> Observed {
        let mut record = self.record(observations);
        record.set_action_end(action_end);
        self.compiled.observe(&record, accumulator)
    }

    #[must_use]
    pub fn view(&self, key: AssertionKey) -> KeyView {
        self.compiled.view(key)
    }

    fn mask(&self) -> u64 {
        node_mask(self.nodes)
    }

    #[must_use]
    pub fn context(&self, parent: AssertionKey) -> AssertionKey {
        let view = self.compiled.view(parent);
        let lane = |values: &[i64], index: Option<usize>| {
            index
                .and_then(|index| values.get(index))
                .map_or(0, |value| u64::try_from(*value).unwrap_or(0))
        };
        let place = |index| lane(&view.place, index);
        let mask = self.mask();
        let width = |index: Option<usize>| index.map_or(0, |_| u32::from(self.nodes));
        let fields = [
            (u64::from(place(self.lanes.hooks_running) > 0), 1),
            (
                lane(&view.identity, self.lanes.alive) & mask,
                width(self.lanes.alive),
            ),
            (
                place(self.lanes.event_ready) & mask,
                width(self.lanes.event_ready),
            ),
            (u64::from(place(self.lanes.kill_fires) > 0), 1),
            (u64::from(place(self.lanes.park_fires) > 0), 1),
            (u64::from(place(self.lanes.checks_finished) > 0), 1),
            (u64::from(place(self.lanes.workload_running) > 0), 1),
        ];
        let packed = fields
            .into_iter()
            .fold(0_u64, |packed, (value, bits)| (packed << bits) | value);
        AssertionKey {
            place: Wide::new(packed),
            ..AssertionKey::default()
        }
    }

    #[must_use]
    pub fn context_event_ready(&self, context: AssertionKey) -> u64 {
        if self.lanes.event_ready.is_none() {
            return 0;
        }
        (context.place.get() >> 4) & self.mask()
    }
}

#[cfg(test)]
mod tests;
