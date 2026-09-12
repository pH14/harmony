// SPDX-License-Identifier: AGPL-3.0-or-later

use proptest::prelude::*;
use vmm_core::vendor::x86::linux_loader::{self, SETUP_HEADER_OFFSET};

const MAX_IMAGE: usize = if cfg!(miri) { 256 } else { 8192 };
const MAX_INITRAMFS: usize = if cfg!(miri) { 64 } else { 4096 };
const MAX_RAM: u64 = if cfg!(miri) { 1 << 14 } else { 8 << 20 };

fn config(native: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 16 } else { native });
    if cfg!(miri) {
        cfg.failure_persistence = None;
    }
    cfg
}

fn plant_bzimage(setup_sects: u8, pref_address: u32, init_size: u32, tail_len: usize) -> Vec<u8> {
    let real_sects = if setup_sects == 0 { 4 } else { setup_sects };
    let pm_off = (usize::from(real_sects) + 1) * 512;
    let mut img = vec![0u8; pm_off + tail_len];
    img[0x1fe..0x200].copy_from_slice(&0xAA55u16.to_le_bytes());
    img[0x202..0x206].copy_from_slice(&0x5372_6448u32.to_le_bytes());
    img[0x206..0x208].copy_from_slice(&0x020fu16.to_le_bytes());
    img[SETUP_HEADER_OFFSET] = setup_sects;
    img[0x236..0x238].copy_from_slice(&1u16.to_le_bytes());
    img[0x258..0x260].copy_from_slice(&u64::from(pref_address).to_le_bytes());
    img[0x260..0x264].copy_from_slice(&init_size.to_le_bytes());
    img
}

proptest! {
    #![proptest_config(config(512))]

    #[test]
    fn parse_header_total(bytes in proptest::collection::vec(any::<u8>(), 0..MAX_IMAGE)) {
        prop_assert!(matches!(linux_loader::parse_setup_header(&bytes), Ok(_) | Err(_)));
    }

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
        prop_assert!(linux_loader::parse_setup_header(&img).is_ok());
        let mut ram = vec![0u8; ram_len as usize];
        prop_assert!(matches!(
            linux_loader::load(&img, &initramfs, ram_len, "console=ttyS0 panic=-1", &mut ram),
            Ok(_) | Err(_)
        ));
    }

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
