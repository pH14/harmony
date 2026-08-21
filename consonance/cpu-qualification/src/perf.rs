// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pure half of the counter plumbing: the `perf_event_attr` this suite opens,
//! and the overflow-ring walk.
//!
//! No syscall and no `libc` here, so the attribute layout, the flag bits, and the
//! ring arithmetic compile and are exact-value tested on every platform — a wrong
//! field or a flipped bit is caught in the ordinary lane rather than only on the
//! chip. The syscalls that hand these values to the kernel live in
//! [`crate::perf_sys`], which cannot run without perf.
//!
//! Mirrors `vmm-backend`'s `pmu` / `pmu_sys` split. The difference is that the
//! event config here is a parameter: the known-chip table supplies it, so one
//! measurement path serves Intel, AMD, and aarch64.

/// `PERF_TYPE_RAW`.
pub const PERF_TYPE_RAW: u32 = 4;
/// `perf_event_attr` version-5 size, in bytes.
pub const ATTR_SIZE_VER5: u32 = 112;

// perf_event_attr flag-word bits (include/uapi/linux/perf_event.h). Bit 0 is the
// literal 1 rather than `1 << 0`, which would be an equivalent mutant under a
// shift flip.
/// `disabled` (bit 0).
const F_DISABLED: u64 = 1;
/// `pinned` (bit 2).
const F_PINNED: u64 = 1 << 2;
/// `exclude_kernel` (bit 5).
const F_EXCLUDE_KERNEL: u64 = 1 << 5;
/// `exclude_hv` (bit 6).
const F_EXCLUDE_HV: u64 = 1 << 6;
/// `exclude_host` (bit 19).
const F_EXCLUDE_HOST: u64 = 1 << 19;

/// `read_format.total_time_enabled` (bit 0).
const FORMAT_TOTAL_TIME_ENABLED: u64 = 1;
/// `read_format.total_time_running` (bit 1).
const FORMAT_TOTAL_TIME_RUNNING: u64 = 1 << 1;

/// The composed `read_format` for a counting event: value, enabled, running. A
/// pinned counter must show `enabled == running`; anything else is multiplexing.
pub const READ_FORMAT_TIMED: u64 = FORMAT_TOTAL_TIME_ENABLED + FORMAT_TOTAL_TIME_RUNNING;

/// `PERF_SAMPLE_READ` (bit 4). The kernel writes the counter value taken at the
/// overflow interrupt into the sample, so the skid is that value minus the period
/// with no delivery latency in it.
pub const PERF_SAMPLE_READ: u64 = 1 << 4;

/// `perf_event_mmap_page.data_head` byte offset.
pub const DATA_HEAD_OFF: usize = 1024;
/// `perf_event_mmap_page.data_tail` byte offset.
pub const DATA_TAIL_OFF: usize = 1032;

/// `PERF_RECORD_LOST`.
pub const PERF_RECORD_LOST: u32 = 2;
/// `PERF_RECORD_THROTTLE`.
pub const PERF_RECORD_THROTTLE: u32 = 5;
/// `PERF_RECORD_UNTHROTTLE`.
pub const PERF_RECORD_UNTHROTTLE: u32 = 6;
/// `PERF_RECORD_SAMPLE`.
pub const PERF_RECORD_SAMPLE: u32 = 9;

/// What a counter counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The calling thread's own user-mode execution: `exclude_kernel` and
    /// `exclude_hv`, no `exclude_host`. This is what the host-side exactness
    /// payloads are counted under.
    HostUser,
    /// Guest execution only: `exclude_host`, so exits and the host work between
    /// them contribute nothing.
    GuestOnly,
}

impl Scope {
    /// The flag bits this scope adds on top of the always-set ones.
    const fn flags(self) -> u64 {
        match self {
            Scope::HostUser => F_EXCLUDE_KERNEL + F_EXCLUDE_HV,
            Scope::GuestOnly => F_EXCLUDE_HOST,
        }
    }
}

/// `perf_event_attr`, version-5 layout. Fields beyond what this suite sets stay
/// zero. Handed whole to `perf_event_open`; never read back.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PerfEventAttr {
    type_: u32,
    size: u32,
    config: u64,
    sample_period: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup_events: u32,
    bp_type: u32,
    config1: u64,
    config2: u64,
    branch_sample_type: u64,
    sample_regs_user: u64,
    sample_stack_user: u32,
    clockid: i32,
    sample_regs_intr: u64,
    aux_watermark: u32,
    sample_max_stack: u16,
    reserved_2: u16,
}

const _: () = assert!(std::mem::size_of::<PerfEventAttr>() == ATTR_SIZE_VER5 as usize);

/// A counting event: pinned, disabled at open, timed so multiplexing is visible.
#[must_use]
pub fn counting_attr(config: u64, scope: Scope) -> PerfEventAttr {
    PerfEventAttr {
        type_: PERF_TYPE_RAW,
        size: ATTR_SIZE_VER5,
        config,
        read_format: READ_FORMAT_TIMED,
        flags: F_DISABLED + F_PINNED + scope.flags(),
        ..Default::default()
    }
}

/// A sampling event: pinned, disabled at open, one wakeup per overflow, each
/// sample carrying the counter value taken at the overflow interrupt.
///
/// `period` is the count the first overflow fires at. A caller re-arms with the
/// period ioctl before each measurement rather than relying on the value here.
#[must_use]
pub fn sampling_attr(config: u64, scope: Scope, period: u64) -> PerfEventAttr {
    PerfEventAttr {
        type_: PERF_TYPE_RAW,
        size: ATTR_SIZE_VER5,
        config,
        sample_period: period,
        sample_type: PERF_SAMPLE_READ,
        // The sample body must be exactly one u64, so read_format stays zero.
        read_format: 0,
        flags: F_DISABLED + F_PINNED + scope.flags(),
        wakeup_events: 1,
        ..Default::default()
    }
}

/// What one walk of the overflow ring found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RingScan {
    /// Delivered overflow interrupts.
    pub samples: u64,
    /// The counter value the last sample carried.
    pub last_value: u64,
    /// Records the kernel dropped because the ring was full.
    pub lost: u64,
    /// Throttle and unthrottle records: the kernel suppressed interrupts.
    pub throttle: u64,
    /// Any other record type, and any header the walk could not parse.
    pub other: u64,
}

/// Read `len` bytes starting at ring byte offset `pos`, wrapping at `data_size`.
///
/// # Safety
/// `data` must point to `data_size` readable bytes; `len` must be at most 8.
unsafe fn read_wrapped(data: *const u8, data_size: usize, pos: u64, len: usize) -> u64 {
    let mut bytes = [0u8; 8];
    for (i, slot) in bytes.iter_mut().enumerate().take(len) {
        let at = ((pos + i as u64) % data_size as u64) as usize;
        // SAFETY: `at` is reduced modulo `data_size`, so it indexes inside the
        // caller's mapping.
        *slot = unsafe { std::ptr::read_unaligned(data.add(at)) };
    }
    u64::from_le_bytes(bytes)
}

/// Walk the overflow ring mapped at `base` (one control page of `page_size`
/// bytes, then `data_size` bytes of data pages), counting record types and
/// keeping the last sample's value, then publish `data_tail := data_head` so the
/// kernel never sees a full buffer.
///
/// Records wrap at the end of the data area, so every field is read byte-wise
/// modulo `data_size`. Corrupt cursors and unparseable headers are counted under
/// [`RingScan::other`] and stop the walk; the ring still drains.
///
/// Factored here, in the portable half, so the offset arithmetic and the pointer
/// access run under Miri and the ordinary test lane over a test-owned buffer;
/// [`crate::perf_sys`] only supplies the real mapping.
///
/// # Safety
/// `base` must point to a valid, writable, 8-aligned mapping of at least
/// `page_size + data_size` bytes laid out as a perf ring, with `page_size` at
/// least `DATA_TAIL_OFF + 8` and `data_size` a power-of-two multiple of 8.
pub unsafe fn scan_ring_at(base: *mut u8, page_size: usize, data_size: usize) -> RingScan {
    let mut scan = RingScan::default();
    // SAFETY: the caller guarantees the mapping covers the control page and the
    // data area; head and tail are at the documented offsets and 8-aligned. The
    // acquire fence orders the record reads after the head read, because the
    // kernel publishes records before advancing head.
    unsafe {
        let head = std::ptr::read_volatile(base.add(DATA_HEAD_OFF).cast::<u64>());
        let mut tail = std::ptr::read_volatile(base.add(DATA_TAIL_OFF).cast::<u64>());
        std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
        if tail > head
            || head - tail > data_size as u64
            || !tail.is_multiple_of(8)
            || !head.is_multiple_of(8)
        {
            scan.other += 1;
            std::ptr::write_volatile(base.add(DATA_TAIL_OFF).cast::<u64>(), head);
            return scan;
        }
        let data = base.add(page_size).cast_const();
        while tail.saturating_add(8) <= head {
            // perf_event_header: u32 type at +0, u16 misc at +4, u16 size at +6.
            let ty = u32::try_from(read_wrapped(data, data_size, tail, 4)).unwrap_or(u32::MAX);
            let size = read_wrapped(data, data_size, tail + 6, 2);
            if size < 8 || !size.is_multiple_of(8) || size > head - tail {
                scan.other += 1;
                break;
            }
            match ty {
                PERF_RECORD_SAMPLE => {
                    scan.samples += 1;
                    if size >= 16 {
                        scan.last_value = read_wrapped(data, data_size, tail + 8, 8);
                    }
                }
                PERF_RECORD_LOST => scan.lost += 1,
                PERF_RECORD_THROTTLE | PERF_RECORD_UNTHROTTLE => scan.throttle += 1,
                _ => scan.other += 1,
            }
            tail += size;
        }
        std::ptr::write_volatile(base.add(DATA_TAIL_OFF).cast::<u64>(), head);
    }
    scan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_constants_are_the_exact_uapi_values() {
        assert_eq!(PERF_TYPE_RAW, 4);
        assert_eq!(ATTR_SIZE_VER5, 112);
        assert_eq!(F_DISABLED, 0x1);
        assert_eq!(F_PINNED, 0x4);
        assert_eq!(F_EXCLUDE_KERNEL, 0x20);
        assert_eq!(F_EXCLUDE_HV, 0x40);
        assert_eq!(F_EXCLUDE_HOST, 0x8_0000);
        assert_eq!(FORMAT_TOTAL_TIME_ENABLED, 0x1);
        assert_eq!(FORMAT_TOTAL_TIME_RUNNING, 0x2);
        assert_eq!(READ_FORMAT_TIMED, 0b11);
        assert_eq!(PERF_SAMPLE_READ, 0x10);
        assert_eq!(DATA_HEAD_OFF, 1024);
        assert_eq!(DATA_TAIL_OFF, 1032);
        assert_eq!(PERF_RECORD_LOST, 2);
        assert_eq!(PERF_RECORD_THROTTLE, 5);
        assert_eq!(PERF_RECORD_UNTHROTTLE, 6);
        assert_eq!(PERF_RECORD_SAMPLE, 9);
    }

    #[test]
    fn a_counting_attr_is_pinned_timed_and_scoped() {
        let host = counting_attr(0x1c4, Scope::HostUser);
        assert_eq!(host.type_, 4);
        assert_eq!(host.size, 112);
        assert_eq!(host.config, 0x1c4);
        assert_eq!(host.sample_period, 0, "counting mode, not sampling");
        assert_eq!(host.sample_type, 0);
        assert_eq!(host.read_format, 0b11);
        assert_eq!(
            host.flags, 0x65,
            "disabled | pinned | exclude_kernel | exclude_hv"
        );
        assert_eq!(host.wakeup_events, 0);

        let guest = counting_attr(0x0051_00d1, Scope::GuestOnly);
        assert_eq!(guest.config, 0x0051_00d1);
        assert_eq!(guest.flags, 0x8_0005, "disabled | pinned | exclude_host");
    }

    #[test]
    fn a_sampling_attr_carries_the_value_at_the_interrupt_and_wakes_once() {
        let attr = sampling_attr(0x21, Scope::HostUser, 100_000);
        assert_eq!(attr.config, 0x21);
        assert_eq!(attr.sample_period, 100_000);
        assert_eq!(attr.sample_type, PERF_SAMPLE_READ);
        assert_eq!(
            attr.read_format, 0,
            "a zero read_format makes the sample body exactly one value"
        );
        assert_eq!(attr.flags, 0x65);
        assert_eq!(attr.wakeup_events, 1);
    }

    #[test]
    fn every_unset_attr_field_stays_zero() {
        for attr in [
            counting_attr(1, Scope::HostUser),
            sampling_attr(1, Scope::GuestOnly, 2),
        ] {
            assert_eq!(attr.bp_type, 0);
            assert_eq!(attr.config1, 0);
            assert_eq!(attr.config2, 0);
            assert_eq!(attr.branch_sample_type, 0);
            assert_eq!(attr.sample_regs_user, 0);
            assert_eq!(attr.sample_stack_user, 0);
            assert_eq!(attr.clockid, 0);
            assert_eq!(attr.sample_regs_intr, 0);
            assert_eq!(attr.aux_watermark, 0);
            assert_eq!(attr.sample_max_stack, 0);
            assert_eq!(attr.reserved_2, 0);
        }
    }

    const PAGE: usize = 4096;
    const DATA: usize = 4096;

    /// A u64-aligned test-owned stand-in for the perf ring.
    struct FakeRing {
        buf: Vec<u64>,
    }

    impl FakeRing {
        fn new() -> FakeRing {
            FakeRing {
                buf: vec![0u64; (PAGE + DATA) / 8],
            }
        }
        fn base(&mut self) -> *mut u8 {
            self.buf.as_mut_ptr().cast::<u8>()
        }
        /// Write one byte at ring offset `off`, wrapping.
        fn put(&mut self, off: u64, byte: u8) {
            let at = PAGE + (off % DATA as u64) as usize;
            let word = at / 8;
            let shift = (at % 8) * 8;
            self.buf[word] &= !(0xffu64 << shift);
            self.buf[word] |= u64::from(byte) << shift;
        }
        /// Write a header and, for a 16-byte sample, its value. Returns the next
        /// record's offset.
        fn push(&mut self, off: u64, ty: u32, size: u16, value: u64) -> u64 {
            for (i, b) in ty.to_le_bytes().iter().enumerate() {
                self.put(off + i as u64, *b);
            }
            for (i, b) in size.to_le_bytes().iter().enumerate() {
                self.put(off + 6 + i as u64, *b);
            }
            if size >= 16 {
                for (i, b) in value.to_le_bytes().iter().enumerate() {
                    self.put(off + 8 + i as u64, *b);
                }
            }
            off + u64::from(size)
        }
        fn set(&mut self, head: u64, tail: u64) {
            self.buf[DATA_HEAD_OFF / 8] = head;
            self.buf[DATA_TAIL_OFF / 8] = tail;
        }
        fn tail(&self) -> u64 {
            self.buf[DATA_TAIL_OFF / 8]
        }
    }

    #[test]
    fn the_walk_counts_each_record_type_and_keeps_the_last_sample_value() {
        let mut ring = FakeRing::new();
        let mut off = 0u64;
        off = ring.push(off, PERF_RECORD_SAMPLE, 16, 100_007);
        off = ring.push(off, PERF_RECORD_THROTTLE, 24, 0);
        off = ring.push(off, PERF_RECORD_SAMPLE, 16, 100_042);
        off = ring.push(off, PERF_RECORD_LOST, 16, 0);
        off = ring.push(off, 3, 8, 0);
        ring.set(off, 0);
        // SAFETY: `buf` is u64-aligned and covers PAGE + DATA bytes; `base` is
        // its only live pointer for the duration of the call.
        let scan = unsafe { scan_ring_at(ring.base(), PAGE, DATA) };
        assert_eq!(
            scan,
            RingScan {
                samples: 2,
                last_value: 100_042,
                lost: 1,
                throttle: 1,
                other: 1,
            }
        );
        assert_eq!(ring.tail(), off, "the ring drains");
        // SAFETY: as above.
        let again = unsafe { scan_ring_at(ring.base(), PAGE, DATA) };
        assert_eq!(again, RingScan::default(), "an empty ring counts nothing");
    }

    #[test]
    fn a_sample_whose_value_sits_past_the_wrap_is_read_whole() {
        let mut ring = FakeRing::new();
        // The header occupies the last eight bytes of the data area, so the
        // value it carries is read from the wrapped offset.
        let start = DATA as u64 - 8;
        let off = ring.push(start, PERF_RECORD_SAMPLE, 16, 0x0102_0304_0506_0708);
        ring.set(off, start);
        // SAFETY: as above.
        let scan = unsafe { scan_ring_at(ring.base(), PAGE, DATA) };
        assert_eq!(scan.samples, 1);
        assert_eq!(
            scan.last_value, 0x0102_0304_0506_0708,
            "a value past the ring wrap must be read from the wrapped offset"
        );
    }

    #[test]
    fn records_on_both_sides_of_the_wrap_are_counted() {
        let mut ring = FakeRing::new();
        let start = DATA as u64 - 16;
        let mut off = ring.push(start, PERF_RECORD_SAMPLE, 16, 7);
        off = ring.push(off, PERF_RECORD_SAMPLE, 16, 9);
        ring.set(off, start);
        // SAFETY: as above.
        let scan = unsafe { scan_ring_at(ring.base(), PAGE, DATA) };
        assert_eq!(scan.samples, 2);
        assert_eq!(scan.last_value, 9);
    }

    #[test]
    fn a_header_claiming_more_than_was_published_is_refused() {
        let mut ring = FakeRing::new();
        let off = ring.push(0, PERF_RECORD_SAMPLE, 16, 5);
        ring.push(off, PERF_RECORD_SAMPLE, 64, 0);
        let head = off + 8;
        ring.set(head, 0);
        // SAFETY: as above.
        let scan = unsafe { scan_ring_at(ring.base(), PAGE, DATA) };
        assert_eq!((scan.samples, scan.other), (1, 1));
        assert_eq!(ring.tail(), head, "the ring still drains");
    }

    #[test]
    fn a_corrupt_header_stops_the_walk_without_spinning() {
        let mut ring = FakeRing::new();
        let off = ring.push(0, PERF_RECORD_SAMPLE, 16, 5);
        ring.push(off, PERF_RECORD_SAMPLE, 0, 0);
        let head = off + 16;
        ring.set(head, 0);
        // SAFETY: as above.
        let scan = unsafe { scan_ring_at(ring.base(), PAGE, DATA) };
        assert_eq!((scan.samples, scan.other), (1, 1));
        assert_eq!(ring.tail(), head);
    }

    #[test]
    fn corrupt_cursors_are_refused_before_any_record_is_read() {
        for (head, tail) in [
            (8u64, 16u64),
            (DATA as u64 + 16, 0u64),
            (DATA as u64, DATA as u64 - 4),
            (DATA as u64 - 4, DATA as u64 - 16),
        ] {
            let mut ring = FakeRing::new();
            ring.push(0, PERF_RECORD_SAMPLE, 16, 1);
            ring.set(head, tail);
            // SAFETY: as above.
            let scan = unsafe { scan_ring_at(ring.base(), PAGE, DATA) };
            assert_eq!(
                (scan.samples, scan.other),
                (0, 1),
                "head={head} tail={tail} must be refused, not walked"
            );
            assert_eq!(ring.tail(), head);
        }
    }

    #[test]
    fn a_header_only_sample_is_counted_without_a_value() {
        let mut ring = FakeRing::new();
        let off = ring.push(0, PERF_RECORD_SAMPLE, 8, 0);
        ring.set(off, 0);
        // SAFETY: as above.
        let scan = unsafe { scan_ring_at(ring.base(), PAGE, DATA) };
        assert_eq!(scan.samples, 1);
        assert_eq!(scan.last_value, 0);
    }
}
