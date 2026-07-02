// SPDX-License-Identifier: AGPL-3.0-or-later
//! The hand-rolled glob matcher — no `regex` (not on the task-66 whitelist).
//!
//! The DSL's string attribute predicates are globs with a single wildcard,
//! `*`, matching any (possibly empty) run of bytes; every other byte matches
//! itself. A pattern with no `*` is exact equality; a trailing `*` is the
//! prefix form.
//!
//! **Segment matching — genuinely linear, `O(pattern + text)`.** The record
//! bytes are guest-emitted, i.e. adversary-influenced in a fuzzer, so a matcher
//! with an `O(pattern · text)` worst case (the greedy two-pointer's failure
//! mode on a `*` followed by a partially-matching literal run) is a real CPU
//! DoS on the replay plane. Instead we split the pattern at `*` into literal
//! **segments**, anchor the head segment as a prefix (unless the pattern starts
//! with `*`) and the tail segment as a suffix (unless it ends with `*`), then
//! locate the interior segments greedily left-to-right with a **Knuth–Morris–Pratt**
//! substring search ([`find`]) — linear per segment, so linear overall with no
//! adversarial blowup. Greedy-leftmost placement is optimal for ordered
//! substring containment, so it never yields a false negative.
//!
//! Matching is over **bytes**, not `char`s: the DSL renders each [`Value`] to a
//! canonical byte string (see [`crate::value`]) and both pattern and text are
//! compared byte-for-byte, so it stays total on non-UTF-8 guest bytes (a reason
//! to hand-roll KMP over `[u8]` rather than lean on `str::find`, which needs
//! UTF-8). `*` matches any byte sequence — the intended "any substring"
//! semantics.

/// The wildcard byte: matches any (possibly empty) run of bytes.
const STAR: u8 = b'*';

/// Whether `text` matches the glob `pattern` (`*` = any run of bytes; every
/// other byte is literal). Linear `O(pattern + text)` via prefix/suffix
/// anchoring plus KMP interior search — total on every input, including
/// patterns that are all `*` and non-UTF-8 text.
pub fn matches(pattern: &[u8], text: &[u8]) -> bool {
    // No wildcard ⇒ exact equality (also handles the empty pattern: it matches
    // only the empty text). This is the one case both ends of the pattern are
    // the same single segment, so anchoring it once here avoids double-anchoring.
    if !pattern.contains(&STAR) {
        return pattern == text;
    }

    // The pattern has ≥1 `*` and is non-empty. Split into maximal non-empty
    // literal runs; consecutive/edge `*`s just drop empty pieces.
    let leading_star = pattern[0] == STAR;
    let trailing_star = pattern.last() == Some(&STAR);
    let segments: Vec<&[u8]> = pattern
        .split(|&b| b == STAR)
        .filter(|s| !s.is_empty())
        .collect();

    // All `*` (no literal runs) ⇒ matches anything.
    if segments.is_empty() {
        return true;
    }

    let n = segments.len();
    let mut lo = 0usize; // first text index still available to interior segments
    let mut hi = text.len(); // exclusive upper bound (before any anchored tail)

    // Anchor the head as a prefix unless the pattern opens with `*`.
    let first_interior = if leading_star {
        0
    } else {
        let head = segments[0];
        if text.len() < head.len() || &text[..head.len()] != head {
            return false;
        }
        lo = head.len();
        1
    };

    // Anchor the tail as a suffix unless the pattern ends with `*`. The guard
    // `hi < lo + tail.len()` rejects a tail that would overlap the anchored head
    // (e.g. `aa*aa` vs `aaa`).
    let last_interior = if trailing_star {
        n
    } else {
        let tail = segments[n - 1];
        if hi < lo + tail.len() || &text[hi - tail.len()..] != tail {
            return false;
        }
        hi -= tail.len();
        n - 1
    };

    // Locate each interior segment greedily in the shrinking window `[lo, hi)`.
    // `.get(..)` yields an empty slice when the range is degenerate (it never is
    // for a valid pattern, but this stays panic-free regardless).
    for seg in segments.get(first_interior..last_interior).unwrap_or(&[]) {
        match find(&text[lo..hi], seg) {
            Some(i) => lo += i + seg.len(),
            None => return false,
        }
    }
    true
}

/// The leftmost index of `needle` in `haystack`, or `None` — Knuth–Morris–Pratt,
/// `O(haystack + needle)` with no quadratic blowup on adversarial repeats (the
/// property the segment matcher needs to stay linear on guest bytes).
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    // Failure function: `lps[i]` = length of the longest proper prefix of
    // `needle[..=i]` that is also a suffix of it.
    let mut lps = vec![0usize; needle.len()];
    let mut len = 0usize;
    let mut i = 1usize;
    while i < needle.len() {
        if needle[i] == needle[len] {
            len += 1;
            lps[i] = len;
            i += 1;
        } else if len > 0 {
            len = lps[len - 1];
        } else {
            lps[i] = 0;
            i += 1;
        }
    }
    // Scan the haystack, never re-examining a matched byte.
    let mut q = 0usize;
    for (h, &c) in haystack.iter().enumerate() {
        while q > 0 && c != needle[q] {
            q = lps[q - 1];
        }
        if c == needle[q] {
            q += 1;
        }
        if q == needle.len() {
            return Some(h + 1 - needle.len());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{STAR, find, matches};
    use proptest::prelude::*;

    /// A naive recursive reference the segment matcher must agree with; it is
    /// intentionally simple (and exponential) so it is obviously correct.
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

    /// KMP `find` agrees with a naive scan and terminates on adversarial repeats.
    #[test]
    fn kmp_find_locates_leftmost() {
        assert_eq!(find(b"abcabcd", b"abcd"), Some(3));
        assert_eq!(find(b"aaaaa", b"aab"), None);
        assert_eq!(find(b"aaab", b"aaab"), Some(0));
        assert_eq!(find(b"xyz", b""), Some(0)); // empty needle
        assert_eq!(find(b"ab", b"abc"), None); // needle longer than haystack
        assert_eq!(find(b"abababc", b"ababc"), Some(2)); // KMP failure-fn path
    }

    /// Regression (codex round-2 P2 #2): guest-emitted (adversary-influenced)
    /// text must not trigger a CPU DoS. With segment matching + KMP the work is
    /// `O(pattern + text)`, so these large pathological inputs complete
    /// near-instantly; the old `O(pattern · text)` two-pointer would grind (a
    /// naive interior scan of the 4 KiB needle across the run would be ~10^9+
    /// byte-compares). Completion *is* the tightened bound.
    #[test]
    fn segment_matching_stays_linear_on_pathological_input() {
        let text = vec![b'a'; 1_000_000];
        // Tail-anchored: `*aaaab` fails at the suffix literal in O(1).
        assert!(!matches(b"*aaaab", &text));
        // Interior segment (both ends starred) → the KMP path. A 4 KiB run of
        // `a` then `b`, searched in a megabyte of `a`, finds no `b`: linear scan,
        // no match. A quadratic matcher would not return in time.
        let mut interior = vec![b'*'];
        interior.extend(std::iter::repeat_n(b'a', 4096));
        interior.push(b'b');
        interior.push(b'*');
        assert!(!matches(&interior, &text));
        // The matching variants terminate just as fast.
        let mut hay = vec![b'a'; 1_000_000];
        hay.push(b'b');
        assert!(matches(b"*aaaab", &hay));
        assert!(matches(&interior, &hay));
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

        /// Gate 5: the segment matcher agrees with the naive reference on random
        /// pattern/text pairs, and never panics — including on patterns that are
        /// dense runs of `*` (the anchoring / interior-search edge cases).
        #[test]
        fn segment_matcher_agrees_with_reference(pat in glob_bytes(), text in text_bytes()) {
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
