// SPDX-License-Identifier: AGPL-3.0-or-later
//! The hand-rolled glob matcher — no `regex` (not on the task-66 whitelist).
//!
//! The DSL's string attribute predicates are globs with a single wildcard,
//! `*`, matching any (possibly empty) run of bytes; every other byte matches
//! itself. A pattern with no `*` is exact equality; a trailing `*` is the
//! prefix form. Matching runs the **standard two-pointer star-backtracking**
//! algorithm the task spec names ("the linear-time two-pointer algorithm"): it
//! advances a pointer into the pattern and one into the text, remembering the
//! most recent `*` (and the text position to resume from) to fall back to on a
//! literal mismatch.
//!
//! **Complexity, stated precisely.** The load-bearing guarantee is **no
//! catastrophic backtracking** — unlike the naive recursive matcher, a `*`
//! never spawns exponential work; a run of `*`s collapses. The strict worst
//! case is `O(pattern · text)` (a `*` followed by a literal run that repeatedly
//! partially matches re-scans that run per text byte), *not* strict linear
//! time — but with tiny constants, and genuinely linear on the short attribute
//! patterns the DSL actually matches. A true `O(pattern + text)` matcher would
//! need KMP/Z per `*`-delimited segment, which is disproportionate here; the
//! pathological-input regression test proves the `O(pattern · text)` case
//! completes near-instantly (no blowup), and the proptest pins agreement with a
//! naive reference on 512+ random and adversarial pairs.
//!
//! Matching is over **bytes**, not `char`s: the DSL renders each [`Value`] to a
//! canonical byte string (see [`crate::value`]) and both pattern and text are
//! compared byte-for-byte. `*` therefore matches any byte sequence, which is the
//! intended "any substring" semantics and stays total on non-UTF-8 bytes.

/// The wildcard byte: matches any (possibly empty) run of bytes.
const STAR: u8 = b'*';

/// Whether `text` matches the glob `pattern` (`*` = any run of bytes; every
/// other byte is literal). Single left-to-right scan with `*`-backtracking: no
/// recursion, no exponential blowup — total on every input, including patterns
/// that are all `*`.
pub fn matches(pattern: &[u8], text: &[u8]) -> bool {
    // Two cursors plus a remembered `*` fallback: `star` is the pattern index
    // just past the last `*` seen, `resume` is the text index to retry from
    // after that `*` consumes one more byte. `None` means "no `*` to fall back
    // to yet", so a literal mismatch is fatal.
    let mut p = 0usize;
    let mut t = 0usize;
    let mut star: Option<usize> = None;
    let mut resume = 0usize;

    while t < text.len() {
        if p < pattern.len() && pattern[p] == text[t] && pattern[p] != STAR {
            // A literal byte matched: advance both cursors.
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == STAR {
            // A `*`: let it match the empty run for now, but remember where to
            // fall back so it can extend one byte at a time on later mismatch.
            star = Some(p + 1);
            resume = t;
            p += 1;
        } else if let Some(after_star) = star {
            // Mismatch under an open `*`: extend the `*` by one text byte.
            p = after_star;
            resume += 1;
            t = resume;
        } else {
            // Mismatch with no `*` to absorb it.
            return false;
        }
    }

    // Text exhausted: the remaining pattern must be all `*` to match.
    while p < pattern.len() && pattern[p] == STAR {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::{STAR, matches};
    use proptest::prelude::*;

    /// A naive recursive reference the two-pointer matcher must agree with; it
    /// is intentionally simple (and exponential) so it is obviously correct.
    fn reference(pattern: &[u8], text: &[u8]) -> bool {
        match pattern.first() {
            None => text.is_empty(),
            Some(&b'*') => {
                // `*` matches the empty run, or one byte then retry.
                reference(&pattern[1..], text)
                    || (!text.is_empty() && reference(pattern, &text[1..]))
            }
            Some(&c) => !text.is_empty() && text[0] == c && reference(&pattern[1..], &text[1..]),
        }
    }

    #[test]
    fn exact_prefix_and_wildcard_forms() {
        // No `*` → exact equality.
        assert!(matches(b"won", b"won"));
        assert!(!matches(b"won", b"wo"));
        assert!(!matches(b"won", b"wone"));
        // Trailing `*` → prefix form.
        assert!(matches(
            b"database system is ready*",
            b"database system is ready to accept"
        ));
        assert!(matches(b"ready*", b"ready"));
        assert!(!matches(b"ready*", b"read"));
        // Leading / interior `*`.
        assert!(matches(b"*ready", b"already"));
        assert!(matches(b"a*c", b"abbbc"));
        assert!(matches(b"a*c", b"ac"));
        assert!(!matches(b"a*c", b"ab"));
        // Bare `*` matches anything, including the empty string.
        assert!(matches(b"*", b""));
        assert!(matches(b"*", b"anything at all"));
        assert!(matches(b"", b""));
        assert!(!matches(b"", b"x"));
    }

    #[test]
    fn pathological_star_runs_do_not_blow_up() {
        // Runs of `*` collapse; no exponential backtracking.
        let pat = vec![b'*'; 64];
        assert!(matches(&pat, &vec![b'a'; 4096]));
        let mut pat2 = vec![b'*'; 32];
        pat2.push(b'z');
        assert!(matches(&pat2, b"aaaaaaaaaaaaaaaaz"));
        assert!(!matches(&pat2, b"aaaaaaaaaaaaaaaay"));
        // The classic adversarial case for naive matchers: `a…a*…*b` vs `a…a`.
        let mut adversary = vec![b'a'; 20];
        adversary.extend(std::iter::repeat_n(b'*', 20));
        adversary.push(b'b');
        assert!(!matches(&adversary, &[b'a'; 40]));
    }

    /// Regression (codex P2): the `*` + literal-run + terminal-byte pattern
    /// against a long run of the run-byte — the two-pointer's `O(pattern · text)`
    /// worst case. It must complete near-instantly (bounded polynomial work, no
    /// blowup); if it were exponential this test would never return.
    #[test]
    fn star_then_literal_run_completes_fast() {
        // `*aaaab` vs `aaaa…a` (50_000 a's, no trailing b): the star repeatedly
        // rescans the 4-byte `aaaa` run then fails at `b` — ~O(5 · 50_000) work.
        let pattern = b"*aaaab";
        let text = vec![b'a'; 50_000];
        assert!(!matches(pattern, &text));
        // And the matching variant terminates just as fast.
        let mut matching = vec![b'a'; 50_000];
        matching.push(b'b');
        assert!(matches(pattern, &matching));
    }

    #[test]
    fn agrees_with_reference_on_a_grid() {
        // A small exhaustive-ish grid over {a, b, *} agrees with the reference.
        let alphabet = [b'a', b'b', STAR];
        let words = [b"", b"a".as_slice(), b"ab", b"ba", b"aab", b"bba", b"abab"];
        for pl in 0..=3usize {
            let mut idx = vec![0usize; pl];
            loop {
                let pat: Vec<u8> = idx.iter().map(|&i| alphabet[i]).collect();
                for w in &words {
                    assert_eq!(
                        matches(&pat, w),
                        reference(&pat, w),
                        "pattern {pat:?} text {w:?}"
                    );
                }
                // Odometer over the pattern alphabet.
                let mut k = 0;
                while k < pl {
                    idx[k] += 1;
                    if idx[k] < alphabet.len() {
                        break;
                    }
                    idx[k] = 0;
                    k += 1;
                }
                if k == pl {
                    break;
                }
            }
        }
    }

    /// A byte string over a tiny alphabet `{a, b, *}` — dense in `*` so runs and
    /// interleavings (the adversarial cases) are common.
    fn glob_bytes() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(prop::sample::select(vec![b'a', b'b', STAR]), 0..=12)
    }

    /// Text over `{a, b}` (no `*`, since `*` is only a pattern metacharacter,
    /// though the matcher stays total if a text byte happens to be `*`).
    fn text_bytes() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(prop::sample::select(vec![b'a', b'b']), 0..=12)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Gate 5: the two-pointer matcher agrees with the naive reference on
        /// random pattern/text pairs, and never panics — including on patterns
        /// that are dense runs of `*`.
        #[test]
        fn two_pointer_agrees_with_reference(pat in glob_bytes(), text in text_bytes()) {
            prop_assert_eq!(matches(&pat, &text), reference(&pat, &text));
        }

        /// Even with arbitrary bytes on both sides (text may contain `*`), the
        /// matcher stays total and agrees with the reference.
        #[test]
        fn total_on_arbitrary_bytes(pat in prop::collection::vec(any::<u8>(), 0..=16),
                                    text in prop::collection::vec(any::<u8>(), 0..=16)) {
            prop_assert_eq!(matches(&pat, &text), reference(&pat, &text));
        }
    }
}
