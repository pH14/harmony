// SPDX-License-Identifier: AGPL-3.0-or-later
//! Loader trust-boundary property test (conventions rule 4 /
//! no-panic-on-untrusted-input): arbitrary, truncated, and aligned-magic byte
//! images must always yield `Ok | Err(LoadError)` from `parse_header`/`load` —
//! never a panic, slice-index OOB, or arithmetic overflow.

use proptest::prelude::*;
use vmm_core::vendor::x86::multiboot::{self, MULTIBOOT_HEADER_MAGIC};

/// Image-size bound: full 16 KiB natively; tiny under Miri (allocation + the
/// interpreted copy dominate Miri's runtime — the UB surface is the bounds math,
/// not large buffers).
const MAX_IMAGE: usize = if cfg!(miri) { 256 } else { 16_384 };
/// Guest-RAM size bound: up to 2 MiB natively; small under Miri.
const MAX_RAM: usize = if cfg!(miri) { 1 << 13 } else { 1 << 21 };

/// Fewer cases under Miri (10–100× slower interpreter); ≥256 otherwise. Under Miri
/// failure persistence is also disabled — its regression file needs `getcwd`,
/// which Miri's isolation blocks (the same helper shape vm-state / vmm-backend use).
fn config(native: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 24 } else { native });
    if cfg!(miri) {
        cfg.failure_persistence = None;
    }
    cfg
}

proptest! {
    #![proptest_config(config(512))]

    /// Arbitrary bytes never panic the header parser.
    #[test]
    fn parse_header_total(bytes in proptest::collection::vec(any::<u8>(), 0..MAX_IMAGE)) {
        let _ = multiboot::parse_header(&bytes);
    }

    /// Arbitrary bytes + arbitrary RAM size never panic the loader.
    #[test]
    fn load_total(
        bytes in proptest::collection::vec(any::<u8>(), 0..MAX_IMAGE),
        ram_len in 0usize..MAX_RAM,
    ) {
        let mut ram = vec![0u8; ram_len];
        let _ = multiboot::load(&bytes, &mut ram);
    }

    /// An image with the magic planted at a random 4-byte-aligned offset (the
    /// realistic adversarial shape) still never panics — it reaches the field
    /// parse / checksum / address-field paths with arbitrary trailing bytes.
    #[test]
    fn aligned_magic_total(
        prefix_words in 0usize..(MAX_IMAGE / 8),
        rest in proptest::collection::vec(any::<u8>(), 0..512),
        ram_len in 0usize..MAX_RAM,
    ) {
        let mut bytes = vec![0u8; prefix_words * 4];
        bytes.extend_from_slice(&MULTIBOOT_HEADER_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&rest);
        prop_assert!(matches!(multiboot::parse_header(&bytes), Ok(_) | Err(_)));
        let mut ram = vec![0u8; ram_len];
        prop_assert!(matches!(multiboot::load(&bytes, &mut ram), Ok(_) | Err(_)));
    }
}
