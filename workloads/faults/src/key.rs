// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, fmt::Debug};

use searcher::search::{
    archive::{ArchiveKey, KeyShape},
    assertion::{
        Assertion, AssertionKey, Catalog, Compiled, KeyView, Lines, Number, Record, Strategy, Wide,
    },
};
use serde::{Deserialize, Serialize};

use crate::{
    archive::{FaultArchiveKey, KEY_POLICY_IDENTIFIER, archive_key},
    target::FaultObservations,
};

pub const STRATEGY: &str = include_str!("faults.toml");

pub const DECLARATIONS: usize = 18;

const WIDE: i64 = (1 << 62) - 1;
const COUNT: i64 = u16::MAX as i64;
const CONTEXT_NODES: u16 = 29;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KeyRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hand: bool,
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
        nodes if nodes >= 62 => WIDE as u64,
        nodes => (1_u64 << nodes) - 1,
    }
}

#[must_use]
pub fn declare(observations: &FaultObservations, nodes: u16) -> [Assertion; DECLARATIONS] {
    let mask = node_mask(nodes);
    let set = observations.assertions.key();
    let value = |name: &'static str, value: i64, max: i64| Assertion::Value {
        name,
        value: Number::new(value.clamp(0, max), 0, max),
    };
    let counter = |name: &'static str, count: u64| value(name, wide(count), WIDE);
    let violated = observations.violations().len();
    [
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
    ]
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
}

pub struct FaultKeys {
    compiled: Compiled,
    catalog: Catalog,
    nodes: u16,
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
        if nodes > CONTEXT_NODES {
            return Err(format!(
                "the duration context packs two node masks into 64 bits, so a strategy key holds at most {CONTEXT_NODES} nodes"
            )
            .into());
        }
        let catalog = Catalog::from_assertions(declare(&FaultObservations::default(), nodes))?;
        let parsed = Strategy::parse(strategy)?;
        if !parsed.stage.is_empty() {
            return Err("the faults strategy gives a tier, not stages".into());
        }
        if parsed.lines()?.reject.is_some() {
            return Err("the faults workload reads no reject line".into());
        }
        let compiled = Compiled::new(&parsed, &catalog)?;
        if !compiled.is_pure() {
            return Err(
                "the faults workload keeps no accumulator, so its strategy holds no aggregates"
                    .into(),
            );
        }
        Ok(Self {
            lanes: ContextLanes::find(&parsed)?,
            compiled,
            catalog,
            nodes,
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
        Record::from_assertions(declare(observations, self.nodes))
    }

    #[must_use]
    pub fn key(&self, observations: &FaultObservations) -> Option<AssertionKey> {
        self.compiled.key(&self.record(observations))
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
        let width = u32::from(self.nodes);
        let fields = [
            (u64::from(place(self.lanes.hooks_running) > 0), 1),
            (lane(&view.identity, self.lanes.alive) & mask, width),
            (place(self.lanes.event_ready) & mask, width),
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
        (context.place.get() >> 4) & self.mask()
    }
}

pub trait FaultKey: ArchiveKey + Debug + Eq + Send + Sync + 'static {
    fn of(keys: &FaultKeys, observations: &FaultObservations) -> Option<Self>;
    fn duration_context(keys: &FaultKeys, parent: Self) -> Self;
    fn event_ready(keys: &FaultKeys, context: Self) -> u64;
    fn policy(keys: &FaultKeys) -> String;
    fn shape(keys: &FaultKeys) -> KeyShape;
    fn hand(self) -> Option<FaultArchiveKey>;
}

impl FaultKey for FaultArchiveKey {
    fn of(_keys: &FaultKeys, observations: &FaultObservations) -> Option<Self> {
        Some(archive_key(observations))
    }

    fn duration_context(_keys: &FaultKeys, parent: Self) -> Self {
        Self {
            alive: parent.alive,
            event_ready: parent.event_ready,
            hooks_running: u64::from(parent.hooks_running > 0),
            event_kill_fires: u64::from(parent.event_kill_fires > 0),
            event_park_fires: u64::from(parent.event_park_fires > 0),
            workload_running: parent.workload_running,
            checks_finished: u64::from(parent.checks_finished > 0),
            ..Self::default()
        }
    }

    fn event_ready(_keys: &FaultKeys, context: Self) -> u64 {
        context.event_ready
    }

    fn policy(_keys: &FaultKeys) -> String {
        KEY_POLICY_IDENTIFIER.to_owned()
    }

    fn shape(_keys: &FaultKeys) -> KeyShape {
        KeyShape::of::<Self>()
    }

    fn hand(self) -> Option<FaultArchiveKey> {
        Some(self)
    }
}

impl FaultKey for AssertionKey {
    fn of(keys: &FaultKeys, observations: &FaultObservations) -> Option<Self> {
        keys.key(observations)
    }

    fn duration_context(keys: &FaultKeys, parent: Self) -> Self {
        keys.context(parent)
    }

    fn event_ready(keys: &FaultKeys, context: Self) -> u64 {
        keys.context_event_ready(context)
    }

    fn policy(keys: &FaultKeys) -> String {
        keys.compiled.policy_identifier().to_owned()
    }

    fn shape(keys: &FaultKeys) -> KeyShape {
        keys.compiled.shape()
    }

    fn hand(self) -> Option<FaultArchiveKey> {
        None
    }
}

#[cfg(test)]
mod tests;
