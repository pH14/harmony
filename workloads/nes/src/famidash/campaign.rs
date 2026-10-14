// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io::Write};

use machine::quicknes::QuickNesMachine;

pub use nes_games::famidash::campaign::{
    CAMPAIGN_STREAM_FORMAT, FamidashCampaignCheckpoint, FamidashCampaignEvidence,
    FamidashCampaignModeReport, FamidashCampaignRun, FamidashCampaignStreamHeader,
    FamidashSnapshotCheckpoint, FamidashTargets, SNAPSHOT_CHECKPOINT_FORMAT,
};

use crate::{
    famidash::target::FamidashInput,
    search::campaign::{CampaignOrigin, TargetExecution},
    target::{ExitKind, Target},
};

pub type FamidashGame = nes_games::famidash::campaign::FamidashGame<QuickNesMachine>;
pub type FamidashCampaignOrigin = CampaignOrigin<FamidashGame>;

impl crate::film::Filmable for FamidashGame {
    fn film(
        &self,
        input: &FamidashInput,
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

impl crate::film::Endpointed for FamidashGame {
    fn headless_endpoint(
        &self,
        input: &FamidashInput,
    ) -> Result<serde_json::Value, Box<dyn Error>> {
        let mut target = self
            .new_target()
            .map_err(|error| -> Box<dyn Error> { error.into() })?;
        target.reset();
        for action in &input.actions {
            target.apply(action);
            if target.exit_kind() != ExitKind::Ok {
                return Err("the recorded Famidash input crashed during headless replay".into());
            }
        }
        Ok(serde_json::to_value(target.observe().decoded)?)
    }

    fn input_frames(&self, input: &FamidashInput) -> Result<u64, Box<dyn Error>> {
        Ok(input
            .actions
            .iter()
            .map(|action| u64::from(action.bounded_hold_frames()))
            .sum())
    }
}
