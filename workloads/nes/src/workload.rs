// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, fs, io::Write, path::Path};

use machine::quicknes::QuickNesMachine;
use nes_games::workload::{Game, NesSnapshot, NesWorkload, RECORDS_FILE};
use serde::Serialize;

use crate::{
    eval::Request,
    film::{Endpointed, FilmMetadata, Filmable},
    search::{archive::Input, campaign::TargetExecution},
    target::{ExitKind, Target},
    witness::EndpointIdentity,
};

pub const STRATEGY_FILE: &str = "strategy.toml";
pub fn configured<G: Game>(
    workload: NesWorkload<G, QuickNesMachine>,
    request: &Request,
    out: &Path,
) -> Result<NesWorkload<G, QuickNesMachine>, Box<dyn Error>> {
    let workload = match &request.strategy {
        Some(path) => {
            let text = fs::read_to_string(path)?;
            fs::create_dir_all(out)?;
            fs::write(out.join(STRATEGY_FILE), &text)?;
            workload.with_strategy(&text)?
        }
        None => workload,
    };
    let workload = match request.choices {
        Some(choices) => workload.with_choice_policy(choices)?,
        None => workload,
    };
    if request.records {
        fs::create_dir_all(out)?;
        workload.with_records(&out.join(RECORDS_FILE), request.seed)
    } else {
        Ok(workload)
    }
}

impl<S: Clone + Serialize> EndpointIdentity for NesSnapshot<S> {
    fn endpoint_sha256(&self) -> Result<String, Box<dyn Error>> {
        self.endpoint_identity_sha256()
    }
}

impl<G: Game> Endpointed for NesWorkload<G, QuickNesMachine> {
    fn headless_endpoint(
        &self,
        input: &Input<machine::nes::ButtonChord>,
    ) -> Result<serde_json::Value, Box<dyn Error>> {
        let mut target = self.new_target()?;
        target.reset();
        for action in &input.actions {
            target.apply(action);
            if target.exit_kind() != ExitKind::Ok {
                return Err(format!(
                    "the recorded {} input crashed during headless replay",
                    G::NAME
                )
                .into());
            }
        }
        Ok(serde_json::to_value(target.observe().state)?)
    }

    fn input_frames(
        &self,
        input: &Input<machine::nes::ButtonChord>,
    ) -> Result<u64, Box<dyn Error>> {
        Ok(input
            .actions
            .iter()
            .map(|action| u64::from(action.bounded_hold_frames()))
            .sum())
    }
}

impl<G: Game> Filmable for NesWorkload<G, QuickNesMachine> {
    fn film(
        &self,
        input: &Input<machine::nes::ButtonChord>,
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<FilmMetadata, Box<dyn Error>> {
        let mut target = self.new_target()?;
        let (video, endpoint) = target.render_input(
            &input.actions,
            tail_frames,
            skip_frames,
            video_output,
            audio_output,
        )?;
        Ok(FilmMetadata {
            width: video.width,
            height: video.height,
            frames: video.frames,
            skipped_frames: video.skipped_frames,
            audio_sample_rate: video.audio_sample_rate,
            audio_channels: video.audio_channels,
            audio_frames: video.audio_frames,
            input_endpoint: serde_json::to_value(endpoint)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use machine::{Machine, quicknes::QuickNesMachine};
    use nes_games::{
        nes_backend::pack_quicknes_state,
        workload::{NesObservation, NesSnapshot, Satisfied},
    };
    use searcher::search::assertion::Accumulator;

    use crate::witness::EndpointIdentity;

    fn raw_state(machine: &mut QuickNesMachine) -> Vec<u8> {
        let held = machine.snapshot().expect("snapshot");
        machine.take_snapshot(held).expect("raw state")
    }

    fn snapshot(raw: &[u8], frame_count: u64, accumulator: Accumulator) -> NesSnapshot<u8> {
        NesSnapshot {
            emulator_state: pack_quicknes_state(raw).expect("pack"),
            observation: NesObservation {
                frame_count,
                state: 3,
                key: None,
                rejected: None,
                goal: false,
                satisfied: Satisfied(0),
                overran: false,
            },
            accumulator,
            failed: false,
        }
    }

    #[test]
    fn endpoint_identity_covers_the_game_state_and_not_the_execution_hash() {
        let mut machine = QuickNesMachine::loopback_for_tests(&[0]).expect("loopback core");
        let own = raw_state(&mut machine);
        let mut foreign = own.clone();
        for (index, byte) in foreign[112..176].iter_mut().enumerate() {
            *byte = b"0123456789abcdef"[(index * 7 + index / 5) % 16];
        }
        assert_ne!(own, foreign);
        machine.poke_wram(0x10, 7).expect("poke");
        let changed_ram = raw_state(&mut machine);
        assert_ne!(changed_ram, own);
        let mut other_core = own.clone();
        other_core[48] ^= 1;
        let staged: Accumulator = postcard::from_bytes(&[1, 2, 0]).expect("accumulator");
        let highest = |value: u8| -> Accumulator {
            postcard::from_bytes(&[1, 2, 1, value]).expect("accumulator")
        };

        let identity = |raw: &[u8], frames: u64, accumulator: Accumulator| {
            snapshot(raw, frames, accumulator)
                .endpoint_sha256()
                .expect("identity")
        };
        let base = identity(&own, 9, Accumulator::default());
        assert_eq!(base, identity(&foreign, 9, Accumulator::default()));
        assert_ne!(base, identity(&changed_ram, 9, Accumulator::default()));
        assert_ne!(base, identity(&own, 9, staged.clone()));
        assert_eq!(
            identity(&own, 9, highest(2)),
            identity(&foreign, 9, highest(2))
        );
        assert_ne!(identity(&own, 9, highest(2)), identity(&own, 9, highest(4)));
        assert_ne!(identity(&own, 9, highest(2)), identity(&own, 9, staged));
        assert_ne!(base, identity(&own, 10, Accumulator::default()));
        assert_ne!(base, identity(&other_core, 9, Accumulator::default()));
    }
}
