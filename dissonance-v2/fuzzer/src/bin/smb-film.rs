// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deterministic M7 PNG frame-strip generator for recorded SMB milestone inputs.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use fuzzer::{
    phase4b::{
        SmbCampaignReport, SmbConfiguredReport, SmbInput, SmbTarget, observe_smb_input,
        smb_milestones_from_wram,
    },
    target::Target,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const WIDTH: usize = 256;
const HEIGHT: usize = 240;

#[derive(Debug, Deserialize)]
struct M5Report {
    ratchet: Vec<SmbCampaignReport>,
}

#[derive(Debug, Serialize)]
struct FilmManifest {
    source_report: PathBuf,
    milestone: String,
    rom_sha256: String,
    input: SmbInput,
    frames: Vec<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let mode = args.next().ok_or(
        "usage: smb-film <m5|campaign|configured> <report> [run-index] <milestone> <output-dir>",
    )?;
    let source = PathBuf::from(args.next().ok_or("missing source report")?);
    let (campaign, milestone, output) = match mode.to_str() {
        Some("m5") => {
            let run_index: usize = args
                .next()
                .ok_or("missing M5 run index")?
                .to_string_lossy()
                .parse()?;
            let milestone = args
                .next()
                .ok_or("missing milestone")?
                .to_string_lossy()
                .into_owned();
            let output = PathBuf::from(args.next().ok_or("missing output directory")?);
            let report: M5Report = serde_json::from_slice(&fs::read(&source)?)?;
            let campaign = report
                .ratchet
                .get(run_index)
                .ok_or("M5 run index is out of bounds")?
                .clone();
            (campaign, milestone, output)
        }
        Some("campaign") => {
            let milestone = args
                .next()
                .ok_or("missing milestone")?
                .to_string_lossy()
                .into_owned();
            let output = PathBuf::from(args.next().ok_or("missing output directory")?);
            let campaign: SmbCampaignReport = serde_json::from_slice(&fs::read(&source)?)?;
            (campaign, milestone, output)
        }
        Some("configured") => {
            let milestone = args
                .next()
                .ok_or("missing milestone")?
                .to_string_lossy()
                .into_owned();
            let output = PathBuf::from(args.next().ok_or("missing output directory")?);
            let report: SmbConfiguredReport = serde_json::from_slice(&fs::read(&source)?)?;
            (report.campaign, milestone, output)
        }
        _ => return Err("unknown film source mode".into()),
    };
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = fs::read(rom_path)?;
    let input = milestone_input(&campaign, &milestone, &rom)?;
    fs::create_dir_all(&output)?;
    let mut target = SmbTarget::from_smb_rom_bytes(&rom)?;
    let mut frames = Vec::new();
    write_frame(&output, 0, &target.frame_rgba(), &mut frames)?;
    for (index, action) in input.actions.iter().enumerate() {
        target.apply(action);
        if target.exit_kind() != libafl::executors::ExitKind::Ok {
            return Err(format!("emulator failed while rendering action {index}").into());
        }
        write_frame(&output, index + 1, &target.frame_rgba(), &mut frames)?;
        if target.is_dead() {
            break;
        }
    }
    let manifest = FilmManifest {
        source_report: source,
        milestone,
        rom_sha256: format!("{:x}", Sha256::digest(&rom)),
        input,
        frames,
    };
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    Ok(())
}

fn milestone_input(
    campaign: &SmbCampaignReport,
    milestone: &str,
    rom: &[u8],
) -> Result<SmbInput, Box<dyn Error>> {
    match milestone {
        "progress" => campaign.first_inputs.progress_into_1_1.clone(),
        "flag" => campaign.first_inputs.flag_1_1.clone(),
        "1-2" => campaign.first_inputs.level_1_2.clone(),
        "onward" => campaign.first_inputs.onward.clone(),
        "max-scroll" => {
            let target = campaign.milestones.max_1_1_scroll_bucket;
            for input in &campaign.corpus {
                let mut reached = 0_u16;
                for observation in observe_smb_input(rom, input)? {
                    let wram = observation
                        .wram
                        .as_slice()
                        .try_into()
                        .map_err(|_| "observation WRAM is not exactly 2 KiB")?;
                    reached = reached.max(smb_milestones_from_wram(wram).max_1_1_scroll_bucket);
                }
                if reached == target {
                    return Ok(input.clone());
                }
            }
            return Err(format!(
                "campaign corpus has no input reaching recorded max scroll bucket {target}"
            )
            .into());
        }
        _ => {
            return Err(
                "unknown milestone (expected progress, max-scroll, flag, 1-2, or onward)".into(),
            );
        }
    }
    .ok_or_else(|| format!("campaign has no first-reaching input for {milestone}").into())
}

fn write_frame(
    directory: &Path,
    index: usize,
    rgba: &[u8],
    frames: &mut Vec<String>,
) -> Result<(), Box<dyn Error>> {
    let filename = format!("frame-{index:04}.png");
    fs::write(directory.join(&filename), encode_png_rgba(rgba)?)?;
    frames.push(filename);
    Ok(())
}

fn encode_png_rgba(rgba: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
    let expected = WIDTH
        .checked_mul(HEIGHT)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or("PNG dimensions overflow")?;
    if rgba.len() != expected {
        return Err("unexpected TetaNES RGBA frame length".into());
    }
    let mut scanlines = Vec::with_capacity(expected + HEIGHT);
    for row in rgba.chunks_exact(WIDTH * 4) {
        scanlines.push(0);
        scanlines.extend_from_slice(row);
    }
    let compressed = zlib_stored(&scanlines)?;
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(WIDTH as u32).to_be_bytes());
    ihdr.extend_from_slice(&(HEIGHT as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    append_chunk(&mut png, *b"IHDR", &ihdr)?;
    append_chunk(&mut png, *b"IDAT", &compressed)?;
    append_chunk(&mut png, *b"IEND", &[])?;
    Ok(png)
}

fn zlib_stored(data: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut result = vec![0x78, 0x01];
    let mut remaining = data;
    while !remaining.is_empty() {
        let length = remaining.len().min(u16::MAX as usize);
        let final_block = length == remaining.len();
        result.push(u8::from(final_block));
        let length_u16 = u16::try_from(length)?;
        result.extend_from_slice(&length_u16.to_le_bytes());
        result.extend_from_slice(&(!length_u16).to_le_bytes());
        result.extend_from_slice(&remaining[..length]);
        remaining = &remaining[length..];
    }
    result.extend_from_slice(&adler32(data).to_be_bytes());
    Ok(result)
}

fn append_chunk(png: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) -> Result<(), Box<dyn Error>> {
    png.extend_from_slice(&u32::try_from(data.len())?.to_be_bytes());
    png.extend_from_slice(&kind);
    png.extend_from_slice(data);
    let mut checksum_input = Vec::with_capacity(4 + data.len());
    checksum_input.extend_from_slice(&kind);
    checksum_input.extend_from_slice(data);
    png.extend_from_slice(&crc32(&checksum_input).to_be_bytes());
    Ok(())
}

fn adler32(data: &[u8]) -> u32 {
    const MODULUS: u32 = 65_521;
    let mut a = 1_u32;
    let mut b = 0_u32;
    for byte in data {
        a = (a + u32::from(*byte)) % MODULUS;
        b = (b + a) % MODULUS;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::encode_png_rgba;

    #[test]
    fn png_encoder_writes_signature_and_terminal_chunk() {
        let rgba = vec![0_u8; 256 * 240 * 4];
        let png = encode_png_rgba(&rgba).expect("encode PNG");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }
}
