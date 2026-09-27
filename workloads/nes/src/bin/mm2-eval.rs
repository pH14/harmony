// SPDX-License-Identifier: AGPL-3.0-or-later

use nes_workload::{
    eval::{Result, evaluate, run_cli},
    mm2::{
        campaign::{Mm2CampaignRun, Mm2Game},
        target::{Mm2Input, Mm2Stage},
    },
};

fn main() -> Result<()> {
    run_cli(|request, rom, out, started| {
        if request.game != "mm2"
            || request.level.is_some()
            || request.ai.is_some()
            || request.metroid_terminal.is_some()
            || (request.whole_game && request.stage.is_some())
        {
            return Err("mm2-eval accepts only MM2 stage or whole_game requests".into());
        }
        let mut game = if request.whole_game {
            Mm2Game::new_whole_game(&rom, &request.core, &request.core_sha256)
        } else {
            Mm2Game::new_at_stage(
                &rom,
                &request.core,
                &request.core_sha256,
                Mm2Stage::from_number(request.stage.unwrap_or(0))?,
            )
        };
        if let Some(path) = &request.root_input {
            let input: Mm2Input = serde_json::from_slice(&std::fs::read(path)?)?;
            game = game.with_root_input(input)?;
        }
        evaluate(
            game.with_milestone_input_dir(out.join("milestone-inputs")),
            Mm2CampaignRun,
            &request,
            &out,
            started,
        )
    })
}
