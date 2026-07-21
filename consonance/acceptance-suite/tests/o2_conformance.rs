// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 2 — O2 conformance: matches the golden of an actual run; any other hex
//! fails with a mismatch detail; malformed/short hex is a `Fail`, never a panic.

use acceptance_suite::check_conformance;
use proptest::prelude::*;
use unison::toy::{ToyFactory, asm, generate_program};
use unison::{Subject, SubjectFactory};

/// Lowercase 64-char hex of the terminal observable_digest at `seed` (the O2
/// conformance signal — see `check_conformance`).
fn golden_for(f: &ToyFactory, seed: u64, limit: u64) -> String {
    let mut m = f.spawn(seed);
    m.run_to(limit).unwrap();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for b in m.observable_digest() {
        s.push(char::from(HEX[usize::from(b >> 4)]));
        s.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    s
}

#[test]
fn flip_one_nibble_of_golden_fails() {
    let f = ToyFactory {
        program: vec![asm::loadi(0, 7), asm::out(0), asm::halt()],
    };
    let golden = golden_for(&f, 1, 1000);
    // Matching golden passes.
    let ok = check_conformance(&f, 1, 1000, &golden).unwrap();
    assert!(ok.passed, "{ok:?}");
    // Flip the first nibble: still 64 valid hex chars, but a mismatch.
    let mut bad: Vec<char> = golden.chars().collect();
    bad[0] = if bad[0] == '0' { '1' } else { '0' };
    let bad: String = bad.into_iter().collect();
    let res = check_conformance(&f, 1, 1000, &bad).unwrap();
    assert!(!res.passed);
    assert!(res.detail.contains("mismatch"), "{}", res.detail);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The golden computed from the run itself always matches; any *different*
    /// 64-hex string fails.
    #[test]
    fn matches_own_golden_rejects_others(
        prog_seed in any::<u64>(),
        seed in any::<u64>(),
        other in proptest::array::uniform32(any::<u8>()),
    ) {
        let f = ToyFactory { program: generate_program(prog_seed, 200).instrs };
        let limit = 100_000u64;
        let golden = golden_for(&f, seed, limit);

        let good = check_conformance(&f, seed, limit, &golden).unwrap();
        prop_assert!(good.passed, "own golden must match: {good:?}");
        prop_assert!(good.divergence.is_none());

        // A random 32-byte digest as hex: passes only in the (cryptographically
        // impossible) case it equals the real hash.
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut other_hex = String::with_capacity(64);
        for b in other {
            other_hex.push(char::from(HEX[usize::from(b >> 4)]));
            other_hex.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
        let res = check_conformance(&f, seed, limit, &other_hex).unwrap();
        prop_assert_eq!(res.passed, other_hex == golden);
    }

    /// Malformed / short / overlong / non-hex goldens are a `Fail`, never a panic.
    #[test]
    fn malformed_hex_is_fail_not_panic(
        prog_seed in any::<u64>(),
        seed in any::<u64>(),
        junk in "\\PC{0,80}",
    ) {
        let f = ToyFactory { program: generate_program(prog_seed, 200).instrs };
        let golden = golden_for(&f, seed, 100_000);
        // `junk` is almost never a 64-char hex string equal to the golden; if it
        // happens to be exactly the golden, passing is correct.
        let res = check_conformance(&f, seed, 100_000, &junk).unwrap();
        let trimmed = junk.trim();
        let is_real_match = trimmed.len() == 64
            && trimmed.eq_ignore_ascii_case(&golden)
            && trimmed.bytes().all(|c| c.is_ascii_hexdigit());
        prop_assert_eq!(res.passed, is_real_match);
    }
}
