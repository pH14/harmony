// SPDX-License-Identifier: AGPL-3.0-or-later

use nes_workload::{
    metroid::campaign::MetroidGame,
    mm2::campaign::Mm2Game,
    search::{
        archive::{ArchiveKey, cell_of},
        campaign::{Workload, inspection::inspect_checkpoint},
    },
    smb::campaign::SmbGame,
};
use serde_json::json;
use std::{
    error::Error,
    io::{self, Write},
    path::Path,
};

fn emit<G: Workload>(path: &Path, key_policy: &str) -> Result<(), Box<dyn Error>> {
    let inspected = inspect_checkpoint::<G>(path)?;
    if inspected
        .header
        .policies
        .get("key_policy")
        .map(String::as_str)
        != Some(key_policy)
    {
        return Err("checkpoint key policy differs from the selected adapter".into());
    }
    let mut out = io::BufWriter::new(io::stdout().lock());
    writeln!(
        out,
        "{}",
        json!({"format":"nes-checkpoint-inspection-v1", "header":inspected.header, "checkpoint_sha256":inspected.sha256, "historical_entries_dropped":inspected.historical_entries_dropped, "draws_by_cell":inspected.selector.draws_by_cell,"tier_runs":inspected.selector.tier_runs})
    )?;
    for entry in inspected.entries {
        writeln!(
            out,
            "{}",
            json!({"id":entry.id,"key":entry.key,"tier":format!("{:?}",entry.key.progress()),"cell":format!("{:?}",cell_of(entry.key)),"active":entry.active,"selector":entry.selector,"snapshot":entry.snapshot.filter(|_| entry.active).map(compact_snapshot).transpose()?})
        )?;
    }
    out.flush()?;
    Ok(())
}

fn compact_snapshot<S: serde::Serialize>(snapshot: S) -> Result<serde_json::Value, Box<dyn Error>> {
    use sha2::{Digest, Sha256};
    let mut value = serde_json::to_value(&snapshot)?;
    let identity = format!("{:x}", Sha256::digest(serde_json::to_vec(&snapshot)?));
    let packed: Vec<u8> = serde_json::from_value(
        value
            .get_mut("emulator_state")
            .ok_or("snapshot has no emulator state")?
            .take(),
    )?;
    if packed.len() < 4 {
        return Err("truncated compressed snapshot".into());
    }
    let size = u32::from_le_bytes(packed[..4].try_into()?) as usize;
    if size > 64 << 20 {
        return Err("snapshot exceeds inspection limit".into());
    }
    let mut state = Vec::new();
    nes_workload::nes_backend::unpack_state_into(&packed, size, &mut state)?;
    value["state_sha256"] = json!(identity);
    value
        .as_object_mut()
        .ok_or("snapshot is not an object")?
        .remove("emulator_state");
    value["decoded_ram"] = decode_ram(&state)?;
    Ok(value)
}

fn decode_ram(state: &[u8]) -> Result<serde_json::Value, Box<dyn Error>> {
    if state.len() < 184 || !state.starts_with(b"HQNESST3") {
        return Err("invalid QuickNES envelope".into());
    }
    if u64::from_le_bytes(state[176..184].try_into()?) != (state.len() - 184) as u64 {
        return Err("QuickNES envelope size differs".into());
    }
    let data = &state[184..];
    if !data.starts_with(b"NESS\xff\xff\xff\xff") {
        return Err("invalid QuickNES state".into());
    }
    let mut offset = 8;
    let mut work = None;
    let mut save = None;
    while offset < data.len() {
        let header = data
            .get(offset..offset + 8)
            .ok_or("truncated QuickNES block")?;
        let length = u32::from_le_bytes(header[4..8].try_into()?) as usize;
        offset += 8;
        let end = offset
            .checked_add(length)
            .ok_or("QuickNES block overflow")?;
        let payload = data.get(offset..end).ok_or("truncated QuickNES payload")?;
        let target = match &header[..4] {
            b"LRAM" | b"WRAM" => Some(&mut work),
            b"SRAM" => Some(&mut save),
            _ => None,
        };
        if let Some(target) = target
            && target.replace(payload).is_some()
        {
            return Err("duplicate QuickNES RAM block".into());
        }
        offset = end;
    }
    let work = work
        .filter(|ram| ram.len() == 2048)
        .ok_or("QuickNES state lacks work RAM")?;
    let save = save.unwrap_or_default();
    if save.len() > 8192 {
        return Err("QuickNES save RAM exceeds its address window".into());
    }
    Ok(json!({"work":work,"save":save}))
}

#[cfg(test)]
mod tests {
    use super::decode_ram;

    fn fixture() -> Vec<u8> {
        let mut state = b"HQNESST3".to_vec();
        state.resize(184, 0);
        state.extend_from_slice(b"NESS\xff\xff\xff\xffLRAM");
        state.extend_from_slice(&2048_u32.to_le_bytes());
        state.extend_from_slice(&[7; 2048]);
        let length = (state.len() - 184) as u64;
        state[176..184].copy_from_slice(&length.to_le_bytes());
        state
    }

    #[test]
    fn ram_decoder_checks_boundaries_and_required_blocks() {
        let good = fixture();
        assert_eq!(decode_ram(&good).unwrap()["work"][0], 7);
        for end in [0, 8, 183, 184, good.len() - 1] {
            assert!(decode_ram(&good[..end]).is_err());
        }
        let mut wrong = good.clone();
        wrong[192..196].copy_from_slice(b"JUNK");
        assert!(decode_ram(&wrong).is_err());
        let mut huge = good;
        huge[196..200].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_ram(&huge).is_err());
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [game, checkpoint] = args.as_slice() else {
        return Err("usage: nes-checkpoint-inspect <auto|smb|metroid|mm2> FINAL.ckpt".into());
    };
    match game.to_str() {
        Some("auto") => {
            let header = nes_workload::search::checkpoint::read_header(Path::new(checkpoint))?;
            match header.policies.get("key_policy").map(String::as_str) {
                Some(nes_workload::smb::archive::KEY_POLICY_IDENTIFIER) => emit::<SmbGame>(
                    Path::new(checkpoint),
                    nes_workload::smb::archive::KEY_POLICY_IDENTIFIER,
                ),
                Some(nes_workload::metroid::archive::KEY_POLICY_IDENTIFIER) => emit::<MetroidGame>(
                    Path::new(checkpoint),
                    nes_workload::metroid::archive::KEY_POLICY_IDENTIFIER,
                ),
                Some(nes_workload::mm2::archive::KEY_POLICY_IDENTIFIER) => emit::<Mm2Game>(
                    Path::new(checkpoint),
                    nes_workload::mm2::archive::KEY_POLICY_IDENTIFIER,
                ),
                _ => Err("checkpoint key policy needs a matching workload inspector".into()),
            }
        }
        Some("smb") => emit::<SmbGame>(
            Path::new(checkpoint),
            nes_workload::smb::archive::KEY_POLICY_IDENTIFIER,
        ),
        Some("metroid") => emit::<MetroidGame>(
            Path::new(checkpoint),
            nes_workload::metroid::archive::KEY_POLICY_IDENTIFIER,
        ),
        Some("mm2") => emit::<Mm2Game>(
            Path::new(checkpoint),
            nes_workload::mm2::archive::KEY_POLICY_IDENTIFIER,
        ),
        _ => Err("unsupported checkpoint workload".into()),
    }
}
