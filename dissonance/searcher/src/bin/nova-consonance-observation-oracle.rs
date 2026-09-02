// SPDX-License-Identifier: AGPL-3.0-or-later

//! Compare per-action Nova observations from direct QuickNES and Consonance.

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    real::run()
}

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
)))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("nova-consonance-observation-oracle requires Linux KVM outside Miri".into())
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
mod real {
    use std::{env, error::Error, ffi::OsString, fs, path::PathBuf};

    use machine::consonance::ConsonanceMachine;
    use searcher::{
        nova::target::{ButtonChord, MAX_HOLD_FRAMES, NovaTarget},
        target::Target,
    };
    use sha2::{Digest, Sha256};

    struct Args {
        kernel: PathBuf,
        initramfs: PathBuf,
        rom: PathBuf,
        core: PathBuf,
        seed: u64,
        sequences: u64,
        actions_per_sequence: u64,
    }

    impl Args {
        fn parse() -> Result<Self, Box<dyn Error>> {
            let mut kernel = None;
            let mut initramfs = None;
            let mut rom = None;
            let mut core = None;
            let mut seed = 0x4e4f_5641_5f4f_4253_u64;
            let mut sequences = 100_u64;
            let mut actions_per_sequence = 8_u64;
            let mut args = env::args_os().skip(1);
            while let Some(flag) = args.next() {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value after {}", flag.to_string_lossy()))?;
                match flag.to_string_lossy().as_ref() {
                    "--kernel" => kernel = Some(PathBuf::from(value)),
                    "--initramfs" => initramfs = Some(PathBuf::from(value)),
                    "--rom" => rom = Some(PathBuf::from(value)),
                    "--core" => core = Some(PathBuf::from(value)),
                    "--seed" => seed = parse_number("seed", value)?,
                    "--sequences" => sequences = parse_number("sequences", value)?,
                    "--actions-per-sequence" => {
                        actions_per_sequence = parse_number("actions-per-sequence", value)?;
                    }
                    other => return Err(format!("unknown argument {other:?}").into()),
                }
            }
            if sequences == 0 || actions_per_sequence == 0 {
                return Err("sequence and action counts must both be positive".into());
            }
            Ok(Self {
                kernel: kernel.ok_or("missing --kernel")?,
                initramfs: initramfs.ok_or("missing --initramfs")?,
                rom: rom.ok_or("missing --rom")?,
                core: core.ok_or("missing --core")?,
                seed,
                sequences,
                actions_per_sequence,
            })
        }
    }

    fn parse_number(name: &str, value: OsString) -> Result<u64, Box<dyn Error>> {
        let text = value
            .into_string()
            .map_err(|_| format!("{name} is not UTF-8"))?
            .replace('_', "");
        if let Some(hex) = text.strip_prefix("0x") {
            Ok(u64::from_str_radix(hex, 16)?)
        } else {
            Ok(text.parse()?)
        }
    }

    fn next_u64(state: &mut u64) -> u64 {
        let mut value = *state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        *state = value;
        value
    }

    fn next_action(state: &mut u64) -> ButtonChord {
        let buttons = next_u64(state).to_le_bytes()[0];
        let hold = next_u64(state) % u64::from(MAX_HOLD_FRAMES);
        ButtonChord::new(buttons, u8::try_from(hold + 1).unwrap_or(MAX_HOLD_FRAMES))
    }

    pub fn run() -> Result<(), Box<dyn Error>> {
        let args = Args::parse()?;
        let rom = fs::read(&args.rom)?;
        let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&args.core)?));
        let mut direct = NovaTarget::from_rom_bytes_headless(&rom, &args.core, &core_sha256)?;
        let kernel = fs::read(&args.kernel)?;
        let initramfs = fs::read(&args.initramfs)?;
        let mut consonance =
            NovaTarget::from_machine(ConsonanceMachine::new(&kernel, &initramfs)?)?;
        let mut rng = args.seed;
        let mut compared_actions = 0_u64;
        let mut stream = Sha256::new();
        stream.update(b"nova-consonance-observation-oracle-v1\0");
        stream.update(args.seed.to_le_bytes());
        stream.update(args.sequences.to_le_bytes());
        stream.update(args.actions_per_sequence.to_le_bytes());

        for sequence in 0..args.sequences {
            direct.reset();
            consonance.reset();
            if direct.mechanical_state() != consonance.mechanical_state() {
                return Err(format!("setup state mismatch in sequence {sequence}").into());
            }
            for action_index in 0..args.actions_per_sequence {
                let action = next_action(&mut rng);
                direct.apply(&action);
                consonance.apply(&action);
                if direct.last_action_observations() != consonance.last_action_observations() {
                    return Err(format!(
                        "observation mismatch at sequence {sequence} action {action_index}: direct={:?} consonance={:?}",
                        direct.last_action_observations(),
                        consonance.last_action_observations(),
                    )
                    .into());
                }
                let encoded = serde_json::to_vec(direct.last_action_observations())?;
                stream.update(sequence.to_le_bytes());
                stream.update(action_index.to_le_bytes());
                stream.update([action.buttons, action.bounded_hold_frames()]);
                stream.update(u64::try_from(encoded.len())?.to_le_bytes());
                stream.update(encoded);
                compared_actions = compared_actions.saturating_add(1);
            }
        }

        println!(
            "NOVA_CONSONANCE_OBSERVATION_ORACLE_OK sequences={} actions={} seed={:016x} stream_sha256={:x}",
            args.sequences,
            compared_actions,
            args.seed,
            stream.finalize(),
        );
        Ok(())
    }
}
