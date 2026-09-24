// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{Key, actions, deadline, deadline_actions, delayed, maze, resource};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "family",
    content = "parameters",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum World {
    Resource(resource::Config),
    Maze(maze::Config),
    Actions(actions::Config),
    Deadline(deadline::Config),
    Delayed(delayed::Config),
    DeadlineActions(deadline_actions::Config),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum State {
    Resource(resource::State),
    Maze(maze::State),
    Actions(actions::State),
    Deadline(deadline::State),
    Delayed(delayed::State),
    DeadlineActions(deadline_actions::State),
}

impl World {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Resource(w) => w.validate(),
            Self::Maze(w) => w.validate(),
            Self::Actions(w) => w.validate(),
            Self::Deadline(w) => w.validate(),
            Self::Delayed(w) => w.validate(),
            Self::DeadlineActions(w) => w.validate(),
        }
    }
    pub fn valid_state(&self, state: State) -> bool {
        self.validate().is_ok()
            && match (self, state) {
                (Self::Resource(w), State::Resource(s)) => {
                    w.state_is_bounded(s) && (!s.goal || w.goal(s))
                }
                (Self::Maze(w), State::Maze(s)) => w.state_is_bounded(s) && (!s.goal || w.goal(s)),
                (Self::Actions(w), State::Actions(s)) => {
                    w.state_is_bounded(s) && (!s.goal || w.goal(s))
                }
                (Self::Deadline(w), State::Deadline(s)) => w.state_is_bounded(s),
                (Self::Delayed(w), State::Delayed(s)) => w.state_is_bounded(s),
                (Self::DeadlineActions(w), State::DeadlineActions(s)) => w.state_is_bounded(s),
                _ => false,
            }
    }
    pub fn initial(&self) -> State {
        match self {
            Self::Resource(w) => State::Resource(w.initial()),
            Self::Maze(w) => State::Maze(w.initial()),
            Self::Actions(w) => State::Actions(w.initial()),
            Self::Deadline(w) => State::Deadline(w.initial()),
            Self::Delayed(w) => State::Delayed(w.initial()),
            Self::DeadlineActions(w) => State::DeadlineActions(w.initial()),
        }
    }
    pub fn step(&self, state: State, action: u8) -> State {
        match (self, state) {
            (Self::Resource(w), State::Resource(s)) => State::Resource(w.step(s, action)),
            (Self::Maze(w), State::Maze(s)) => State::Maze(w.step(s, action)),
            (Self::Actions(w), State::Actions(s)) => State::Actions(w.step(s, action)),
            (Self::Deadline(w), State::Deadline(s)) => State::Deadline(w.step(s, action)),
            (Self::Delayed(w), State::Delayed(s)) => State::Delayed(w.step(s, action)),
            (Self::DeadlineActions(w), State::DeadlineActions(s)) => {
                State::DeadlineActions(w.step(s, action))
            }
            _ => panic!("world and state family mismatch"),
        }
    }
    pub fn goal(&self, state: State) -> bool {
        match (self, state) {
            (Self::Resource(w), State::Resource(s)) => w.goal(s),
            (Self::Maze(w), State::Maze(s)) => w.goal(s),
            (Self::Actions(w), State::Actions(s)) => w.goal(s),
            (Self::Deadline(w), State::Deadline(s)) => w.goal(s),
            (Self::Delayed(w), State::Delayed(s)) => w.goal(s),
            (Self::DeadlineActions(w), State::DeadlineActions(s)) => w.goal(s),
            _ => false,
        }
    }
    pub fn reachable(&self) -> Result<bool, String> {
        match self {
            Self::Resource(w) => w.reachable(),
            Self::Maze(w) => w.reachable(),
            Self::Actions(w) => w.reachable(),
            Self::Deadline(w) => w.reachable(),
            Self::Delayed(w) => w.reachable(),
            Self::DeadlineActions(w) => w.reachable(),
        }
    }
    pub fn key(&self, state: State, broken: bool) -> Key {
        match (self, state) {
            (Self::Resource(_), State::Resource(s)) => Key {
                place: u16::from(s.place),
                context: 0,
                charge: if broken { 0 } else { s.charge },
                health: s.health,
                goal: s.goal,
            },
            (Self::Maze(w), State::Maze(s)) => w.key(s, broken),
            (Self::Actions(w), State::Actions(s)) => w.key(s, broken),
            (Self::Deadline(w), State::Deadline(s)) => w.key(s, broken),
            (Self::Delayed(w), State::Delayed(s)) => w.key(s, broken),
            (Self::DeadlineActions(w), State::DeadlineActions(s)) => w.key(s, broken),
            _ => panic!("world and state family mismatch"),
        }
    }
    pub fn changes_actions(&self) -> bool {
        matches!(self, Self::Actions(_) | Self::DeadlineActions(_))
    }
    pub fn sample_action(&self, action: u8, broken: bool) -> u8 {
        if self.changes_actions() && broken {
            0
        } else {
            action
        }
    }
    pub fn mixture(&self) -> searcher::search::draw::DrawMixture {
        if self.changes_actions() {
            searcher::search::draw::DrawMixture::BiasedHalf
        } else {
            searcher::search::draw::DrawMixture::AlphabetOnly
        }
    }
}
