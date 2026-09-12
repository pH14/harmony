// SPDX-License-Identifier: AGPL-3.0-or-later
//! Backend equivalence through actions, restored continuations, and probes.
#[cfg(not(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
)))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("nes-backend-oracle requires Linux KVM and the consonance feature".into())
}
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    real::run()
}
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
mod real {
    use machine::{consonance::ConsonanceMachine, nes::ButtonChord};
    use nes_workload::{
        nova::target::NovaTarget,
        package::RomKind,
        smb::target::SmbTarget,
        target::{ExitKind, Target},
    };
    use sha2::{Digest, Sha256};
    use std::{error::Error, fs, path::PathBuf};
    pub fn run() -> Result<(), Box<dyn Error>> {
        let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
        let [rom, core, kernel, base] = args.as_slice() else {
            return Err("usage: nes-backend-oracle ROM CORE KERNEL NES_BASE_INITRAMFS".into());
        };
        let rom = fs::read(rom)?;
        let hash = format!("{:x}", Sha256::digest(fs::read(core)?));
        let image = nes_workload::prepare::prepare(&rom, &fs::read(base)?)?;
        let vm = ConsonanceMachine::new(&fs::read(kernel)?, &image)?;
        if !vm.starts_at_power_on() {
            return Err("oracle requires a generic power-on NES image".into());
        }
        macro_rules! compare_game {
            ($native:expr, $vm:expr, $terminal:expr) => {
                compare(
                    $native?,
                    $vm?,
                    |t| t.last_action_observations().to_vec(),
                    |t| t.last_action_observations().to_vec(),
                    |t| t.survives_probe(0x80, 16),
                    |t| t.survives_probe(0x80, 16),
                    $terminal,
                    $terminal,
                )
            };
        }
        match RomKind::identify(&rom)? {
            RomKind::Smb => compare_game!(
                SmbTarget::from_smb_rom_bytes_headless(&rom, core, &hash),
                SmbTarget::from_machine(vm),
                |t| t.is_dead() || t.is_victory()
            ),
            RomKind::Nova => compare_game!(
                NovaTarget::from_rom_bytes_headless(&rom, core, &hash),
                NovaTarget::from_power_on(vm),
                |t| t.is_dead() || t.cleared_a_level()
            ),
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn compare<N, V>(
        mut native: N,
        mut vm: V,
        n_observe: impl Fn(&N) -> Vec<N::Observations>,
        v_observe: impl Fn(&V) -> Vec<N::Observations>,
        n_probe: impl Fn(&mut N) -> bool,
        v_probe: impl Fn(&mut V) -> bool,
        n_terminal: impl Fn(&N) -> bool,
        v_terminal: impl Fn(&V) -> bool,
    ) -> Result<(), Box<dyn Error>>
    where
        N: Target<Action = ButtonChord>,
        V: Target<Action = ButtonChord, Observations = N::Observations>,
    {
        if native.observe() != vm.observe() {
            return Err("genesis observations differ".into());
        }
        let ng = native.snapshot().ok_or("native genesis snapshot failed")?;
        let vg = vm.snapshot().ok_or("VM genesis snapshot failed")?;
        let mut digest = Sha256::new();
        let mut count = 0_u64;
        for sequence in 0..8_u8 {
            native.restore(&ng)?;
            vm.restore(&vg)?;
            for index in 0..16_u8 {
                let buttons = [0, 0x80, 0x81, 0x83, 0x40, 0x41, 0x01, 0x82]
                    [usize::from(sequence.wrapping_add(index) % 8)];
                let action = ButtonChord::new(buttons, [1, 4, 16, 60, 120][usize::from(index % 5)]);
                let ns = native.snapshot().ok_or("native snapshot failed")?;
                let vs = vm.snapshot().ok_or("VM snapshot failed")?;
                native.apply(&action);
                vm.apply(&action);
                if native.exit_kind() != ExitKind::Ok || vm.exit_kind() != ExitKind::Ok {
                    return Err("backend execution failed".into());
                }
                let expected = n_observe(&native);
                if expected != v_observe(&vm)
                    || native.observe() != vm.observe()
                    || n_terminal(&native) != v_terminal(&vm)
                {
                    return Err(
                        format!("backend mismatch at sequence {sequence} action {index}").into(),
                    );
                }
                digest.update(serde_json::to_vec(&expected)?);
                count += 1;
                native.restore(&ns)?;
                vm.restore(&vs)?;
                if n_probe(&mut native) != v_probe(&mut vm) {
                    return Err("probe outcomes differ".into());
                }
                native.apply(&action);
                vm.apply(&action);
                if native.exit_kind() != ExitKind::Ok
                    || vm.exit_kind() != ExitKind::Ok
                    || n_observe(&native) != expected
                    || v_observe(&vm) != expected
                {
                    return Err(format!(
                        "restored/probed continuation differs at sequence {sequence} action {index}"
                    )
                    .into());
                }
                if n_terminal(&native) {
                    break;
                }
            }
        }
        println!(
            "NES_BACKEND_ORACLE_OK actions={count} sha256={:x}",
            digest.finalize()
        );
        Ok(())
    }
}
