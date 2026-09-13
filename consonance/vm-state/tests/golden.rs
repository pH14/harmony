// SPDX-License-Identifier: AGPL-3.0-or-later

mod common;

use common::fully_populated;
use vm_state::VmState;

const GOLDEN_HEX: &str = "564d5331030001000d0001009000000001000000000000000200000000000000030000000000000004000000000000000500000000000000060000000000000000e0ffffff7f000000e1ffffff7f0000080000000000000009000000000000000a000000000000000b000000000000000c000000000000000d000000000000000e000000000000000f0000000000000000000081ffffffff02020000000000000200d400000000100000000000001100000008000b9b2000200000000000001200000010000b9b2000300000000000001300000018000b9b2000400000000000001400000020000b9b2000500000000000001500000028000b9b2000600000000000001600000030000b9b2000700000000000001700000038000b9b2000800000000000001800000040000b9b2000000082ffffffff7f0000000083ffffffffff0f3300058000000000efbeadde000000000000000100000000b0062000000000000000000000000000010d0000000000000009e0fe0000000003000800000007000000000000000400300000001000000000000000200000000000000030000000000000004000000000000000f00fffff000000000004000000000000050009000000010e020000000001020600010000000107002800000003000000100000008877665544332211740100000800000000000000800000c001050000000000000800080000007f1f0000aabbccdd0900180000000094357700000000000000000000000015cd5b07000000000a006c000000030000000000000003000000e803000000000000000000000000000007000000000000000000000000000000e80300000000000001000000000000000900000000000000f401000000000000d0070000000000000200000000000000030000000000000000000000000000000b000500000001020304050c00030000005566770d0020000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn from_hex(hex: &str) -> Vec<u8> {
    let hex = hex.trim();
    assert_eq!(hex.len() % 2, 0);
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16).unwrap();
            let lo = (pair[1] as char).to_digit(16).unwrap();
            ((hi << 4) | lo) as u8
        })
        .collect()
}

#[test]
fn golden_encoding_is_stable() {
    let bytes = fully_populated().encode().unwrap();
    assert_eq!(
        to_hex(&bytes),
        GOLDEN_HEX,
        "vm_state encoding drifted; if intentional, update GOLDEN_HEX and bump VM_STATE_VERSION"
    );
}

#[test]
fn golden_blob_round_trips() {
    let s = fully_populated();
    let bytes = s.encode().unwrap();
    assert_eq!(VmState::decode(&bytes), Ok(s));
}

#[test]
fn c950_v4_fixture_round_trips_with_legacy_bytes() {
    let bytes = from_hex(include_str!("fixtures/x86-extended-record.hex"));
    assert_eq!(VmState::peek_version(&bytes), Ok(4));
    let decoded = VmState::decode(&bytes).unwrap();
    assert_eq!(decoded.engine_state, [0xca, 0xfe]);
    assert_eq!(decoded.sregs.flags, 0);
    assert_eq!(decoded.sregs.pdptrs, [0; 4]);
    assert_eq!(decoded.debugregs.flags, 0);
    assert_eq!(decoded.encode().unwrap(), bytes);
}

#[test]
#[ignore = "run with --ignored --nocapture to regenerate GOLDEN_HEX"]
fn print_golden() {
    let bytes = fully_populated().encode().unwrap();
    println!("GOLDEN_HEX = \"{}\"", to_hex(&bytes));
}
