// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io::Write};

use machine::quicknes::QuickNesMachine;

pub use nes_games::ice_climbers::campaign::*;

use crate::{
    ice_climbers::target::IceClimbersInput,
    search::campaign::TargetExecution,
    target::{ExitKind, Target},
};

pub type IceClimbersGame = nes_games::ice_climbers::campaign::IceClimbersGame<QuickNesMachine>;

impl crate::film::Filmable for IceClimbersGame {
    fn film(
        &self,
        input: &IceClimbersInput,
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<crate::film::FilmMetadata, Box<dyn Error>> {
        let mut target = self
            .new_target()
            .map_err(|error| -> Box<dyn Error> { error.into() })?;
        let video =
            target.render_input(input, tail_frames, skip_frames, video_output, audio_output)?;
        Ok(crate::film::FilmMetadata {
            width: video.width,
            height: video.height,
            frames: video.frames,
            skipped_frames: video.skipped_frames,
            audio_sample_rate: video.audio_sample_rate,
            audio_channels: video.audio_channels,
            audio_frames: video.audio_frames,
            input_endpoint: serde_json::to_value(video.input_endpoint)?,
        })
    }
}

impl crate::film::Endpointed for IceClimbersGame {
    fn headless_endpoint(
        &self,
        input: &IceClimbersInput,
    ) -> Result<serde_json::Value, Box<dyn Error>> {
        let mut target = self
            .new_target()
            .map_err(|error| -> Box<dyn Error> { error.into() })?;
        target.reset();
        for action in &input.actions {
            target.apply(action);
            if target.exit_kind() != ExitKind::Ok {
                return Err("the recorded IceClimbers input crashed during headless replay".into());
            }
        }
        Ok(serde_json::to_value(target.observe().decoded)?)
    }

    fn input_frames(&self, input: &IceClimbersInput) -> Result<u64, Box<dyn Error>> {
        Ok(input
            .actions
            .iter()
            .map(|action| u64::from(action.bounded_hold_frames()))
            .sum())
    }
}
