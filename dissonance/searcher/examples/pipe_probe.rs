// SPDX-License-Identifier: AGPL-3.0-or-later

//! Scratch diagnostic: measure how many kept states at the deepest band die
//! on a short neutral hold.

use std::{env, error::Error, fs};

use searcher::{
    search::campaign::Game,
    smb::archive::SmbArchiveReport,
    smb::{campaign::SmbGame, target::ButtonChord},
    target::Target,
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let report_path = args.next().ok_or("usage: pipe_probe <report>")?;
    let rom = fs::read(env::var("HARMONY_SMB_ROM")?)?;
    let game = SmbGame::from_environment(&rom)?;
    let mut target = game
        .new_target()
        .map_err(|error| -> Box<dyn Error> { error.into() })?;
    let report: SmbArchiveReport = serde_json::from_slice(&fs::read(&report_path)?)?;
    let mut candidates = report
        .entries
        .iter()
        .filter(|entry| {
            let k = &entry.key;
            k.world == 1 && k.level == 1 && k.progress == 176
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|entry| entry.id);
    // Newest states dominate the recency window; sample those.
    let sample = candidates
        .iter()
        .rev()
        .step_by(37)
        .take(40)
        .collect::<Vec<_>>();
    let mut dead = 0_u32;
    let mut alive = 0_u32;
    for entry in &sample {
        target.reset();
        for action in &entry.input.actions {
            target.apply(action);
        }
        target.apply(&ButtonChord::new(0, 30));
        if target.is_dead() {
            dead += 1;
        } else {
            alive += 1;
        }
    }
    println!(
        "sampled {}: dead-in-30-frames {} alive {}",
        sample.len(),
        dead,
        alive
    );
    Ok(())
}
