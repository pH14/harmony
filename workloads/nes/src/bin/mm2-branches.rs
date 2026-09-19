// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{env, error::Error, fs, path::PathBuf};

use nes_workload::{
    mm2::target::{Mm2Input, Mm2Stage, Mm2Target},
    target::Target,
};
use serde_json::json;
use sha2::{Digest, Sha256};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let stage = Mm2Stage::parse(&args.next().ok_or("missing stage")?)?;
    let prefix_path = args.next().ok_or("missing stage-prefix input")?;
    let root_path = args.next().ok_or("missing stage-relative root input")?;
    let bank_path = args.next().ok_or("missing suffix-bank JSON array")?;
    let output_path = args.next().ok_or("missing output JSON path")?;
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }
    let prefix_bytes = fs::read(prefix_path)?;
    let root_bytes = fs::read(root_path)?;
    let bank_bytes = fs::read(bank_path)?;
    let prefix: Mm2Input = serde_json::from_slice(&prefix_bytes)?;
    let root: Mm2Input = serde_json::from_slice(&root_bytes)?;
    let bank: Vec<Mm2Input> = serde_json::from_slice(&bank_bytes)?;
    let rom = fs::read(env::var_os("HARMONY_MM2_ROM").ok_or("missing HARMONY_MM2_ROM")?)?;
    let core =
        PathBuf::from(env::var_os("HARMONY_QUICKNES_CORE").ok_or("missing HARMONY_QUICKNES_CORE")?);
    let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core)?));
    let mut target =
        Mm2Target::from_rom_bytes_after(&rom, &core, &core_sha256, &prefix.actions, stage)?;
    target.advance_genesis(&root.actions)?;
    let genesis = target.observe();
    let snapshot = target.snapshot().ok_or("could not snapshot root")?;
    let mut results = Vec::with_capacity(bank.len());
    for (index, input) in bank.iter().enumerate() {
        target.restore(&snapshot)?;
        let mut observations = Vec::new();
        let start_work = target.execution_work();
        for action in &input.actions {
            if target.is_dead() || target.defeated_a_boss() {
                break;
            }
            target.apply(action);
            observations.push(target.observe());
        }
        results.push(json!({
            "index": index,
            "input": input,
            "actions_applied": observations.len(),
            "execution_work": target.execution_work().saturating_sub(start_work),
            "endpoint": target.observe(),
            "observations": observations,
            "exit_kind": format!("{:?}", target.exit_kind()),
        }));
    }
    fs::write(
        output_path,
        serde_json::to_vec_pretty(&json!({
            "rom_sha256": format!("{:x}", Sha256::digest(&rom)),
            "core_sha256": core_sha256,
            "prefix_sha256": format!("{:x}", Sha256::digest(&prefix_bytes)),
            "root_sha256": format!("{:x}", Sha256::digest(&root_bytes)),
            "bank_sha256": format!("{:x}", Sha256::digest(&bank_bytes)),
            "genesis": genesis,
            "results": results,
        }))?,
    )?;
    Ok(())
}
