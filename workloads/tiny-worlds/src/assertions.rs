// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::worlds::{State, World};
use crate::{Keep, backtrack, chain, delayed, fork, map, resource, route, trap};
use searcher::search::assertion::{Assertion, Catalog, Number, Record, Strategy};
use std::error::Error;

const MOST: usize = 48;

fn value(name: &'static str, value: Number) -> Assertion {
    Assertion::Value { name, value }
}

fn byte(name: &'static str, n: u8) -> Assertion {
    value(name, Number::byte(n))
}

fn word(name: &'static str, n: u16) -> Assertion {
    value(name, Number::word(n))
}

fn flag(name: &'static str, n: bool) -> Assertion {
    value(name, Number::flag(n))
}

fn choice(name: &'static str, n: u8, last: u8) -> Assertion {
    value(name, Number::new(i64::from(n), 0, i64::from(last)))
}

fn goal(reached: bool) -> Assertion {
    Assertion::Sometimes {
        name: "goal",
        condition: reached,
    }
}

fn resource_signals(s: resource::State) -> [Assertion; 3] {
    [
        byte("resource.place", s.place),
        byte("resource.charge", s.charge),
        byte("resource.health", s.health),
    ]
}

fn route_signals(s: route::State, ranked_upgrade: bool) -> [Assertion; 4] {
    [
        byte("route.position", s.position),
        byte("route.lane", s.lane),
        byte("route.phase", s.phase),
        flag("route.ranked_upgrade", ranked_upgrade),
    ]
}

fn placement(placement: delayed::Placement) -> u8 {
    match placement {
        delayed::Placement::Identity => 0,
        delayed::Placement::Place => 1,
        delayed::Placement::Engaged => 2,
        delayed::Placement::Tier => 3,
    }
}

fn delayed_signals(s: delayed::State, horizon: u8, placed: u8, sticky: bool) -> [Assertion; 7] {
    [
        byte("delayed.lane", s.lane),
        byte("delayed.progress", s.progress),
        byte("delayed.memory", s.memory),
        byte("delayed.ammo", s.ammo),
        byte("delayed.horizon", horizon),
        choice("delayed.placement", placed, 3),
        flag("delayed.sticky", sticky),
    ]
}

fn trap_signals(s: trap::State) -> [Assertion; 4] {
    [
        byte("trap.position", s.position),
        byte("trap.trap", s.trap),
        byte("trap.room", s.room),
        flag("trap.item", s.item),
    ]
}

struct Buffer {
    items: [Assertion; MOST],
    len: usize,
}

impl Buffer {
    fn new() -> Self {
        Self {
            items: [goal(false); MOST],
            len: 0,
        }
    }

    fn push(&mut self, items: &[Assertion]) {
        self.items[self.len..self.len + items.len()].copy_from_slice(items);
        self.len += items.len();
    }

    fn items(&self) -> &[Assertion] {
        &self.items[..self.len]
    }
}

fn stage_family(world: &World) -> Option<u8> {
    match world {
        World::Resource(_) => Some(0),
        World::Route(_) => Some(1),
        World::Delayed(_) => Some(2),
        World::Trap(_) => Some(3),
        _ => None,
    }
}

fn chain_signals(w: &chain::Config, s: chain::State, buffer: &mut Buffer) {
    let world = &w.stages[usize::from(s.stage)].world;
    let local = s.local.world_state();
    let last = u8::try_from(w.stages.len() - 1).unwrap_or(u8::MAX);
    buffer.push(&[
        choice("chain.stage", s.stage, last),
        byte("chain.charge", s.charge),
        choice("chain.family", stage_family(world).unwrap_or(0), 3),
        flag("chain.ranked", w.ranked),
        flag("chain.carry", w.carry_charge),
    ]);
    buffer.push(&resource_signals(match local {
        State::Resource(s) => s,
        _ => resource::State {
            place: 0,
            charge: 0,
            health: 0,
            goal: false,
        },
    }));
    buffer.push(&match (world, local) {
        (World::Route(c), State::Route(s)) => route_signals(s, c.ranked_upgrade),
        _ => route_signals(
            route::State {
                position: 0,
                lane: 0,
                phase: 0,
                goal: false,
            },
            false,
        ),
    });
    buffer.push(&match (world, local) {
        (World::Delayed(c), State::Delayed(s)) => {
            delayed_signals(s, c.horizon, placement(c.placement), c.sticky_credit)
        }
        _ => delayed_signals(
            delayed::State {
                lane: 0,
                progress: 0,
                memory: 0,
                ammo: 0,
                goal: false,
            },
            0,
            0,
            false,
        ),
    });
    buffer.push(&trap_signals(match local {
        State::Trap(s) => s,
        _ => trap::State {
            position: 0,
            trap: 0,
            room: 0,
            item: false,
            goal: false,
        },
    }));
}

fn backtrack_signals(w: &backtrack::Config, s: backtrack::State) -> [Assertion; 4] {
    let placed = match w.placement {
        backtrack::Placement::Tier => 0,
        backtrack::Placement::Identity => 1,
        backtrack::Placement::Preference => 2,
    };
    [
        byte("backtrack.position", s.position),
        byte("backtrack.items", s.items),
        flag("backtrack.scouted", s.scouted),
        choice("backtrack.placement", placed, 2),
    ]
}

fn map_signals(w: &map::Config, s: map::State) -> [Assertion; 18] {
    [
        byte("map.cell", s.cell),
        byte("map.arm", s.arm),
        byte("map.progress", s.progress),
        flag("map.item", s.item),
        flag("map.dead", s.dead),
        flag("map.at_goal", s.cell == w.layout().goal),
        byte("map.health", s.health),
        byte("map.stock", s.stock),
        word("map.found", s.found),
        byte("map.hits", s.hits),
        byte("map.shield", s.shield),
        byte("map.tail", s.tail),
        byte(
            "map.spot",
            if w.arena > 0 {
                w.spot(s.cell, s.tail)
            } else {
                0
            },
        ),
        byte("map.arena", w.arena),
        byte("map.items", w.items),
        flag("map.locked", w.locked),
        flag("map.hit_tier", w.hit_tier),
        flag("map.tail_slots", w.tail_slots),
    ]
}

fn fork_signals(s: fork::State) -> [Assertion; 3] {
    [
        choice("fork.route", s.route, 2),
        byte("fork.position", s.position),
        byte("fork.room", s.room),
    ]
}

fn signals(world: &World, state: State, broken: bool, buffer: &mut Buffer) {
    buffer.push(&[flag("broken", broken)]);
    match (world, state) {
        (World::Resource(_), State::Resource(s)) => {
            buffer.push(&[goal(s.goal)]);
            buffer.push(&resource_signals(s));
        }
        (World::Route(w), State::Route(s)) => {
            buffer.push(&[goal(w.goal(s))]);
            buffer.push(&route_signals(s, w.ranked_upgrade));
        }
        (World::Delayed(w), State::Delayed(s)) => {
            buffer.push(&[goal(s.goal)]);
            buffer.push(&delayed_signals(
                s,
                w.horizon,
                placement(w.placement),
                w.sticky_credit,
            ));
        }
        (World::Trap(_), State::Trap(s)) => {
            buffer.push(&[goal(s.goal)]);
            buffer.push(&trap_signals(s));
        }
        (World::Chain(w), State::Chain(s)) => {
            buffer.push(&[goal(w.goal(s))]);
            chain_signals(w, s, buffer);
        }
        (World::Backtrack(w), State::Backtrack(s)) => {
            buffer.push(&[goal(s.goal)]);
            buffer.push(&backtrack_signals(w, s));
        }
        (World::Map(w), State::Map(s)) => {
            buffer.push(&[goal(s.goal)]);
            buffer.push(&map_signals(w, s));
        }
        (World::Fork(_), State::Fork(s)) => {
            buffer.push(&[goal(s.goal)]);
            buffer.push(&fork_signals(s));
        }
        (World::Crossing(w), State::Crossing(s)) => buffer.push(&[
            goal(w.goal(s)),
            word("crossing.position", s.position),
            word("crossing.cells", w.cells),
        ]),
        (World::PassiveClock(w), State::PassiveClock(s)) => buffer.push(&[
            goal(w.goal(s)),
            word(
                "passive.phase",
                u16::try_from(w.phase(s)).unwrap_or(u16::MAX),
            ),
        ]),
        _ => {}
    }
}

fn family(world: &World) -> &'static str {
    match world {
        World::Route(_) => "route",
        World::Chain(_) => "chain",
        World::Resource(_) => "resource",
        World::Maze(_) => "maze",
        World::Actions(_) => "actions",
        World::Deadline(_) => "deadline",
        World::Delayed(_) => "delayed",
        World::DeadlineActions(_) => "deadline_actions",
        World::Trap(_) => "trap",
        World::Backtrack(_) => "backtrack",
        World::Map(_) => "map",
        World::Graph(_) => "graph",
        World::Crossing(_) => "crossing",
        World::PassiveClock(_) => "passive_clock",
        World::Targets(_) => "targets",
        World::Fork(_) => "fork",
        World::Picks(_) => "picks",
    }
}

fn lowered(world: &World) -> Result<(), Box<dyn Error>> {
    match world {
        World::Chain(w) => {
            if let Some(stage) = w
                .stages
                .iter()
                .find(|stage| stage_family(&stage.world).is_none())
            {
                return Err(format!(
                    "no tiny-world strategy lowers a {} chain stage",
                    family(&stage.world)
                )
                .into());
            }
            Ok(())
        }
        World::Resource(_)
        | World::Route(_)
        | World::Delayed(_)
        | World::Trap(_)
        | World::Backtrack(_)
        | World::Map(_)
        | World::Fork(_)
        | World::Crossing(_)
        | World::PassiveClock(_) => Ok(()),
        _ => Err(format!("no tiny-world strategy lowers the {} family", family(world)).into()),
    }
}

pub fn catalog(world: &World, broken: bool) -> Result<Catalog, Box<dyn Error>> {
    lowered(world)?;
    let mut buffer = Buffer::new();
    signals(world, world.initial(), broken, &mut buffer);
    Catalog::from_assertions(buffer.items().iter().copied())
}

#[must_use]
pub fn record(world: &World, state: State, broken: bool) -> Record {
    let mut buffer = Buffer::new();
    signals(world, state, broken, &mut buffer);
    Record::from_assertions(buffer.items().iter().copied())
}

pub fn shipped_strategy(world: &World, keep: Keep) -> Result<Strategy, Box<dyn Error>> {
    lowered(world)?;
    let text = match world {
        World::Resource(_) => include_str!("../strategies/resource.toml"),
        World::Route(_) => include_str!("../strategies/route.toml"),
        World::Delayed(_) => include_str!("../strategies/delayed.toml"),
        World::Trap(_) => include_str!("../strategies/trap.toml"),
        World::Chain(_) => include_str!("../strategies/chain.toml"),
        World::Backtrack(_) => include_str!("../strategies/backtrack.toml"),
        World::Map(_) => include_str!("../strategies/map.toml"),
        World::Fork(_) => include_str!("../strategies/fork.toml"),
        World::Crossing(_) => include_str!("../strategies/crossing.toml"),
        _ => include_str!("../strategies/passive_clock.toml"),
    };
    let mut strategy = Strategy::parse(text)?;
    if keep == Keep::CapacityTwo {
        strategy.capacity = Some(2);
        if let Some(prefer) = &mut strategy.prefer {
            prefer.truncate(1);
        }
    }
    Ok(strategy)
}

pub fn checked_record(
    catalog: &Catalog,
    world: &World,
    state: State,
    broken: bool,
) -> Result<Record, Box<dyn Error>> {
    let mut buffer = Buffer::new();
    signals(world, state, broken, &mut buffer);
    catalog.checked_record(buffer.items().iter().copied())
}
