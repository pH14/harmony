// SPDX-License-Identifier: AGPL-3.0-or-later

use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
struct ReviewedInputs {
    kernel: &'static str,
    initramfs: &'static str,
    ram_bytes: usize,
    cmdline: &'static str,
}

const REVIEWED: &[ReviewedInputs] =
    include!("../../../workloads/guest-images/admission/controlled-profiles.rs");

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
    if !REVIEWED.iter().any(|entry| {
        kernel == entry.kernel
            && initramfs == entry.initramfs
            && ram == entry.ram_bytes
            && cmdline == entry.cmdline
    }) {
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
        let mut identities = std::collections::BTreeSet::new();
        for entry in REVIEWED {
            let ReviewedInputs {
                kernel,
                initramfs,
                ram_bytes,
                cmdline,
            } = *entry;
            let identity = match_hashes(kernel, initramfs, ram_bytes, cmdline).unwrap();
            assert!(
                identities.insert(identity),
                "reviewed profiles must be distinct"
            );
            for (kernel, initramfs, ram, args) in [
                ("changed", initramfs, ram_bytes, cmdline),
                (kernel, "changed", ram_bytes, cmdline),
                (kernel, initramfs, ram_bytes + 4096, cmdline),
                (kernel, initramfs, ram_bytes, "changed"),
            ] {
                assert_eq!(match_hashes(kernel, initramfs, ram, args), None);
            }
            assert_eq!(
                linux_identity(b"kernel", b"initramfs", ram_bytes, cmdline),
                None
            );
            assert_eq!(
                match_hashes(
                    kernel,
                    initramfs,
                    ram_bytes,
                    &format!("{cmdline} init=/bin/sh")
                ),
                None
            );
        }
        assert!(!identities.is_empty());
    }
}
