// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest window — the portable half.
//!
//! Two stage-1 measurements read the counter through a vCPU rather than from
//! userspace: count exactness with the work clock filtered to guest execution,
//! and the save/restore fixpoint over the vCPU's state. Everything here — the
//! guest payload's encoding, its analytical oracle, and the fixpoint
//! comparison — is portable and unit-tested. The KVM calls live in
//! [`crate::guest_sys`].

use std::collections::BTreeSet;

/// Where the guest payload is placed, and where execution starts.
pub const GUEST_PHYS: u64 = 0x1000;

/// How much RAM the guest gets. Enough for the payload with room to spare, and
/// small enough that mapping it costs nothing.
pub const GUEST_RAM_BYTES: usize = 0x1_0000;

/// The encoded loop payload's length in bytes.
pub const LOOP_PAYLOAD_BYTES: usize = 11;

/// A refusal from the guest window.
#[derive(Debug, thiserror::Error)]
pub enum GuestError {
    /// The guest window needs KVM.
    #[error("the guest window runs on Linux with KVM and this build is for {target}")]
    WrongPlatform {
        /// The platform this build targets.
        target: &'static str,
    },
    /// The guest window is not built for this architecture.
    #[error("the guest window is not built for {arch}")]
    WrongArch {
        /// The architecture this build targets.
        arch: &'static str,
    },
    /// A KVM call failed.
    #[error("{what} failed: {detail}")]
    Kvm {
        /// What was being done.
        what: String,
        /// Why it failed.
        detail: String,
    },
    /// The guest left through an exit the measurement cannot account for. A
    /// fault that ends the run must not read as a completed payload.
    #[error("the guest exited with reason {reason} rather than halting")]
    NotHalted {
        /// The `kvm_run` exit reason.
        reason: u32,
    },
    /// A state component the fixpoint requires was not captured.
    #[error("the vCPU state capture is missing: {missing:?}")]
    IncompleteCapture {
        /// The components that are required and absent.
        missing: Vec<String>,
    },
}

/// Encode the guest payload: a real-mode loop of `n` iterations, then `hlt`.
///
/// ```text
/// 66 b9 <n:32>   mov ecx, n
/// 66 49          dec ecx        <- the loop body
/// 75 fc          jnz -4
/// f4             hlt
/// ```
///
/// The `jnz` is taken on every iteration but the last, so the payload retires
/// exactly `n - 1` taken branches and nothing else branches. That is the whole
/// analysis behind [`guest_oracle_delta`].
#[must_use]
pub fn emit_loop_payload(n: u32) -> [u8; LOOP_PAYLOAD_BYTES] {
    let [b0, b1, b2, b3] = n.to_le_bytes();
    [0x66, 0xb9, b0, b1, b2, b3, 0x66, 0x49, 0x75, 0xfc, 0xf4]
}

/// The analytical count for the difference between two guest runs.
///
/// Each run of `n` iterations retires `n - 1` taken branches, so the difference
/// between an `n2` run and an `n1` run is exactly `n2 - n1`. The `- 1` cancels,
/// which is why the differential needs no assumption about entry or exit work.
#[must_use]
pub fn guest_oracle_delta(n1: u64, n2: u64) -> u64 {
    n2.saturating_sub(n1)
}

/// The taken branches one guest run of `n` iterations retires.
#[must_use]
pub fn guest_oracle_absolute(n: u64) -> u64 {
    n.saturating_sub(1)
}

/// One component of a saved vCPU state, and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateComponent {
    /// The component's name, as the report spells it.
    pub name: &'static str,
    /// The component's bytes, exactly as the vCPU handed them over.
    pub bytes: Vec<u8>,
}

/// A full save of a vCPU's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateCapture {
    /// Every captured component, in the order they were read.
    pub components: Vec<StateComponent>,
}

/// The components a fixpoint must cover. A capture missing any of these is not
/// a full vCPU state, so it cannot demonstrate a fixpoint over one.
pub const REQUIRED_COMPONENTS: [&str; 6] =
    ["regs", "sregs", "xsave", "xcrs", "msrs", "vcpu-events"];

impl StateCapture {
    /// Add one component.
    pub fn push(&mut self, name: &'static str, bytes: Vec<u8>) {
        self.components.push(StateComponent { name, bytes });
    }

    /// The names of every captured component.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.components.iter().map(|c| c.name.to_string()).collect()
    }

    /// How many bytes the whole capture holds.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.components.iter().map(|c| c.bytes.len() as u64).sum()
    }

    /// Required components this capture does not hold, and any component that
    /// is present but empty. An empty component is a read that returned
    /// nothing, which must not read as a captured one.
    #[must_use]
    pub fn missing(&self) -> Vec<String> {
        let present: BTreeSet<&str> = self
            .components
            .iter()
            .filter(|c| !c.bytes.is_empty())
            .map(|c| c.name)
            .collect();
        REQUIRED_COMPONENTS
            .iter()
            .filter(|name| !present.contains(*name))
            .map(|name| (*name).to_string())
            .collect()
    }
}

/// Components whose bytes differ between two saves, and components present in
/// one save but not the other.
#[must_use]
pub fn differing_components(first: &StateCapture, second: &StateCapture) -> Vec<String> {
    let mut differing = Vec::new();
    for component in &first.components {
        match second.components.iter().find(|c| c.name == component.name) {
            Some(other) if other.bytes == component.bytes => {}
            Some(other) => differing.push(format!(
                "{}: {} bytes then {} bytes, contents differ",
                component.name,
                component.bytes.len(),
                other.bytes.len()
            )),
            None => differing.push(format!("{}: absent from the second save", component.name)),
        }
    }
    for component in &second.components {
        if !first.components.iter().any(|c| c.name == component.name) {
            differing.push(format!("{}: absent from the first save", component.name));
        }
    }
    differing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_payload_encodes_its_iteration_count_little_endian() {
        let payload = emit_loop_payload(0x0002_0001);
        assert_eq!(
            payload,
            [
                0x66, 0xb9, 0x01, 0x00, 0x02, 0x00, 0x66, 0x49, 0x75, 0xfc, 0xf4
            ]
        );
        // The jump is back four bytes, to the `dec ecx` at offset 6.
        assert_eq!(payload[8], 0x75);
        assert_eq!(payload[9] as i8, -4);
        assert_eq!(payload[LOOP_PAYLOAD_BYTES - 1], 0xf4, "the payload halts");
    }

    #[test]
    fn the_payload_fits_the_guest_ram_it_is_placed_in() {
        assert!(GUEST_PHYS as usize + LOOP_PAYLOAD_BYTES < GUEST_RAM_BYTES);
    }

    #[test]
    fn the_guest_oracle_is_the_iteration_difference() {
        assert_eq!(guest_oracle_delta(100_000, 200_000), 100_000);
        // One fewer taken branch than iterations, on each side, so the
        // difference carries no correction.
        assert_eq!(
            guest_oracle_absolute(200_000) - guest_oracle_absolute(100_000),
            guest_oracle_delta(100_000, 200_000)
        );
        assert_eq!(guest_oracle_absolute(0), 0);
    }

    fn capture(names: &[(&'static str, &[u8])]) -> StateCapture {
        let mut capture = StateCapture::default();
        for (name, bytes) in names {
            capture.push(name, bytes.to_vec());
        }
        capture
    }

    #[test]
    fn a_capture_that_covers_every_required_component_is_missing_nothing() {
        let full = capture(
            &REQUIRED_COMPONENTS
                .iter()
                .map(|n| (*n, &b"x"[..]))
                .collect::<Vec<_>>(),
        );
        assert!(full.missing().is_empty());
        assert_eq!(full.total_bytes(), REQUIRED_COMPONENTS.len() as u64);
        assert_eq!(full.names().len(), REQUIRED_COMPONENTS.len());
    }

    #[test]
    fn an_empty_component_counts_as_missing_rather_than_captured() {
        let mut partial = capture(
            &REQUIRED_COMPONENTS
                .iter()
                .map(|n| (*n, &b"x"[..]))
                .collect::<Vec<_>>(),
        );
        partial
            .components
            .iter_mut()
            .find(|c| c.name == "xsave")
            .expect("xsave is required")
            .bytes
            .clear();
        assert_eq!(partial.missing(), vec!["xsave".to_string()]);
    }

    #[test]
    fn a_capture_of_nothing_is_missing_everything() {
        let empty = StateCapture::default();
        assert_eq!(empty.missing().len(), REQUIRED_COMPONENTS.len());
        assert_eq!(empty.total_bytes(), 0);
    }

    #[test]
    fn two_identical_saves_differ_nowhere() {
        let a = capture(&[("regs", b"aaaa"), ("sregs", b"bbbb")]);
        let b = capture(&[("regs", b"aaaa"), ("sregs", b"bbbb")]);
        assert!(differing_components(&a, &b).is_empty());
    }

    #[test]
    fn a_component_whose_bytes_moved_is_named_with_both_lengths() {
        let a = capture(&[("regs", b"aaaa")]);
        let b = capture(&[("regs", b"aaab")]);
        let differing = differing_components(&a, &b);
        assert_eq!(differing.len(), 1);
        assert!(differing[0].starts_with("regs:"), "{}", differing[0]);
        assert!(differing[0].contains("contents differ"), "{}", differing[0]);
    }

    #[test]
    fn a_component_present_in_only_one_save_is_named() {
        let a = capture(&[("regs", b"aaaa"), ("xsave", b"cc")]);
        let b = capture(&[("regs", b"aaaa")]);
        assert_eq!(
            differing_components(&a, &b),
            vec!["xsave: absent from the second save".to_string()]
        );
        assert_eq!(
            differing_components(&b, &a),
            vec!["xsave: absent from the first save".to_string()]
        );
    }
}
