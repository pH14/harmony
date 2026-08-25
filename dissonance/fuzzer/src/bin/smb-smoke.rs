// SPDX-License-Identifier: AGPL-3.0-or-later

//! Real-ROM Phase 4b determinism smoke; never executed by unit tests.

use std::{collections::BTreeSet, env, error::Error, fs, path::PathBuf};

use fuzzer::{
    smb::target::{
        ButtonChord, SmbMilestones, SmbObservations, SmbTarget, smb_milestones_from_wram,
    },
    target::{Target, execute_actions},
};
use serde::Serialize;
use sha2::{Digest, Sha256};

const SMOKE_SEED: u64 = 0x5eed_d600;
const MINI_CAMPAIGN_EXECUTIONS: usize = 16;

type MiniCorpus = Vec<(u64, Vec<ButtonChord>)>;

#[derive(Debug, Serialize)]
struct SmokeReport {
    rom_path: PathBuf,
    rom_sha256: String,
    same_input_identical_ram_trace: bool,
    snapshot_cache_equivalent: bool,
    headless_ram_trace_equivalent: bool,
    same_seed_campaign_reproducible: bool,
    mini_campaign_corpus_size: usize,
    final_frame_count: u64,
    final_changed_indices: usize,
    genesis_selected_wram: Vec<(usize, u8)>,
    final_selected_wram: Vec<(usize, u8)>,
    calibration_sequences: Vec<(String, SmbMilestones)>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let output = PathBuf::from(
        env::args_os()
            .nth(1)
            .ok_or("usage: smb-smoke <output-directory>")?,
    );
    fs::create_dir_all(&output)?;
    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = fs::read(&rom_path)?;
    let rom_sha256 = format!("{:x}", Sha256::digest(&rom));

    let actions = smoke_actions();
    let mut first = SmbTarget::from_smb_rom_bytes(&rom)?;
    let mut second = SmbTarget::from_smb_rom_bytes(&rom)?;
    let selected_offsets = [
        0x006d, 0x0086, 0x00ce, 0x071a, 0x071c, 0x0746, 0x075c, 0x075f, 0x0770,
    ];
    let genesis_selected_wram = selected_offsets
        .iter()
        .map(|offset| (*offset, first.wram()[*offset]))
        .collect();
    let first_trace = execute_actions(&mut first, &actions);
    let second_trace = execute_actions(&mut second, &actions);
    let same_input_identical_ram_trace = first_trace == second_trace;
    let mut headless = SmbTarget::from_smb_rom_bytes_headless(&rom)?;
    let headless_trace = execute_actions(&mut headless, &actions);
    let headless_ram_trace_equivalent = first_trace == headless_trace;

    let split = actions.len() / 2;
    let mut cached = SmbTarget::from_smb_rom_bytes(&rom)?;
    execute_actions(&mut cached, &actions[..split]);
    let prefix = cached.snapshot().ok_or("failed to save prefix snapshot")?;
    for action in &actions[split..] {
        cached.apply(action);
    }
    let cached_result = cached.observe();
    cached.restore(&prefix)?;
    for action in &actions[split..] {
        cached.apply(action);
    }
    let restored_result = cached.observe();
    let mut uncached = SmbTarget::from_smb_rom_bytes(&rom)?;
    let uncached_result = execute_actions(&mut uncached, &actions)
        .into_iter()
        .last()
        .ok_or("uncached trace was empty")?;
    let snapshot_cache_equivalent =
        cached_result == restored_result && restored_result == uncached_result;

    let first_campaign = mini_campaign(&rom, SMOKE_SEED, MINI_CAMPAIGN_EXECUTIONS)?;
    let second_campaign = mini_campaign(&rom, SMOKE_SEED, MINI_CAMPAIGN_EXECUTIONS)?;
    let same_seed_campaign_reproducible = first_campaign == second_campaign;
    let final_observation = first_trace.last().cloned().unwrap_or(SmbObservations {
        frame_count: 0,
        wram: Vec::new(),
        decoded: Default::default(),
        milestones: Default::default(),
        changed_indices: Vec::new(),
        dead: false,
        log_line: String::new(),
    });
    let report = SmokeReport {
        rom_path,
        rom_sha256,
        same_input_identical_ram_trace,
        snapshot_cache_equivalent,
        headless_ram_trace_equivalent,
        same_seed_campaign_reproducible,
        mini_campaign_corpus_size: first_campaign.len(),
        final_frame_count: final_observation.frame_count,
        final_changed_indices: final_observation.changed_indices.len(),
        genesis_selected_wram,
        final_selected_wram: selected_offsets
            .iter()
            .map(|offset| (*offset, first.wram()[*offset]))
            .collect(),
        calibration_sequences: calibration_sequences(&rom)?,
    };
    write_ppm(&output.join("smb-smoke-final.ppm"), &first.frame_rgba())?;
    fs::write(
        output.join("smb-smoke-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !same_input_identical_ram_trace
        || !snapshot_cache_equivalent
        || !headless_ram_trace_equivalent
        || !same_seed_campaign_reproducible
    {
        return Err("M4 determinism smoke failed".into());
    }
    Ok(())
}

fn calibration_sequences(rom: &[u8]) -> Result<Vec<(String, SmbMilestones)>, Box<dyn Error>> {
    let sequences = [
        ("run_jump_60", vec![ButtonChord::new(0x83, 60)]),
        ("run_jump_120", vec![ButtonChord::new(0x83, 120)]),
        (
            "jump_then_run_jump",
            vec![ButtonChord::new(0x01, 48), ButtonChord::new(0x83, 60)],
        ),
        (
            "two_run_jumps",
            vec![ButtonChord::new(0x83, 60), ButtonChord::new(0x83, 60)],
        ),
        ("eight_run_jumps", vec![ButtonChord::new(0x83, 60); 8]),
    ];
    let mut results = Vec::new();
    for (name, actions) in sequences {
        let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
        let mut milestones = smb_milestones_from_wram(target.wram());
        for action in actions {
            target.apply(&action);
            let current = smb_milestones_from_wram(target.wram());
            milestones.max_1_1_scroll_bucket = milestones
                .max_1_1_scroll_bucket
                .max(current.max_1_1_scroll_bucket);
            milestones.reached_1_1_flag |= current.reached_1_1_flag;
            milestones.reached_1_2 |= current.reached_1_2;
            milestones.reached_onward |= current.reached_onward;
            if target.is_dead() {
                break;
            }
        }
        results.push((name.to_owned(), milestones));
    }
    Ok(results)
}

fn write_ppm(path: &std::path::Path, rgba: &[u8]) -> Result<(), Box<dyn Error>> {
    const WIDTH: usize = 256;
    const HEIGHT: usize = 240;
    if rgba.len() != WIDTH * HEIGHT * 4 {
        return Err("unexpected TetaNES RGBA frame length".into());
    }
    let mut ppm = format!("P6\n{WIDTH} {HEIGHT}\n255\n").into_bytes();
    ppm.reserve(WIDTH * HEIGHT * 3);
    for pixel in rgba.chunks_exact(4) {
        ppm.extend_from_slice(&pixel[..3]);
    }
    fs::write(path, ppm)?;
    Ok(())
}

fn smoke_actions() -> Vec<ButtonChord> {
    let mut actions = Vec::new();
    for step in 0..12 {
        let buttons = if step % 3 == 1 { 0x81 } else { 0x80 };
        actions.push(ButtonChord::new(buttons, 30));
    }
    actions
}

fn mini_campaign(rom: &[u8], seed: u64, executions: usize) -> Result<MiniCorpus, Box<dyn Error>> {
    let mut rng = seed;
    let mut seen = BTreeSet::new();
    let mut corpus = Vec::new();
    for _ in 0..executions {
        let length = usize::from(next_byte(&mut rng) % 12) + 1;
        let mut actions = Vec::with_capacity(length);
        for _ in 0..length {
            actions.push(ButtonChord::new(
                next_byte(&mut rng),
                (next_byte(&mut rng) % 60) + 1,
            ));
        }
        let mut target = SmbTarget::from_smb_rom_bytes(rom)?;
        execute_actions(&mut target, &actions);
        let fingerprint = target.fingerprint();
        if seen.insert(fingerprint) {
            corpus.push((fingerprint, actions));
        }
    }
    Ok(corpus)
}

fn next_byte(state: &mut u64) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    state.to_le_bytes()[0]
}
