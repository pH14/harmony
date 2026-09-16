// SPDX-License-Identifier: AGPL-3.0-or-later

use sha2::{Digest, Sha256};

const KERNEL: &str = "7ce25244cf1d138db1286ce61fb1c880bd224b2866ffaebd1ec19d04b2ad69b6";
const MINIMAL: &str = "3609719da1de4f95944bfa2e9e79960698ed3b6784e594777440251bcfb1aa21";
const NES: &str = "075c8f52e17978602c7630fe807d39e643d7d2be590c4acb52093c87c50b2a5c";
const POSTGRES: &str = "9ab0487109626b62c9d8588d62b831302d1a73cbe7c6858c7044404272e74d44";
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic hpet=disable harmony_pvclock noxsaveopt noxsaves LD_BIND_NOW=1";

pub fn linux_identity(
    kernel: &[u8],
    initramfs: &[u8],
    ram_bytes: usize,
    cmdline: &str,
) -> Option<[u8; 32]> {
    match_hashes(
        &format!("{:x}", Sha256::digest(kernel)),
        &format!("{:x}", Sha256::digest(initramfs)),
        ram_bytes,
        cmdline,
    )
}

fn match_hashes(kernel: &str, initramfs: &str, ram: usize, cmdline: &str) -> Option<[u8; 32]> {
    if kernel != KERNEL {
        return None;
    }
    let minimal = initramfs == MINIMAL && ram == 256 << 20 && cmdline == CMDLINE;
    let workload = matches!(initramfs, NES | POSTGRES)
        && ram == 128 << 20
        && cmdline == format!("{CMDLINE} rdinit=/init");
    if !minimal && !workload {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(b"harmony.verified-linux-xsave-profile.v1\0");
    hash.update(kernel.as_bytes());
    hash.update(initramfs.as_bytes());
    hash.update((ram as u64).to_le_bytes());
    hash.update(cmdline.as_bytes());
    Some(hash.finalize().into())
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64", not(miri)))]
mod live_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exact_reviewed_linux_inputs_select_logical_identity() {
        let cmdline = format!("{CMDLINE} rdinit=/init");
        let nes = match_hashes(KERNEL, NES, 128 << 20, &cmdline).unwrap();
        let postgres = match_hashes(KERNEL, POSTGRES, 128 << 20, &cmdline).unwrap();
        let minimal = match_hashes(KERNEL, MINIMAL, 256 << 20, CMDLINE).unwrap();
        assert_ne!(nes, postgres);
        assert_ne!(nes, minimal);
        for (kernel, initramfs, ram, args) in [
            ("changed", NES, 128 << 20, cmdline.as_str()),
            (KERNEL, "changed", 128 << 20, cmdline.as_str()),
            (KERNEL, NES, 256 << 20, cmdline.as_str()),
            (KERNEL, NES, 128 << 20, CMDLINE),
            (KERNEL, MINIMAL, 128 << 20, CMDLINE),
            (KERNEL, MINIMAL, 256 << 20, cmdline.as_str()),
        ] {
            assert_eq!(match_hashes(kernel, initramfs, ram, args), None);
        }
        assert_eq!(
            linux_identity(b"kernel", b"initramfs", 128 << 20, &cmdline),
            None
        );
        assert_eq!(
            match_hashes(KERNEL, NES, 128 << 20, &format!("{cmdline} init=/bin/sh")),
            None
        );
    }
}
