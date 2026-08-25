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
/// The `jnz` retires once per iteration — taken on every iteration but the
/// last — and nothing else branches. An event that counts every retired
/// conditional branch sees `n` per run; one that counts only taken branches
/// sees `n - 1`. Either way the count is `n` plus a constant, which is the
/// whole analysis behind [`guest_oracle_delta`].
#[must_use]
pub fn emit_loop_payload(n: u32) -> [u8; LOOP_PAYLOAD_BYTES] {
    let [b0, b1, b2, b3] = n.to_le_bytes();
    [0x66, 0xb9, b0, b1, b2, b3, 0x66, 0x49, 0x75, 0xfc, 0xf4]
}

/// The analytical count for the difference between two guest runs.
///
/// Each run of `n` iterations retires the `jnz` `n` times, counted as `n` or
/// `n - 1` depending on whether the event counts not-taken conditional
/// branches. The constant cancels in the difference, so an `n2` run minus an
/// `n1` run is exactly `n2 - n1` under either counting rule, with no
/// assumption about entry or exit work. There is deliberately no absolute
/// oracle: the per-run count is a per-event fact, and the differential is the
/// measurement.
#[must_use]
pub fn guest_oracle_delta(n1: u64, n2: u64) -> u64 {
    n2.saturating_sub(n1)
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

/// The registers that advance with time on their own. A save cannot reproduce
/// one by writing it back, because the register has moved on by the time the
/// next save reads it. They are the vCPU's clocks, not its state, so the
/// fixpoint holds them to advancing rather than to staying put.
pub const TIME_BASE_MSRS: [TimeBase; 3] = [
    TimeBase {
        index: 0x0000_0010,
        name: "IA32_TIME_STAMP_COUNTER",
        hz: 0,
    },
    TimeBase {
        index: 0x4000_0010,
        name: "HV_X64_MSR_VP_RUNTIME",
        hz: 10_000_000,
    },
    TimeBase {
        index: 0x4000_0020,
        name: "HV_X64_MSR_TIME_REF_COUNT",
        hz: 10_000_000,
    },
];

/// One free-running register, and how fast it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeBase {
    /// The MSR index.
    pub index: u32,
    /// The vendor's name for it.
    pub name: &'static str,
    /// Its rate in ticks per second, or zero for the timestamp counter, whose
    /// rate is a property of the part and is read from the vCPU.
    pub hz: u64,
}

impl TimeBase {
    /// The most this register can advance in `nanos`, with the rate of the
    /// timestamp counter supplied by the caller.
    #[must_use]
    pub fn ticks_in(self, nanos: u128, tsc_hz: u64) -> u128 {
        let hz = if self.hz == 0 { tsc_hz } else { self.hz };
        nanos * u128::from(hz) / 1_000_000_000
    }
}

/// Registers the vCPU state includes that a host write does not change, with
/// the reason each is that way. A fixpoint cannot be demonstrated over one, and
/// excluding it is a statement about the register rather than about the run, so
/// each is declared here with its evidence and confirmed by measurement.
///
/// The declaration is checked both ways. A register named here that does take a
/// write means this list is wrong; a register not named here that ignores one is
/// a restore that failed.
pub const READ_ONLY_MSRS: [(u32, &str, &str); 1] = [(
    0x4000_0020,
    "HV_X64_MSR_TIME_REF_COUNT",
    "the Hyper-V reference counter, read-only in the Hyper-V specification: a guest \
     reads elapsed reference time from it and never writes it. Linux v6.12.95 \
     arch/x86/kvm/hyperv.c kvm_hv_set_msr_pw rejects a guest write and discards a \
     host-initiated one, with the comment \"read-only, but still ignore it if \
     host-initiated\"; the read path returns get_time_ref_counter(kvm), a computed \
     value with no stored field behind it. Synthetic hypervisor state, not vCPU state \
     the silicon owns",
)];

/// Whether `index` is declared read-only, and why.
#[must_use]
pub fn read_only_reason(index: u32) -> Option<&'static str> {
    READ_ONLY_MSRS
        .iter()
        .find(|(i, _, _)| *i == index)
        .map(|(_, _, reason)| *reason)
}

/// How two saves of the same vCPU compare.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateDiff {
    /// Everything that moved and should not have. A fixpoint requires this to
    /// be empty.
    pub differing: Vec<String>,
    /// The time bases, with the value written back and the value read after. A
    /// time base that did not advance is reported in `differing` instead: a
    /// clock that stops, or runs backwards, is not the reason a fixpoint
    /// comparison may skip it.
    pub time_bases: Vec<TimeBaseReading>,
}

/// What one time base read after being written back the value it held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeBaseReading {
    /// Which register.
    pub base: TimeBase,
    /// The value the restore wrote.
    pub restored: u64,
    /// The value the save after the restore read.
    pub observed: u64,
}

impl TimeBaseReading {
    /// How far the register moved between the restore and the save after it.
    #[must_use]
    pub fn advance(self) -> u64 {
        self.observed.saturating_sub(self.restored)
    }

    /// This reading, with the bound it was judged against.
    #[must_use]
    pub fn describe(self, bound: u128) -> String {
        format!(
            "{} ({:#010x}): restored {:#x}, read {:#x}, advanced {} of at most {bound} \
             the round trip could consume",
            self.base.name,
            self.base.index,
            self.restored,
            self.observed,
            self.advance()
        )
    }
}

/// Read one MSR component's bytes back as `index, data` pairs.
///
/// # Errors
/// [`GuestError::Kvm`] when the byte count is not a whole number of entries.
pub fn decode_msr_pairs(bytes: &[u8]) -> Result<Vec<(u32, u64)>, GuestError> {
    if !bytes.len().is_multiple_of(12) {
        return Err(GuestError::Kvm {
            what: "decoding the saved MSR list".to_string(),
            detail: format!("{} bytes is not a whole number of entries", bytes.len()),
        });
    }
    Ok(bytes
        .chunks_exact(12)
        .map(|chunk| {
            let index = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            let mut data = [0u8; 8];
            data.copy_from_slice(&chunk[4..12]);
            (index, u64::from_le_bytes(data))
        })
        .collect())
}

/// Compare two MSR captures, holding the time bases to advancing and everything
/// else to being unchanged.
fn compare_msrs(first: &[u8], second: &[u8], diff: &mut StateDiff) {
    let (Ok(before), Ok(after)) = (decode_msr_pairs(first), decode_msr_pairs(second)) else {
        diff.differing
            .push("msrs: a capture could not be read back as entries".to_string());
        return;
    };
    if before.len() != after.len() {
        diff.differing.push(format!(
            "msrs: {} entries then {} entries",
            before.len(),
            after.len()
        ));
        return;
    }
    for ((index, was), (also, now)) in before.iter().zip(after.iter()) {
        if index != also {
            diff.differing.push(format!(
                "msrs: the saves list different registers at the same position, {index:#010x} \
                 then {also:#010x}"
            ));
            return;
        }
        match TIME_BASE_MSRS.iter().find(|t| t.index == *index) {
            Some(base) if now > was => diff.time_bases.push(TimeBaseReading {
                base: *base,
                restored: *was,
                observed: *now,
            }),
            Some(base) => diff.differing.push(format!(
                "{} ({index:#010x}): a time base that did not advance, {was:#x} then {now:#x}",
                base.name
            )),
            None if was != now => diff.differing.push(format!(
                "msrs: {index:#010x} moved across the round trip, {was:#x} then {now:#x}"
            )),
            None => {}
        }
    }
}

/// How two saves of the same vCPU compare, component by component.
#[must_use]
pub fn compare_states(first: &StateCapture, second: &StateCapture) -> StateDiff {
    let mut diff = StateDiff::default();
    for component in &first.components {
        match second.components.iter().find(|c| c.name == component.name) {
            Some(other) if component.name == "msrs" => {
                compare_msrs(&component.bytes, &other.bytes, &mut diff);
            }
            Some(other) if other.bytes == component.bytes => {}
            Some(other) => diff.differing.push(format!(
                "{}: {} bytes then {} bytes, contents differ",
                component.name,
                component.bytes.len(),
                other.bytes.len()
            )),
            None => diff
                .differing
                .push(format!("{}: absent from the second save", component.name)),
        }
    }
    for component in &second.components {
        if !first.components.iter().any(|c| c.name == component.name) {
            diff.differing
                .push(format!("{}: absent from the first save", component.name));
        }
    }
    diff
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
        assert_eq!(guest_oracle_delta(100_000, 100_000), 0);
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
        assert!(compare_states(&a, &b).differing.is_empty());
    }

    #[test]
    fn a_component_whose_bytes_moved_is_named_with_both_lengths() {
        let a = capture(&[("regs", b"aaaa")]);
        let b = capture(&[("regs", b"aaab")]);
        let differing = compare_states(&a, &b).differing;
        assert_eq!(differing.len(), 1);
        assert!(differing[0].starts_with("regs:"), "{}", differing[0]);
        assert!(differing[0].contains("contents differ"), "{}", differing[0]);
    }

    /// An MSR component's bytes, in the `index, data` encoding a save uses.
    fn msr_bytes(pairs: &[(u32, u64)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (index, data) in pairs {
            out.extend_from_slice(&index.to_le_bytes());
            out.extend_from_slice(&data.to_le_bytes());
        }
        out
    }

    fn msr_capture(pairs: &[(u32, u64)]) -> StateCapture {
        let mut capture = StateCapture::default();
        capture.push("msrs", msr_bytes(pairs));
        capture
    }

    #[test]
    fn a_time_base_that_advanced_is_reported_as_a_clock_not_a_difference() {
        let a = msr_capture(&[(0x0000_0010, 100), (0x0000_0174, 8)]);
        let b = msr_capture(&[(0x0000_0010, 4_200), (0x0000_0174, 8)]);
        let diff = compare_states(&a, &b);
        assert!(diff.differing.is_empty(), "{:?}", diff.differing);
        assert_eq!(diff.time_bases.len(), 1);
        assert_eq!(diff.time_bases[0].base.name, "IA32_TIME_STAMP_COUNTER");
        assert_eq!(diff.time_bases[0].restored, 100);
        assert_eq!(diff.time_bases[0].observed, 4_200);
        assert_eq!(diff.time_bases[0].advance(), 4_100);
    }

    #[test]
    fn a_readings_description_carries_both_values_and_the_bound() {
        let reading = TimeBaseReading {
            base: TIME_BASE_MSRS[0],
            restored: 0x1000,
            observed: 0x1064,
        };
        assert_eq!(reading.advance(), 100);
        let text = reading.describe(4_096);
        for part in ["IA32_TIME_STAMP_COUNTER", "0x1000", "0x1064", "100", "4096"] {
            assert!(text.contains(part), "{part} missing from {text}");
        }
    }

    #[test]
    fn every_read_only_register_is_declared_with_its_evidence() {
        for (index, name, reason) in READ_ONLY_MSRS {
            assert_eq!(read_only_reason(index), Some(reason));
            assert!(!name.is_empty());
            // A register excluded from the must-restore set is excluded on
            // evidence, so the reason has to name where that evidence is.
            assert!(
                reason.contains("arch/x86/kvm/") && reason.len() > 100,
                "{name} needs the source that says it is read-only, got {reason:?}"
            );
        }
        assert_eq!(read_only_reason(0x0000_0010), None);
    }

    #[test]
    fn a_read_only_register_is_one_the_probe_actually_tests() {
        // The write probe walks the time bases. A register declared read-only
        // that is not among them would be excluded on a claim nothing checks.
        for (index, name, _) in READ_ONLY_MSRS {
            assert!(
                TIME_BASE_MSRS.iter().any(|t| t.index == index),
                "{name} is declared read-only but the probe never writes it"
            );
        }
    }

    #[test]
    fn a_time_bases_bound_is_its_own_rate_over_the_elapsed_time() {
        let tsc = TIME_BASE_MSRS[0];
        let hv = TIME_BASE_MSRS[2];
        assert_eq!(
            tsc.hz, 0,
            "the timestamp counter's rate comes from the part"
        );
        // A millisecond of a 3 GHz timestamp counter is three million ticks; the
        // same millisecond of a 10 MHz reference counter is ten thousand.
        assert_eq!(tsc.ticks_in(1_000_000, 3_000_000_000), 3_000_000);
        assert_eq!(hv.ticks_in(1_000_000, 3_000_000_000), 10_000);
    }

    #[test]
    fn a_time_base_that_stopped_or_ran_backwards_is_a_difference() {
        for second in [100u64, 40] {
            let a = msr_capture(&[(0x0000_0010, 100)]);
            let b = msr_capture(&[(0x0000_0010, second)]);
            let diff = compare_states(&a, &b);
            assert!(diff.time_bases.is_empty(), "{:?}", diff.time_bases);
            assert_eq!(diff.differing.len(), 1, "{:?}", diff.differing);
            assert!(
                diff.differing[0].contains("did not advance"),
                "{}",
                diff.differing[0]
            );
        }
    }

    #[test]
    fn an_msr_that_is_not_a_time_base_is_named_by_index_when_it_moves() {
        let a = msr_capture(&[(0x0000_0174, 8)]);
        let b = msr_capture(&[(0x0000_0174, 9)]);
        let diff = compare_states(&a, &b);
        assert_eq!(diff.differing.len(), 1);
        assert!(
            diff.differing[0].contains("0x00000174") && diff.differing[0].contains("0x8"),
            "{}",
            diff.differing[0]
        );
    }

    #[test]
    fn two_saves_that_list_different_registers_are_not_compared_pairwise() {
        let a = msr_capture(&[(0x0000_0174, 8)]);
        let b = msr_capture(&[(0x0000_0175, 8)]);
        let diff = compare_states(&a, &b);
        assert_eq!(diff.differing.len(), 1);
        assert!(
            diff.differing[0].contains("different registers"),
            "{}",
            diff.differing[0]
        );
    }

    #[test]
    fn a_component_present_in_only_one_save_is_named() {
        let a = capture(&[("regs", b"aaaa"), ("xsave", b"cc")]);
        let b = capture(&[("regs", b"aaaa")]);
        assert_eq!(
            compare_states(&a, &b).differing,
            vec!["xsave: absent from the second save".to_string()]
        );
        assert_eq!(
            compare_states(&b, &a).differing,
            vec!["xsave: absent from the first save".to_string()]
        );
    }
}
