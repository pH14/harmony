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
    if request.records {
        fs::create_dir_all(out)?;
        workload.with_records(&out.join(RECORDS_FILE), request.seed)
    } else {
        Ok(workload)
    }
}

impl<S: Serialize> EndpointIdentity for NesSnapshot<S> {}

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
