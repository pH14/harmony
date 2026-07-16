// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arch-neutral snapshot codec seam: [`SnapshotRecords`].
//!
//! A snapshot's **record set** (which registers a `REGS`-class section carries)
//! is per-architecture, but everything the *engine* does with a snapshot is not:
//! it seals the canonical bytes into the snapshot store, decodes them back on
//! restore, and reads the arch-neutral engine blocks (the V-time clock, the
//! timer queue, the entropy-stream position). This trait names exactly that
//! engine-facing surface, so vmm-core's snapshot glue can hold a vendor's
//! associated snapshot type (`Vendor::Snapshot`) without ever naming a register
//! record — the `docs/ARCH-BOUNDARY.md` §D snapshot-state seam, ruled 2026-07-14
//! (PR #109) and landed with the ARM skeleton (`hm-cbt`).
//!
//! Each implementor is one architecture's record set over the same TLV
//! container (`VM_STATE_MAGIC` / [`VM_STATE_VERSION`](crate::VM_STATE_VERSION)),
//! distinguished by the header's **arch tag**: a blob is only ever decoded under
//! its own tag, and a foreign tag is a loud
//! [`UnsupportedArch`](crate::VmStateError::UnsupportedArch), never a
//! reinterpretation.
//!
//! designed-not-frozen (AA-3): the trait shape is the ruled seam design; the
//! ARM spike's trait-freeze memo owns the freeze, and AA-6 owns which records
//! an arm64 snapshot must carry.

use crate::error::VmStateError;
use crate::types::{TimerQueueState, VtimeState};
use crate::{ARCH_X86_64, VmState};

/// One architecture's canonical snapshot record set: the codec surface the
/// engine seals and restores through, plus the arch-neutral engine blocks it
/// reads directly. Everything else in a snapshot (register records, the device
/// blob, the contract hash) is the vendor's own and is reached only through
/// the vendor's `Vendor` hooks.
pub trait SnapshotRecords: Sized {
    /// The container arch tag this record set encodes under
    /// ([`ARCH_X86_64`](crate::ARCH_X86_64) = 1;
    /// [`ARCH_AARCH64`](crate::ARCH_AARCH64) = 2). `decode` rejects any other
    /// tag as [`VmStateError::UnsupportedArch`].
    const ARCH_TAG: u16;

    /// Encode to the canonical, byte-deterministic TLV blob (equal values ⇒
    /// equal bytes). Fallible exactly as the underlying codec is: a
    /// non-restorable-exactly state (e.g. a fractional V-time ratio) is
    /// refused rather than written.
    ///
    /// # Errors
    /// The implementor's codec errors — e.g.
    /// [`VmStateError::FractionalRatio`] or [`VmStateError::InvalidField`].
    fn encode(&self) -> Result<Vec<u8>, VmStateError>;

    /// Decode a blob produced by [`encode`](SnapshotRecords::encode). Total
    /// over arbitrary input (never panics); validates the magic, the version,
    /// **and the arch tag** ([`ARCH_TAG`](SnapshotRecords::ARCH_TAG)) — a
    /// foreign record set is rejected loudly, never reinterpreted.
    ///
    /// # Errors
    /// The matching [`VmStateError`] for any malformed, foreign, or truncated
    /// blob.
    fn decode(bytes: &[u8]) -> Result<Self, VmStateError>;

    /// The engine's V-time block (clock rate + the captured `snapshot_vns`).
    /// Arch-neutral: every vendor's snapshot carries one, and the engine —
    /// not the vendor — validates and re-commits it on restore.
    fn vtime(&self) -> &VtimeState;

    /// The engine's absolute-V-time timer-queue block. Arch-neutral; a
    /// vmm-core snapshot always seals it empty (the fabric timer rides the
    /// vendor's device blob), and the engine fails closed on a non-empty one.
    fn timers(&self) -> &TimerQueueState;

    /// The engine's entropy-stream / hypercall-dispatcher state bytes (the
    /// `hypercall` section: notably the seeded-PRNG position). Opaque here;
    /// the engine's entropy service validates them on restore.
    fn entropy_bytes(&self) -> &[u8];
}

impl SnapshotRecords for VmState {
    const ARCH_TAG: u16 = ARCH_X86_64;

    fn encode(&self) -> Result<Vec<u8>, VmStateError> {
        // The inherent codec (`codec.rs`) — the trait only names it.
        VmState::encode(self)
    }

    fn decode(bytes: &[u8]) -> Result<Self, VmStateError> {
        VmState::decode(bytes)
    }

    fn vtime(&self) -> &VtimeState {
        &self.vtime
    }

    fn timers(&self) -> &TimerQueueState {
        &self.timers
    }

    fn entropy_bytes(&self) -> &[u8] {
        &self.hypercall
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trait surface must agree with the inherent codec byte-for-byte —
    /// the engine reaches the codec only through the trait, so a divergence
    /// here would silently fork the canonical form.
    #[test]
    fn trait_codec_is_the_inherent_codec() {
        let mut s = VmState {
            contract_hash: [9u8; 32],
            hypercall: vec![1, 2, 3],
            ..Default::default()
        };
        s.vtime.ratio_den = 1;
        s.vtime.snapshot_vns = 42;

        let via_trait = <VmState as SnapshotRecords>::encode(&s).unwrap();
        assert_eq!(via_trait, VmState::encode(&s).unwrap());
        let back = <VmState as SnapshotRecords>::decode(&via_trait).unwrap();
        assert_eq!(back, s);

        assert_eq!(<VmState as SnapshotRecords>::ARCH_TAG, ARCH_X86_64);
        assert_eq!(s.vtime(), &s.vtime);
        assert_eq!(s.timers(), &s.timers);
        assert_eq!(s.entropy_bytes(), &s.hypercall[..]);
    }

    /// A fractional ratio is refused through the trait exactly as through the
    /// inherent codec (the engine relies on encode-side fail-closed).
    #[test]
    fn trait_encode_rejects_fractional_ratio() {
        let mut s = VmState::default();
        s.vtime.ratio_den = 2;
        assert_eq!(
            <VmState as SnapshotRecords>::encode(&s),
            Err(VmStateError::FractionalRatio)
        );
    }
}
