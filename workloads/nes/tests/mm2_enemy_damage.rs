// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use nes_workload::{
    mm2::target::{ButtonChord, Mm2Input, Mm2Stage, Mm2Target},
    target::{ExitKind, Target},
};
use sha2::{Digest, Sha256};

type TestResult = Result<(), Box<dyn Error>>;

struct Harness {
    rom: Vec<u8>,
    core: PathBuf,
    core_sha256: String,
    prefix: Vec<ButtonChord>,
    root: Vec<ButtonChord>,
    stage: Mm2Stage,
}

impl Harness {
    fn target(&self) -> Result<Mm2Target, Box<dyn Error>> {
        let mut target = Mm2Target::from_rom_bytes_after(
            &self.rom,
            &self.core,
            &self.core_sha256,
            &self.prefix,
            self.stage,
        )?;
        target.advance_genesis(&self.root)?;
        Ok(target)
    }
}

fn required_path(names: &[&str]) -> Result<PathBuf, Box<dyn Error>> {
    names
        .iter()
        .find_map(|name| env::var_os(name).map(PathBuf::from))
        .ok_or_else(|| format!("one of {} must name a fixture", names.join(", ")).into())
}

fn read_input(path: &PathBuf) -> Result<Mm2Input, Box<dyn Error>> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn load_harness() -> Result<Harness, Box<dyn Error>> {
    let rom_path = required_path(&["HARMONY_MM2_ROM"])?;
    let core = required_path(&["HARMONY_QUICKNES_CORE"])?;
    let prefix_path = required_path(&["HARMONY_MM2_STAGE_PREFIX", "HARMONY_MM2_PREFIX_INPUT"])?;
    let root_path = required_path(&["HARMONY_MM2_DAMAGE_ROOT", "HARMONY_MM2_ROOT_INPUT"])?;
    let stage =
        Mm2Stage::parse(&env::var("HARMONY_MM2_STAGE").unwrap_or_else(|_| "wily4".to_owned()))?;
    let prefix = read_input(&prefix_path)?;
    let root = read_input(&root_path)?;
    let rom = fs::read(&rom_path)?;
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core)?));
    Ok(Harness {
        rom,
        core,
        core_sha256,
        prefix: prefix.actions,
        root: root.actions,
        stage,
    })
}

fn assert_ok(target: &Mm2Target) {
    assert_eq!(target.exit_kind(), ExitKind::Ok);
    assert_eq!(target.mechanical_state().enemy_damage, 4);
}

#[test]
#[ignore = "requires the external MM2 ROM, QuickNES core, and armor-first-hit root fixture"]
fn real_mm2_enemy_damage_survives_action_boundaries_snapshot_restore_and_reset() -> TestResult {
    let harness = load_harness()?;
    let mut held = harness.target()?;
    let genesis = held.mechanical_state();
    assert_eq!(genesis.enemy_damage, 4);
    held.apply(&ButtonChord::new(80, 4));
    assert_ok(&held);

    let mut segmented = harness.target()?;
    for _ in 0..4 {
        segmented.apply(&ButtonChord::new(80, 1));
        assert_ok(&segmented);
    }
    assert_eq!(held.mechanical_state(), segmented.mechanical_state());

    let mut rendered = harness.target()?;
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let metadata = rendered.render_input(
        &Mm2Input {
            actions: vec![ButtonChord::new(80, 4)],
        },
        0,
        &mut video,
        &mut audio,
    )?;
    assert_eq!(metadata.input_endpoint, held.mechanical_state());

    let mut restored = harness.target()?;
    let snapshot = restored
        .snapshot()
        .ok_or("failed to snapshot damage root")?;
    restored.apply(&ButtonChord::new(80, 4));
    assert_ok(&restored);
    restored.restore(&snapshot)?;
    assert_eq!(restored.mechanical_state(), genesis);
    assert_eq!(restored.observe().decoded.enemy_damage, 4);
    restored.apply(&ButtonChord::new(80, 1));
    assert_ok(&restored);
    restored.reset();
    assert_eq!(restored.mechanical_state(), genesis);
    assert_eq!(restored.observe().decoded.enemy_damage, 4);
    Ok(())
}
