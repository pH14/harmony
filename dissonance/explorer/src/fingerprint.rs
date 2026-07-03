// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pinned, versioned **`Bug` fingerprint** schema (task 75) — shared by every
//! oracle so a finding dedups across the many environments that reach it.
//!
//! A fingerprint is a `sha2` digest over a canonical (length-prefixed,
//! BTree-ordered) encoding of three **stable coordinates**:
//!
//! 1. [`TerminalSig`] — the oracle's own terminal signature: its stable id, an
//!    anomaly class, a *normalized* detail (a participating-key set, an assertion
//!    id, a crash-marker class — never a raw address), and the terminal
//!    [`StopReason`](crate::StopReason) discriminant.
//! 2. [`FaultCoord`] — the fault set the finding rode. At mint this is a set of
//!    opaque *plane+class* fault tokens supplied by a **schema-aware** caller
//!    (the campaign, via the `environment` codec's fault projection); task 76
//!    recanonicalizes it to the LDFI individually-necessary set. A **pure trace
//!    oracle** over an opaque [`Environment`](crate::Environment) cannot
//!    enumerate faults, so it mints [`FaultCoord::none`] — see the deviation note
//!    below.
//! 3. [`VTimeCoord`] — the *quantized* V-time of the earliest violating op (or
//!    the terminal). Task 76 replaces the fixed bracket with the
//!    earliest-divergence (inevitability) bracket.
//!
//! ## Provisional at mint, canonical after triage
//!
//! Mint-time fingerprints are tagged **provisional**: they **over-split by
//! design** (Igor's ordering — minimize first, then dedup; task 76
//! recanonicalizes coordinates 2–3). Two supersessions land here: task 12's
//! stop-reason-only digest ([`TerminalOracle`](crate::TerminalOracle)) and task
//! 66's `MatchOracle` scheme both re-mint through [`mint`].
//!
//! **Forbidden in the digest at both stages:** any learned/codebook feature
//! ([`CellKey`](crate::CellKey)s drift — cells are triage *grouping* only, never
//! identity) and coverage/stack hashes (Klees et al.: they actively miscount).
//!
//! ## Deviation considered and taken: the schema-blind fault coordinate
//!
//! Coordinate 2 is described as "the set of fault-classed `Action`s in `env`
//! (plane + class, never `Moment`s)". Extracting that needs the task-24 schema,
//! which a *pure trace oracle* (judging only a [`RunTrace`](crate::RunTrace) with
//! an **opaque** [`Environment`](crate::Environment)) does not have. Two options
//! were weighed:
//!
//! - *Hash the opaque `env` blob as the fault token.* Rejected: the blob carries
//!   `Moment`s (violating "never `Moment`s") and over-splits so hard it defeats
//!   even mint-time grouping — every distinct reproducer becomes its own bug.
//! - *Emit [`FaultCoord::none`] and keep the coordinate first-class* so a
//!   schema-aware caller (and task 76) populates it. **Taken.** It matches the
//!   existing provisional sites (task 66's `never_fingerprint` carries no fault
//!   coordinate either) and preserves mint-time dedup grouping. The `FaultCoord`
//!   input is a real parameter, so the campaign's schema-aware minting path adds
//!   the plane+class set with zero API change.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::StopReason;
use crate::spine::Moment;

/// The fingerprint schema's domain-separation tag. The `v2` supersedes task 12's
/// `dissonance.explorer.bug.v1` stop-only digest; a future scheme bump changes
/// this tag so old and new digests can never collide.
pub const FINGERPRINT_DOMAIN: &[u8] = b"dissonance.oracle.fingerprint.v2";

/// The provisional mint's V-time quantization bracket, in `Moment` units: the
/// [`VTimeCoord`] is `moment / FINGERPRINT_VTIME_BRACKET`. Coarse enough that the
/// same finding at a slightly different V-time collapses, fine enough to keep
/// distant findings apart. Task 76 replaces it with the inevitability bracket.
pub const FINGERPRINT_VTIME_BRACKET: u64 = 64;

/// **Coordinate 1** — an oracle's terminal signature. Every field is a *stable
/// coordinate*: no raw addresses, no learned features. The `detail` bytes must be
/// **canonical** (deterministically ordered — e.g. a sorted key set), because
/// they are hashed verbatim.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TerminalSig {
    /// The minting oracle's stable id (e.g. `"terminal"`, `"elle"`).
    pub oracle: String,
    /// The anomaly class within that oracle (its own stable enumeration).
    pub class: u32,
    /// Normalized, **canonical** detail bytes: a participating-key set, an
    /// assertion id, a crash-marker class. No raw addresses.
    pub detail: Vec<u8>,
    /// The terminal [`StopReason`](crate::StopReason) discriminant
    /// ([`StopReason::discriminant`]).
    pub stop: u8,
}

impl TerminalSig {
    /// A terminal signature with empty detail — the caller sets [`detail`](Self::detail)
    /// (canonically) when the finding participates in specific keys/ops.
    pub fn new(oracle: impl Into<String>, class: u32, stop: u8) -> Self {
        Self {
            oracle: oracle.into(),
            class,
            detail: Vec::new(),
            stop,
        }
    }

    /// Set the canonical detail bytes (builder form).
    pub fn with_detail(mut self, detail: Vec<u8>) -> Self {
        self.detail = detail;
        self
    }
}

/// **Coordinate 2** — the fault coordinate: a canonical set of opaque *plane+class*
/// fault tokens the finding rode. Deterministically ordered (a `BTreeSet`), so no
/// iteration order reaches the digest. A **pure trace oracle** mints
/// [`FaultCoord::none`]; a schema-aware caller supplies the decoded set via
/// [`FaultCoord::from_tokens`].
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct FaultCoord {
    tokens: BTreeSet<Vec<u8>>,
}

impl FaultCoord {
    /// The empty fault coordinate — the provisional mint of a schema-blind oracle
    /// (see the module deviation note). Distinct findings still split on
    /// coordinates 1 and 3.
    pub fn none() -> Self {
        Self::default()
    }

    /// The fault coordinate from a schema-aware caller's decoded plane+class
    /// tokens (each an opaque, stable byte encoding; `Moment`s excluded by the
    /// caller). Deduplicated and canonically ordered.
    pub fn from_tokens<I, T>(tokens: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<Vec<u8>>,
    {
        Self {
            tokens: tokens.into_iter().map(Into::into).collect(),
        }
    }

    /// Whether the coordinate carries no fault tokens (the provisional mint).
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// The number of distinct fault tokens.
    pub fn len(&self) -> usize {
        self.tokens.len()
    }
}

/// **Coordinate 3** — the quantized V-time of the earliest violating op (or the
/// terminal). Construct with [`VTimeCoord::quantize`]; the inner value is the
/// bracket index, not a raw `Moment`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct VTimeCoord(pub u64);

impl VTimeCoord {
    /// Quantize a [`Moment`] into its provisional-mint bracket
    /// (`moment / FINGERPRINT_VTIME_BRACKET`). Integer-only (conventions rule 4).
    pub fn quantize(at: Moment) -> Self {
        Self(at.0 / FINGERPRINT_VTIME_BRACKET)
    }
}

/// Mint a `Bug` fingerprint from the three stable coordinates: a versioned
/// `sha2` digest over their canonical (length-prefixed, BTree-ordered) encoding.
/// Deterministic — equal coordinates yield byte-equal digests, whatever the host.
pub fn mint(sig: &TerminalSig, faults: &FaultCoord, vtime: VTimeCoord) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(FINGERPRINT_DOMAIN);

    // Coordinate 1: the terminal signature.
    h.update([0x01]);
    write_bytes(&mut h, sig.oracle.as_bytes());
    h.update(sig.class.to_le_bytes());
    write_bytes(&mut h, &sig.detail);
    h.update([sig.stop]);

    // Coordinate 2: the fault set, canonically ordered (BTreeSet iteration is
    // sorted, so no order leaks into the digest).
    h.update([0x02]);
    h.update((faults.tokens.len() as u64).to_le_bytes());
    for tok in &faults.tokens {
        write_bytes(&mut h, tok);
    }

    // Coordinate 3: the quantized V-time bracket.
    h.update([0x03]);
    h.update(vtime.0.to_le_bytes());

    h.finalize().into()
}

/// Absorb a length-prefixed byte string, so no `(a, b)` split can alias another
/// `(a', b')` with the same concatenation.
fn write_bytes(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

impl StopReason {
    /// The stable discriminant byte for this stop, for the fingerprint's
    /// coordinate 1. Fixed per variant; a reordering of the enum must not change
    /// these (they are a wire-stable coordinate, not the enum's memory layout).
    pub fn discriminant(&self) -> u8 {
        match self {
            StopReason::Deadline { .. } => 0,
            StopReason::Quiescent { .. } => 1,
            StopReason::Crash { .. } => 2,
            StopReason::Decision { .. } => 3,
            StopReason::Assertion { .. } => 4,
            StopReason::SnapshotPoint { .. } => 5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VTime;

    fn sig() -> TerminalSig {
        TerminalSig::new("elle", 3, 1).with_detail(b"key-set".to_vec())
    }

    /// The mint is deterministic: same coordinates → byte-equal digest.
    #[test]
    fn mint_is_deterministic() {
        let a = mint(
            &sig(),
            &FaultCoord::none(),
            VTimeCoord::quantize(Moment(120)),
        );
        let b = mint(
            &sig(),
            &FaultCoord::none(),
            VTimeCoord::quantize(Moment(120)),
        );
        assert_eq!(a, b);
        assert_ne!(a, [0u8; 32]);
        assert_ne!(a, [1u8; 32]);
    }

    /// Each coordinate is load-bearing: changing any one changes the digest.
    #[test]
    fn each_coordinate_moves_the_digest() {
        let base = mint(
            &sig(),
            &FaultCoord::none(),
            VTimeCoord::quantize(Moment(120)),
        );

        // Coordinate 1 fields.
        let mut s = sig();
        s.oracle = "terminal".into();
        assert_ne!(
            base,
            mint(&s, &FaultCoord::none(), VTimeCoord::quantize(Moment(120)))
        );
        let mut s = sig();
        s.class = 1;
        assert_ne!(
            base,
            mint(&s, &FaultCoord::none(), VTimeCoord::quantize(Moment(120)))
        );
        let mut s = sig();
        s.detail = b"other".to_vec();
        assert_ne!(
            base,
            mint(&s, &FaultCoord::none(), VTimeCoord::quantize(Moment(120)))
        );
        let mut s = sig();
        s.stop = 2;
        assert_ne!(
            base,
            mint(&s, &FaultCoord::none(), VTimeCoord::quantize(Moment(120)))
        );

        // Coordinate 2.
        let faults = FaultCoord::from_tokens([vec![1u8, 2], vec![3u8]]);
        assert_ne!(
            base,
            mint(&sig(), &faults, VTimeCoord::quantize(Moment(120)))
        );

        // Coordinate 3 — a bracket apart.
        assert_ne!(
            base,
            mint(
                &sig(),
                &FaultCoord::none(),
                VTimeCoord::quantize(Moment(120 + FINGERPRINT_VTIME_BRACKET))
            )
        );
    }

    /// The fault coordinate is a canonical *set*: token order and duplicates do
    /// not change the digest.
    #[test]
    fn fault_coord_is_an_ordered_set() {
        let a = FaultCoord::from_tokens([vec![1u8], vec![2u8], vec![2u8]]);
        let b = FaultCoord::from_tokens([vec![2u8], vec![1u8]]);
        assert_eq!(a.len(), 2);
        assert_eq!(
            mint(&sig(), &a, VTimeCoord::quantize(Moment(0))),
            mint(&sig(), &b, VTimeCoord::quantize(Moment(0)))
        );
        assert!(FaultCoord::none().is_empty());
    }

    /// Quantization collapses within a bracket and splits across it.
    #[test]
    fn vtime_quantizes_into_brackets() {
        assert_eq!(
            VTimeCoord::quantize(Moment(0)),
            VTimeCoord::quantize(Moment(FINGERPRINT_VTIME_BRACKET - 1))
        );
        assert_ne!(
            VTimeCoord::quantize(Moment(0)),
            VTimeCoord::quantize(Moment(FINGERPRINT_VTIME_BRACKET))
        );
    }

    /// The stop discriminant is pinned per variant (a reorder-proof coordinate).
    #[test]
    fn stop_discriminant_is_pinned() {
        let z = VTime(0);
        assert_eq!(StopReason::Deadline { vtime: z }.discriminant(), 0);
        assert_eq!(StopReason::Quiescent { vtime: z }.discriminant(), 1);
        assert_eq!(
            StopReason::Crash {
                vtime: z,
                info: vec![]
            }
            .discriminant(),
            2
        );
        assert_eq!(
            StopReason::Decision {
                vtime: z,
                id: 0,
                ctx: vec![]
            }
            .discriminant(),
            3
        );
        assert_eq!(
            StopReason::Assertion {
                vtime: z,
                id: 0,
                data: vec![]
            }
            .discriminant(),
            4
        );
        assert_eq!(StopReason::SnapshotPoint { vtime: z }.discriminant(), 5);
    }
}
