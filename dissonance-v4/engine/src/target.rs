// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Identity,
    Higher,
    Lower,
}

impl Kind {
    pub fn is_identity(self) -> bool {
        self == Kind::Identity
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Action {
    pub input: u32,
    pub arg: u64,
    pub hold: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Vocabulary {
    pub inputs: u32,
    pub holds: bool,
    pub base_steps: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub action: usize,
    pub step: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    Nothing,
    Capture,
    Stop,
}

pub trait Probe {
    fn observe(
        &mut self,
        at: Position,
        work: u64,
        scalars: &[u64],
        array: &[u8],
        milestone: bool,
    ) -> Want;
    fn keep(&mut self, state: Vec<u8>);
}

pub struct Observation {
    pub scalars: Vec<u64>,
    pub array: Vec<u8>,
    pub milestone: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobId {
    pub batch: u64,
    pub index: u64,
}

pub trait Worker: Send {
    fn observe_start(&mut self, start: &[u8]) -> Result<Observation, String>;
    fn run(
        &mut self,
        job: JobId,
        start: &[u8],
        actions: &[Action],
        probe: &mut dyn Probe,
    ) -> Result<u64, String>;
}
