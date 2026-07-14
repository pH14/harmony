// SPDX-License-Identifier: AGPL-3.0-or-later
//! Linux-loader trust-boundary property test (task 30 gate 2; conventions rule 4
//! / no-panic-on-untrusted-input): arbitrary, truncated, and valid-magic-prefixed
//! bzImage bytes — with an arbitrary initramfs and guest-RAM size — must always
//! yield `Ok | Err(LinuxLoadError)` from `parse_setup_header` / `load`, **never** a
//! panic, slice-index OOB, or arithmetic overflow. The loader runs entirely over
//! untrusted image bytes, so this is the totality gate.

use proptest::prelude::*;
use vmm_core::vendor::x86::linux_loader::{self, SETUP_HEADER_OFFSET};

/// Image-size bound: a few KiB natively (enough to plant a full setup header and
/// a protected-mode tail); tiny under Miri (allocation + interpreted copies
/// dominate — the UB surface is the bounds math, not large buffers).
const MAX_IMAGE: usize = if cfg!(miri) { 256 } else { 8192 };
/// Initramfs-size bound.
const MAX_INITRAMFS: usize = if cfg!(miri) { 64 } else { 4096 };
/// Guest-RAM size bound: up to ~8 MiB natively (large enough that a small kernel
/// at `pref_address` 1 MiB can actually load, exercising the success path too);
/// small under Miri.
const MAX_RAM: u64 = if cfg!(miri) { 1 << 14 } else { 8 << 20 };

/// Fewer cases under Miri (10–100× slower interpreter); ≥256 otherwise. Under Miri
/// failure persistence is disabled — its regression file needs `getcwd`, which
/// Miri's isolation blocks (the same shape the multiboot loader proptest uses).
fn config(native: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 16 } else { native });
    if cfg!(miri) {
        cfg.failure_persistence = None;
    }
    cfg
}

/// Plant the four gating values a bzImage needs (`boot_flag`, `HdrS`, a ≥2.12
/// `version`, the `XLF_KERNEL_64` bit) at their fixed offsets in a buffer, plus an
/// arbitrary `setup_sects`/`pref_address`/`init_size` — the realistic adversarial
/// shape that reaches deep into `load`'s geometry math.
fn plant_bzimage(setup_sects: u8, pref_address: u32, init_size: u32, tail_len: usize) -> Vec<u8> {
    let real_sects = if setup_sects == 0 { 4 } else { setup_sects };
    let pm_off = (usize::from(real_sects) + 1) * 512;
    let mut img = vec![0u8; pm_off + tail_len];
    img[0x1fe..0x200].copy_from_slice(&0xAA55u16.to_le_bytes()); // boot_flag
    img[0x202..0x206].copy_from_slice(&0x5372_6448u32.to_le_bytes()); // "HdrS"
    img[0x206..0x208].copy_from_slice(&0x020fu16.to_le_bytes()); // version 2.15
    img[SETUP_HEADER_OFFSET] = setup_sects; // 0x1f1
    img[0x236..0x238].copy_from_slice(&1u16.to_le_bytes()); // xloadflags = XLF_KERNEL_64
    img[0x258..0x260].copy_from_slice(&u64::from(pref_address).to_le_bytes()); // pref_address
    img[0x260..0x264].copy_from_slice(&init_size.to_le_bytes()); // init_size
    img
}

proptest! {
    #![proptest_config(config(512))]

    /// Arbitrary bytes never panic the header parser.
    #[test]
    fn parse_header_total(bytes in proptest::collection::vec(any::<u8>(), 0..MAX_IMAGE)) {
        prop_assert!(matches!(linux_loader::parse_setup_header(&bytes), Ok(_) | Err(_)));
    }

    /// Arbitrary image + initramfs + RAM size never panic the loader.
    #[test]
    fn load_total(
        bytes in proptest::collection::vec(any::<u8>(), 0..MAX_IMAGE),
        initramfs in proptest::collection::vec(any::<u8>(), 0..MAX_INITRAMFS),
        ram_len in 0u64..MAX_RAM,
    ) {
        let mut ram = vec![0u8; ram_len as usize];
        prop_assert!(matches!(
            linux_loader::load(&bytes, &initramfs, ram_len, "console=ttyS0", &mut ram),
            Ok(_) | Err(_)
        ));
    }

    /// A buffer carrying the real bzImage magics at their fixed offsets, with
    /// arbitrary setup-sector count, preferred address, init_size, initramfs, and
    /// RAM size — the adversarial shape that drives `load` through every geometry
    /// check (setup tail, kernel fit, initramfs placement, page tables) — still
    /// never panics. Mixes success and the various `LinuxLoadError`s.
    #[test]
    fn valid_magic_load_total(
        setup_sects in any::<u8>(),
        pref_address in 0u32..0x0040_0000,
        init_size in 0u32..0x0040_0000,
        tail_len in 0usize..2048,
        initramfs in proptest::collection::vec(any::<u8>(), 0..MAX_INITRAMFS),
        ram_len in 0u64..MAX_RAM,
    ) {
        let img = plant_bzimage(setup_sects, pref_address, init_size, tail_len);
        // The header must parse (the magics/version/xloadflags are valid).
        prop_assert!(linux_loader::parse_setup_header(&img).is_ok());
        let mut ram = vec![0u8; ram_len as usize];
        prop_assert!(matches!(
            linux_loader::load(&img, &initramfs, ram_len, "console=ttyS0 panic=-1", &mut ram),
            Ok(_) | Err(_)
        ));
    }

    /// Truncating a valid bzImage at any length never panics (every short read is
    /// a clean error, never an OOB).
    #[test]
    fn truncated_valid_image_total(
        cut in 0usize..3072,
        ram_len in 0u64..MAX_RAM,
    ) {
        let img = plant_bzimage(1, 0x10_0000, 0x1000, 4096);
        let truncated = &img[..cut.min(img.len())];
        prop_assert!(matches!(linux_loader::parse_setup_header(truncated), Ok(_) | Err(_)));
        let mut ram = vec![0u8; ram_len as usize];
        prop_assert!(matches!(
            linux_loader::load(truncated, &[], ram_len, "x", &mut ram),
            Ok(_) | Err(_)
        ));
    }
}
