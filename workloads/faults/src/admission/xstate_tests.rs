// SPDX-License-Identifier: AGPL-3.0-or-later

use super::tests::elf;
use super::*;

fn hex(text: &str) -> Vec<u8> {
    text.split_whitespace()
        .flat_map(|part| {
            part.as_bytes()
                .chunks_exact(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        })
        .collect()
}

fn resolver(save: &[u8]) -> Vec<u8> {
    let mut code = hex(
        "f30f1efa 53 4889e3 4883e4c0 482b2500000000 48890424 48894c2408 4889542410 4889742418 48897c2420 4c89442428 4c894c2430 b8ee000800 31d2",
    );
    for n in 0..8_u32 {
        code.extend(hex("48899424"));
        code.extend((0x240 + n * 8).to_le_bytes());
    }
    code.extend(save);
    code.extend(hex("488b7310 488b7b08"));
    let tail = hex(
        "4989c3 b8ee000800 31d2 0fae6c2440 4c8b4c2430 4c8b442428 488b7c2420 488b742418 488b542410 488b4c2408 488b0424 4889dc 488b1c24 4883c418 41ffe3",
    );
    code.push(0xe8);
    code.extend((tail.len() as i32).to_le_bytes());
    code.extend(tail);
    code.push(0xc3);
    code
}

fn accepted(code: &[u8]) -> bool {
    scan_elf(&elf(code, 5, 6), "fixture").is_ok()
}

#[test]
fn save_families_are_forbidden_even_when_the_image_site_is_reviewed() {
    for instruction in [
        "0fae07", "480fae07", "0fae27", "480fae27", "0fc727", "480fc727", "0fae37", "480fae37",
        "0fc72f", "480fc72f",
    ] {
        let code = hex(&format!("{instruction} c3"));
        let bytes = elf(&code, 5, 6);
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("probe"), &bytes).unwrap();
        fs::create_dir_all(root.path().join("etc/harmony")).unwrap();
        let instruction =
            iced_x86::Decoder::with_ip(64, &code, 0x1000, DecoderOptions::NONE).decode();
        fs::write(
            root.path().join(ALLOWLIST_PATH),
            format!(
                "{:x} 0x1000 {} reviewed\n",
                Sha256::digest(&bytes),
                xstate::name(&instruction).unwrap()
            ),
        )
        .unwrap();
        let report = inspect_root(root.path()).unwrap();
        assert!(
            report
                .scan_errors
                .iter()
                .any(|e| e.contains("save instruction outside a loader trampoline")),
            "{report:?}"
        );
        for mode in [ExecutionMode::Vm, ExecutionMode::Uml] {
            assert!(report.require_for(mode).is_err());
        }
        let staged = oci_support::image::StagedImage {
            rootfs: root.path().to_path_buf(),
            owners: oci_support::image::Ownership::default(),
            config: oci_support::image::RuntimeConfig::default(),
        };
        let packed =
            oci_support::bundle::prepare(&staged, &oci_support::bundle::LaunchRequest::default())
                .unwrap();
        let archive = packed.archived_rootfs().unwrap();
        let archived = ArchivedImage::new(&archive).inspect().unwrap();
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            serde_json::to_value(&archived).unwrap()
        );
    }
}

#[test]
fn xgetbv_requires_zero_selector_and_an_unambiguous_straight_line() {
    for (code, expect) in [
        ("31c9 0f01d0 c3", true),
        ("b900000000 0f01d0 c3", true),
        ("31c9 4889c3 4883c301 0f01d0 c3", true),
        ("b901000000 0f01d0 c3", false),
        ("0f01d0 c3", false),
        ("31c9 b101 0f01d0 c3", false),
        ("31c9 b501 0f01d0 c3", false),
        ("31c9 66b90100 0f01d0 c3", false),
        ("31c9 48c7c101000000 0f01d0 c3", false),
        ("eb02 31c9 0f01d0 c3", false),
        ("7401 8031c9 0f01d0 c3", false),
        ("31c9 e800000000 0f01d0 c3", false),
    ] {
        assert_eq!(accepted(&hex(code)), expect, "{code}");
    }
    let mut bytes = elf(&hex("b8 0fae27 c3 c3"), 5, 6);
    bytes[24..32].copy_from_slice(&0x1001_u64.to_le_bytes());
    assert!(
        scan_elf(&bytes, "fixture")
            .unwrap_err()
            .to_string()
            .contains("save instruction")
    );
}

#[test]
fn exact_loader_resolver_shape_is_allowed_but_entry_or_mask_changes_are_not() {
    for save in [hex("0fae642440"), hex("0fc7642440"), hex("480fae642440")] {
        let code = resolver(&save);
        assert!(accepted(&code));
        let mut branch = hex("e900000000");
        let target = code
            .windows(save.len())
            .position(|window| window == save)
            .unwrap();
        branch[1..5].copy_from_slice(&(target as i32).to_le_bytes());
        branch.extend(&code);
        assert!(!accepted(&branch));
        let mut wrong_mask = code.clone();
        let mask = hex("b8ee000800");
        let at = wrong_mask
            .windows(mask.len())
            .rposition(|w| w == mask)
            .unwrap();
        wrong_mask[at + 1] ^= 1;
        assert!(!accepted(&wrong_mask));
    }
    assert!(!accepted(&resolver(&hex("0fae442440"))));
    assert!(!accepted(&resolver(&hex("0fae642448"))));
    let mut overlap = hex("7401 b8");
    overlap.extend(resolver(&hex("0fae642440")));
    assert!(!accepted(&overlap));
}

#[test]
fn restore_operations_remain_admitted_without_a_save_exemption() {
    for code in [
        "0fae0f c3",
        "480fae0f c3",
        "0fae2f c3",
        "480fae2f c3",
        "0fc71f c3",
        "480fc71f c3",
    ] {
        assert!(accepted(&hex(code)), "{code}");
    }
}

#[test]
fn unused_tlsdesc_shape_requires_matching_masks_and_complete_reload_tail() {
    let mut code =
        hex("48895c24e8 4889e3 4883e4c0 482b2500000000 48890424 b8ee000800 31d2 0fae642440");
    let tail =
        hex("4889c1 b8ee000800 31d2 0fae6c2440 4889c8 488b0424 4889dc 488b5c24e8 e900000000");
    code.push(0xe8);
    code.extend((tail.len() as i32).to_le_bytes());
    code.extend(&tail);
    code.push(0xc3);
    assert!(accepted(&code));
    let mut changed = code.clone();
    let mask = hex("b8ee000800");
    let at = changed
        .windows(mask.len())
        .rposition(|w| w == mask)
        .unwrap();
    changed[at + 1] ^= 1;
    assert!(!accepted(&changed));
    let reload = hex("488b0424");
    let at = code
        .windows(reload.len())
        .position(|w| w == reload)
        .unwrap();
    code[at..at + reload.len()].fill(0x90);
    assert!(!accepted(&code));
}

fn dynamic(tags: &[(u64, u64)], relocation: u32) -> Vec<u8> {
    let mut code = vec![0; 128];
    code[0] = 0xc3;
    code[72..80].copy_from_slice(&u64::from(relocation).to_le_bytes());
    let mut bytes = elf(&code, 5, 6);
    let sections = bytes[192..384].to_vec();
    bytes[512..704].copy_from_slice(&sections);
    bytes[192..384].fill(0);
    bytes[40..48].copy_from_slice(&512_u64.to_le_bytes());
    bytes[56..58].copy_from_slice(&3_u16.to_le_bytes());
    bytes[176..180].copy_from_slice(&object::elf::PT_DYNAMIC.to_le_bytes());
    bytes[184..192].copy_from_slice(&768_u64.to_le_bytes());
    let size = ((tags.len() + 1) * 16) as u64;
    bytes[208..216].copy_from_slice(&size.to_le_bytes());
    bytes[216..224].copy_from_slice(&size.to_le_bytes());
    for (n, &(tag, value)) in tags.iter().enumerate() {
        bytes[768 + n * 16..776 + n * 16].copy_from_slice(&tag.to_le_bytes());
        bytes[776 + n * 16..784 + n * 16].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[test]
fn tlsdesc_and_text_relocations_cannot_invalidate_loader_exemptions() {
    for (tags, relocation, expected) in [
        (vec![(7, 0x1040), (8, 24)], 8, true),
        (vec![(7, 0x1040), (8, 24)], 34, false),
        (vec![(17, 0x1040), (18, 16)], 35, false),
        (vec![(23, 0x1040), (2, 24), (20, 7)], 36, false),
        (vec![(0x6ffffef6, 0)], 0, false),
        (vec![(0x6ffffef7, 0)], 0, false),
        (vec![(22, 0)], 0, false),
        (vec![(30, 4)], 0, false),
        (vec![(7, 0x9000), (8, 24)], 0, false),
        (vec![(7, 0x1040)], 0, false),
        (vec![(7, 0x1040), (8, 23)], 0, false),
    ] {
        let bytes = dynamic(&tags, relocation);
        assert_eq!(
            scan_elf(&bytes, "fixture").is_ok(),
            expected,
            "{tags:?} {relocation}"
        );
    }
}
