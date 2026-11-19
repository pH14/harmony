// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use machine::nes::WRAM_SIZE;
use serde::{Serialize, de::DeserializeOwned};

use super::{ActionObservation, Game, NesRun, NesSnapshot, NesWorkload};
use crate::{
    arcade::{ArcadeGame, Machines},
    nes_backend::{NesBackend, NesMemory},
    search::draw::DrawMixture,
};

pub trait Arcade: Game + Default {
    type View: Serialize + DeserializeOwned + Copy + Send + 'static;
    const MEMORY_BYTES: usize = WRAM_SIZE;

    fn mixture() -> DrawMixture;
    fn view(state: &Self::State) -> Self::View;
    fn motion(state: &Self::State) -> [u16; 4];
    fn cleared(state: &Self::State) -> u8;

    fn at_level(_level: u8) -> Self {
        Self::default()
    }
}

fn decode<G: Arcade>(memory: &[u8]) -> Result<G::State, Box<dyn Error>> {
    if memory.len() < WRAM_SIZE {
        return Err(format!(
            "{} memory has {} bytes, not {WRAM_SIZE}",
            G::NAME,
            memory.len()
        )
        .into());
    }
    Ok(G::default().decode(None, None, memory))
}

impl<G: Arcade, M: NesBackend<Vec<u8>> + NesMemory + 'static> ArcadeGame for NesWorkload<G, M> {
    type Machine = M;
    type View = G::View;
    const MEMORY_BYTES: usize = G::MEMORY_BYTES;

    fn arcade(rom: &[u8], identity: &str, identity_sha256: &str, machines: Machines<M>) -> Self {
        Self::with_machines(
            G::default(),
            G::STRATEGY,
            rom,
            identity,
            identity_sha256,
            machines,
        )
        .expect("the built-in strategy compiles against the game's catalog")
    }

    fn arcade_from(
        rom: &[u8],
        identity: &str,
        identity_sha256: &str,
        machines: Machines<M>,
        level: u8,
    ) -> Self {
        Self::with_machines(
            G::at_level(level),
            G::STRATEGY,
            rom,
            identity,
            identity_sha256,
            machines,
        )
        .expect("the built-in strategy compiles against the game's catalog")
    }

    fn run() -> NesRun {
        NesRun
    }

    fn mixture() -> DrawMixture {
        G::mixture()
    }

    fn motion(wram: &[u8; WRAM_SIZE]) -> [u16; 4] {
        G::motion(&G::default().decode(None, None, wram))
    }

    fn memory_cleared(memory: &[u8]) -> u8 {
        decode::<G>(memory).map_or(0, |state| G::cleared(&state))
    }

    fn memory_view(memory: &[u8]) -> Result<G::View, Box<dyn Error>> {
        Ok(G::view(&decode::<G>(memory)?))
    }

    fn view(observation: &ActionObservation<G::State>) -> G::View {
        G::view(&observation.observation.state)
    }

    fn frame_count(observation: &ActionObservation<G::State>) -> u64 {
        observation.observation.frame_count
    }

    fn cleared(observation: &ActionObservation<G::State>) -> u8 {
        G::cleared(&observation.observation.state)
    }

    fn emulator_state(snapshot: &NesSnapshot<G::State>) -> &[u8] {
        &snapshot.emulator_state
    }

    fn snapshot_frame_count(snapshot: &NesSnapshot<G::State>) -> u64 {
        snapshot.observation.frame_count
    }
}
