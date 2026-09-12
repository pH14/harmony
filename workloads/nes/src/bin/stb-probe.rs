// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use machine::nes::ButtonChord;
use machine::{Machine, StopConditions, nes, quicknes::QuickNesMachine};
use nes_workload::stb::target::{StbAi, StbInput, StbTarget, decode_state, setup_tape_with_ai};
use nes_workload::target::Target;
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let rom = PathBuf::from(
        args.next()
            .or_else(|| env::var_os("HARMONY_STB_ROM"))
            .ok_or("usage: stb-probe [ROM] [buttons:hold,...]")?,
    );
    let schedule = args
        .next()
        .map(|value| value.into_string().map_err(|_| "schedule is not UTF-8"))
        .transpose()?
        .unwrap_or_default();
    let ai = env::var("HARMONY_STB_AI")
        .unwrap_or_else(|_| "hard".to_owned())
        .parse::<StbAi>()?;
    let core = PathBuf::from(
        env::var_os("HARMONY_QUICKNES_CORE").ok_or("HARMONY_QUICKNES_CORE must name QuickNES")?,
    );
    let rom_bytes = fs::read(&rom)?;
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core)?));
    if env::var_os("HARMONY_STB_TRACE_SETUP").is_some() {
        let mut machine = QuickNesMachine::from_rom_bytes(&rom_bytes, &core, &core_sha256)?;
        let mut current = machine.snapshot()?;
        let tape = setup_tape_with_ai(ai);
        let mut prior = decode_state(&machine.read_wram()?)?;
        println!("setup[0] {}", serde_json::to_string(&prior)?);
        for (index, action) in tape.iter().enumerate() {
            machine.branch(current, &nes::reproducer(std::slice::from_ref(action)))?;
            machine.run(StopConditions::default(), None)?;
            let wram = machine.read_wram()?;
            let state = decode_state(&wram)?;
            if state != prior || action.buttons != 0 {
                println!(
                    "setup[{index}] chord={} ctrl={:?} {}",
                    serde_json::to_string(action)?,
                    &wram[0xd0..=0xd4],
                    serde_json::to_string(&state)?
                );
                prior = state;
            }
            let next = machine.snapshot()?;
            machine.drop_snapshot(current)?;
            current = next;
        }
        machine.drop_snapshot(current)?;
    }
    let mut target =
        StbTarget::from_rom_bytes_headless_with_ai(&rom_bytes, &core, &core_sha256, ai)?;
    println!(
        "genesis {}",
        serde_json::to_string(&target.mechanical_state())?
    );

    if env::var_os("HARMONY_STB_CORRECTNESS").is_some() {
        run_correctness_probes(&rom_bytes, &core, &core_sha256, &mut target, ai)?;
        return Ok(());
    }

    if schedule.is_empty() {
        return Ok(());
    }
    let actions = schedule
        .split(',')
        .map(parse_action)
        .collect::<Result<Vec<_>, _>>()?;
    let input = StbInput { actions };
    for (index, action) in input.actions.iter().enumerate() {
        let restore_checkpoint = if env::var_os("HARMONY_STB_RESTORE_FINAL").is_some()
            && index + 1 == input.actions.len()
        {
            Some(
                target
                    .snapshot()
                    .ok_or("could not snapshot before final STB transition")?,
            )
        } else {
            None
        };
        target.apply(action);
        let observations = target.last_action_observations().to_vec();
        println!(
            "action={} chord={} observations={}",
            index,
            serde_json::to_string(action)?,
            serde_json::to_string(&observations)?
        );
        if let Some(checkpoint) = restore_checkpoint {
            let first_observation = target.observe();
            let first_action_observations = observations.clone();
            target.restore(&checkpoint)?;
            target.apply(action);
            if target.observe() != first_observation
                || target.last_action_observations() != first_action_observations
            {
                return Err("STB final transition changed after snapshot restore".into());
            }
            println!(
                "schedule_restore {}",
                serde_json::to_string(&serde_json::json!({
                    "action": index,
                    "terminal": target.is_match_over(),
                    "observation_exact": true,
                    "action_observations_exact": true,
                    "endpoint": target.observe(),
                }))?
            );
        }
        if target.is_match_over() || target.exit_kind() != nes_workload::target::ExitKind::Ok {
            break;
        }
    }
    println!(
        "endpoint {}",
        serde_json::to_string(&target.mechanical_state())?
    );
    Ok(())
}

fn run_correctness_probes(
    rom: &[u8],
    core: &std::path::Path,
    core_sha256: &str,
    target: &mut StbTarget,
    ai: StbAi,
) -> Result<(), Box<dyn Error>> {
    let genesis = target
        .snapshot()
        .ok_or("could not snapshot the STB gameplay genesis")?;
    let controls = [
        ("noop", 0x00),
        ("a", 0x01),
        ("b", 0x02),
        ("up", 0x10),
        ("left", 0x40),
        ("right", 0x80),
    ];
    for (name, buttons) in controls {
        target.restore(&genesis)?;
        let before = target.mechanical_state();
        target.apply(&ButtonChord::new(buttons, 6));
        println!(
            "control {}",
            serde_json::to_string(&serde_json::json!({
                "name": name,
                "buttons": format!("0x{buttons:02x}"),
                "hold": 6,
                "before": before,
                "after": target.mechanical_state(),
                "observations": target.last_action_observations(),
            }))?
        );
    }

    target.restore(&genesis)?;
    target.apply(&ButtonChord::new(0x01, 6));
    let branch = target.snapshot().ok_or("could not snapshot the A branch")?;
    let continuation = ButtonChord::new(0x80, 12);
    target.apply(&continuation);
    let first = (target.observe(), target.fingerprint());
    let first_lifetime_frame = target.execution_work();
    target.restore(&branch)?;
    target.apply(&continuation);
    let second = (target.observe(), target.fingerprint());
    let second_lifetime_frame = target.execution_work();
    if first != second {
        return Err("STB same-continuation snapshot replay diverged".into());
    }
    target.restore(&branch)?;
    target.apply(&ButtonChord::new(0x02, 12));
    let different_branch = target.observe();
    target.restore(&branch)?;
    target.apply(&continuation);
    let third = (target.observe(), target.fingerprint());
    if first != third {
        return Err("STB restore after a different branch diverged".into());
    }
    println!(
        "snapshot_restore {}",
        serde_json::to_string(&serde_json::json!({
            "same_continuation_exact": true,
            "different_branch_observation": different_branch,
            "repeated_endpoint": first.0,
            "repeated_fingerprint": first.1,
            "lifetime_frames_first": first_lifetime_frame,
            "lifetime_frames_second": second_lifetime_frame,
            "lifetime_clock_is_monotonic": second_lifetime_frame > first_lifetime_frame,
        }))?
    );

    target.restore(&genesis)?;
    let before_probe = (target.observe(), target.fingerprint());
    let before_probe_lifetime_frame = target.execution_work();
    let probe_survived = target.survives_probe(0, 45);
    let after_probe = (target.observe(), target.fingerprint());
    let after_probe_lifetime_frame = target.execution_work();
    if before_probe != after_probe || target.exit_kind() != nes_workload::target::ExitKind::Ok {
        return Err(
            "STB admission probe failed its live-RAM and cached-state restoration checks".into(),
        );
    }
    let continuation = ButtonChord::new(0x80, 12);
    target.apply(&continuation);
    let after_probed_continuation = (target.observe(), target.fingerprint());
    target.restore(&genesis)?;
    target.apply(&continuation);
    if (target.observe(), target.fingerprint()) != after_probed_continuation {
        return Err("STB post-probe continuation diverged".into());
    }
    target.restore(&genesis)?;
    println!(
        "admission_probe {}",
        serde_json::to_string(&serde_json::json!({
            "mask": "0x00",
            "frames": 45,
            "survived_future": probe_survived,
            "restored_exact": true,
            "lifetime_frames_before": before_probe_lifetime_frame,
            "lifetime_frames_after": after_probe_lifetime_frame,
            "lifetime_clock_is_monotonic": after_probe_lifetime_frame > before_probe_lifetime_frame,
            "state": target.mechanical_state(),
        }))?
    );

    let clean = StbTarget::from_rom_bytes_headless_with_ai(rom, core, core_sha256, ai)?;
    if clean.mechanical_state() != genesis.state() {
        return Err("STB clean reset disagreed with sealed genesis".into());
    }
    println!("clean_reset_exact true");
    Ok(())
}

fn parse_action(value: &str) -> Result<ButtonChord, Box<dyn Error>> {
    let (buttons, hold) = value
        .split_once(':')
        .ok_or("schedule actions use buttons:hold, e.g. 0x01:6")?;
    let buttons = buttons.trim().strip_prefix("0x").unwrap_or(buttons.trim());
    Ok(ButtonChord::new(
        u8::from_str_radix(buttons, 16)?,
        hold.parse()?,
    ))
}
