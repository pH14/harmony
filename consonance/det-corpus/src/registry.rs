// SPDX-License-Identifier: AGPL-3.0-or-later
//! The task-17 **toy** factory registry: maps a corpus item's `source` to a
//! deterministic [`ToyFactory`]. The real-VMM registry replaces this at
//! integration by handing the same generic [`crate::run_item`] a `Vmm<B>`
//! factory — no library change.
//!
//! Shared by the `det-corpus` binary and the tests so they construct byte-for-
//! byte identical factories (no fragile re-derivation of the program seed).

use unison::toy::{ToyFactory, generate_program};

/// Generated toy programs run at least this many work units before halting; a
/// run `--limit` far larger than this lets every toy program reach its terminal
/// state (required by the O3 halt check).
pub const TOY_MIN_WORK: u64 = 64;

/// FNV-1a over the bytes — a deterministic fallback program seed for a `source`
/// that is not a decimal integer.
fn stable_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Map an item's `source` to a deterministic [`ToyFactory`]: a decimal `source`
/// is the program-generator seed, anything else is hashed to one.
pub fn toy_factory(source: &str) -> ToyFactory {
    let gen_seed = source
        .parse::<u64>()
        .unwrap_or_else(|_| stable_hash(source));
    ToyFactory {
        program: generate_program(gen_seed, TOY_MIN_WORK).instrs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_hash_matches_fnv1a_vectors() {
        // Canonical FNV-1a-64 test vectors — pin the exact value (kills a
        // constant-return mutation of the body) and the XOR-then-multiply mixing
        // (kills an `^=` -> `|=`/`&=` mutation, which changes these exact results).
        assert_eq!(stable_hash(""), 0xcbf2_9ce4_8422_2325); // offset basis
        assert_eq!(stable_hash("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(stable_hash("abc"), 0xe71f_a219_0541_574b);
    }

    #[test]
    fn stable_hash_is_content_and_order_sensitive() {
        assert_ne!(stable_hash("a"), stable_hash("b"));
        assert_ne!(stable_hash("ab"), stable_hash("ac")); // content
        assert_ne!(stable_hash("ab"), stable_hash("ba")); // order
    }

    #[test]
    fn non_numeric_source_is_deterministic_and_distinct() {
        // A non-decimal source routes through stable_hash; equal sources give the
        // same program, distinct sources (almost always) differ.
        assert_eq!(toy_factory("foo").program, toy_factory("foo").program);
        assert_ne!(toy_factory("foo").program, toy_factory("bar").program);
    }
}
