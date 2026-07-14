// SPDX-License-Identifier: AGPL-3.0-or-later
//! `PerfEventAttr` + the exact `BR_INST_RETIRED.CONDITIONAL` sampling config the
//! box-only PMU counter ([`crate::pmu_sys`]) opens.
//!
//! This is the **pure** half (struct + constants + builder) — no syscall, no
//! `libc` — so it compiles and is exact-value unit- + mutation-tested on **every**
//! platform, and therefore stays **inside** the coverage + mutation gates (unlike
//! the box-only syscall orchestration in [`crate::pmu_sys`], which — like
//! `kvm_sys`/`work_perf` — cannot run without `/dev/kvm` + perf and is excluded).
//! Splitting the testable config out of the FFI is the whole point: a wrong
//! attr field or a flipped flag bit is caught by CI, not only on the box.
//!
//! Productionizes task 07's measured config (identical to vmm-core's `work_perf`):
//! `PERF_TYPE_RAW` config `0x1c4`, **`exclude_host`** (count guest only → VM-exits
//! and host work between exits add zero branches), **`pinned`** (a counter that
//! fails to schedule is a hard error, never silent multiplexing), and **sampling**
//! mode (a non-zero `sample_period`) so the counter can overflow-kick `KVM_RUN`
//! (the §2 inversion seam, task 47).

/// `PERF_TYPE_RAW`.
pub(crate) const PERF_TYPE_RAW: u32 = 4;
/// `BR_INST_RETIRED.CONDITIONAL` (event `0xC4`, umask `0x01`), Coffee Lake-S
/// (i9-9900K) — the exact event task 07 validated, identical to `work_perf`.
pub(crate) const RAW_BR_COND: u64 = 0x1c4;
/// `perf_event_attr` version-5 size (112 bytes).
pub(crate) const ATTR_SIZE_VER5: u32 = 112;

// perf_event_attr flag-word bits (include/uapi/linux/perf_event.h). Bit 0 is
// written as the literal `1` (a `1 << 0` would be an un-killable equivalent mutant
// under `<<`→`>>`); the rest keep the shift, pinned to exact values by the tests.
/// `disabled` (bit 0): the counter is enabled explicitly after setup.
const F_DISABLED: u64 = 1;
/// `pinned` (bit 2): hard-fail rather than silently multiplex.
const F_PINNED: u64 = 1 << 2;
/// `exclude_host` (bit 19): count only guest-mode branches.
const F_EXCLUDE_HOST: u64 = 1 << 19;

// read_format bits.
/// `total_time_enabled` (bit 0).
const FORMAT_TOTAL_TIME_ENABLED: u64 = 1;
/// `total_time_running` (bit 1).
const FORMAT_TOTAL_TIME_RUNNING: u64 = 1 << 1;

// The two composed words use `+`, not `|`. The operands are **disjoint single
// bits**, so `+` is exactly `|` here — but it keeps the value fully mutation-pinned
// without an exclusion: the mutation oracle's `+`→`-` underflows at const-eval (an
// unviable mutant, not a survivor) and `+`→`*`/`/`/`%` all change the bits and die
// on the exact-value tests, whereas `|`→`^` on disjoint bits is an un-killable
// equivalent. (The individual bit positions are asserted above.)
/// The composed `read_format` (enabled+running, so a read can verify the pinned
/// counter actually scheduled: `time_enabled == time_running`).
pub(crate) const READ_FORMAT: u64 = FORMAT_TOTAL_TIME_ENABLED + FORMAT_TOTAL_TIME_RUNNING;
/// The composed flag word: disabled-at-open + pinned + guest-only.
pub(crate) const ATTR_FLAGS: u64 = F_DISABLED + F_PINNED + F_EXCLUDE_HOST;

/// The sampling period installed when **disarmed**: large enough that no overflow
/// fires during plain counting / single-stepping, but a valid (non-zero) sampling
/// period so the event stays in sampling mode and `PERF_EVENT_IOC_PERIOD` keeps
/// working. (`2^56` guest branches ≈ never.)
pub(crate) const DISARM_PERIOD: u64 = 1 << 56;

/// `perf_event_mmap_page` control-field byte offsets (include/uapi/linux/perf_event.h):
/// the data-ring head/tail live at fixed offsets within the ring's first (control)
/// page. Draining the consumed overflow records is `data_tail := data_head`.
/// **Box-validation:** these are the documented uapi layout (head @ 1024, tail @ 1032);
/// a uapi change would desync them. Pure constants → exact-value tested below.
pub(crate) const DATA_HEAD_OFF: usize = 1024;
pub(crate) const DATA_TAIL_OFF: usize = 1032;

// `perf_event_header.type` values (include/uapi/linux/perf_event.h) the overflow
// ring can carry for this event. Exact-value tested below.
/// `PERF_RECORD_LOST` — the kernel dropped records (ring full): a lost overflow.
pub(crate) const PERF_RECORD_LOST: u32 = 2;
/// `PERF_RECORD_THROTTLE` — the kernel throttled the event (PMIs were suppressed).
pub(crate) const PERF_RECORD_THROTTLE: u32 = 5;
/// `PERF_RECORD_UNTHROTTLE` — throttling ended.
pub(crate) const PERF_RECORD_UNTHROTTLE: u32 = 6;
/// `PERF_RECORD_SAMPLE` — one delivered overflow PMI (header-only at
/// `sample_type = 0`).
pub(crate) const PERF_RECORD_SAMPLE: u32 = 9;

/// Overflow-ring record counts for the `run_until` branch counter — the
/// **per-record overflow-multiplicity accounting** instrument of the nested-x86
/// re-certification (PR #98 review / bead hm-b5b): "every armed PMI observed
/// exactly once" must be *counted from perf records*, not inferred from landings.
/// Counts are cumulative since the counter was opened; a caller diffs two reads.
///
/// A [`samples`](Self::samples) record is one delivered overflow PMI. A nonzero
/// [`lost`](Self::lost) means the kernel dropped records (a PMI happened whose
/// record is gone); a nonzero [`throttle`](Self::throttle) means the kernel
/// suppressed PMIs for a while — both break "observed exactly once" and are loud
/// findings for the exactness harness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PmuOverflowStats {
    /// `PERF_RECORD_SAMPLE` count — delivered overflow PMIs.
    pub samples: u64,
    /// `PERF_RECORD_LOST` count — records the kernel dropped (ring full).
    pub lost: u64,
    /// `PERF_RECORD_THROTTLE` + `PERF_RECORD_UNTHROTTLE` count.
    pub throttle: u64,
    /// Any other record type (unexpected for this event; counted, never dropped).
    pub other: u64,
}

impl PmuOverflowStats {
    /// Field-wise accumulate (the box counter folds each drain into its running
    /// totals).
    pub(crate) fn add(&mut self, d: PmuOverflowStats) {
        self.samples += d.samples;
        self.lost += d.lost;
        self.throttle += d.throttle;
        self.other += d.other;
    }
}

/// Drain consumed overflow records from the perf ring mapped at `base` (1 control
/// page of `page_size` bytes + `data_size` bytes of data pages), **counting record
/// types while draining**: walk `perf_event_header`s from `data_tail` to
/// `data_head` (positions are modulo `data_size`; sizes are kernel-guaranteed
/// multiples of 8, so a header never straddles the wrap), then publish
/// `data_tail := data_head` so the single kernel producer never sees a full
/// buffer. A corrupt header size (< 8 or misaligned) stops the walk defensively —
/// counted under [`PmuOverflowStats::other`] — and the drain still completes.
///
/// Factored HERE — the pure, gate-covered half — rather than inline in the box-only
/// [`crate::pmu_sys`], so the offset math + pointer provenance is exercised by
/// `cargo miri test` (and coverage + mutation) over a TEST-OWNED ring; `pmu_sys`
/// only supplies the real `mmap`'d `base`. A bad offset, a swapped head/tail, or a
/// wrong record-type constant is then caught by Miri + the unit tests, not just on
/// the box.
///
/// # Safety
/// `base` must point to a valid, writable, 8-aligned mapping of at least
/// `page_size + data_size` bytes laid out as a perf mmap ring (control page first);
/// `page_size` ≥ `DATA_TAIL_OFF + 8`; `data_size` a power-of-two multiple of 8.
pub(crate) unsafe fn drain_ring_counting_at(
    base: *mut u8,
    page_size: usize,
    data_size: usize,
) -> PmuOverflowStats {
    let mut stats = PmuOverflowStats::default();
    // SAFETY: the caller guarantees `base` covers the control page + data area;
    // head/tail are at the documented uapi offsets, 8-aligned. Volatile on the
    // control words to defeat reordering against the kernel's writes; the acquire
    // fence orders the record reads after the head read (the kernel publishes
    // records before advancing head).
    unsafe {
        let head = std::ptr::read_volatile(base.add(DATA_HEAD_OFF).cast::<u64>());
        let mut tail = std::ptr::read_volatile(base.add(DATA_TAIL_OFF).cast::<u64>());
        std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
        // Validate the ring cursors BEFORE any record deref (PR #98 round-2):
        // head/tail are monotonic byte counters with tail <= head and at most
        // data_size bytes outstanding (the kernel stops writing when full).
        // A violated invariant means a corrupt/foreign control page — never
        // walk it; count one `other`, still publish tail := head so the ring
        // drains rather than wedging full.
        if tail > head || head - tail > data_size as u64 {
            stats.other += 1;
            std::ptr::write_volatile(base.add(DATA_TAIL_OFF).cast::<u64>(), head);
            return stats;
        }
        let data = base.add(page_size);
        while tail.saturating_add(8) <= head {
            let pos = (tail % data_size as u64) as usize;
            // perf_event_header: u32 type @ +0, u16 misc @ +4, u16 size @ +6.
            let ty = std::ptr::read_unaligned(data.add(pos).cast::<u32>());
            let size = u64::from(std::ptr::read_unaligned(data.add(pos + 6).cast::<u16>()));
            if size < 8 || size % 8 != 0 {
                // Corrupt/unparseable header: never spin; count it and stop the
                // walk (the tail still advances to head below, so the ring drains).
                stats.other += 1;
                break;
            }
            match ty {
                PERF_RECORD_SAMPLE => stats.samples += 1,
                PERF_RECORD_LOST => stats.lost += 1,
                PERF_RECORD_THROTTLE | PERF_RECORD_UNTHROTTLE => stats.throttle += 1,
                _ => stats.other += 1,
            }
            tail += size;
        }
        std::ptr::write_volatile(base.add(DATA_TAIL_OFF).cast::<u64>(), head);
    }
    stats
}

/// `perf_event_attr`, version-5 layout (112 bytes); fields beyond what we set are
/// left zero. Identical layout to `vmm-core`'s `work_perf` (no perf wrapper crate
/// is whitelisted). Fields are private; `pmu_sys` hands the whole struct to the
/// `perf_event_open` syscall and never reads them.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct PerfEventAttr {
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

/// Build the guest-only retired-conditional-branch **sampling** `perf_event_attr`
/// (`exclude_host`, `pinned`, disabled-at-open, sampling so it can overflow). Pure
/// — pinned exact-value by [`tests`].
pub(crate) fn branch_counter_attr() -> PerfEventAttr {
    PerfEventAttr {
        type_: PERF_TYPE_RAW,
        size: ATTR_SIZE_VER5,
        config: RAW_BR_COND,
        // Sampling mode (non-zero period) so the counter can overflow; disarmed far
        // out until `arm_overflow` sets the real period.
        sample_period: DISARM_PERIOD,
        // `sample_type` is left at its `Default` (0): we never read sample records,
        // only want the overflow wakeup. (Not written explicitly — an explicit
        // `sample_type: 0` would be a deleteable-but-equivalent mutant.)
        read_format: READ_FORMAT,
        flags: ATTR_FLAGS,
        // Wake (and signal) after a single overflow record.
        wakeup_events: 1,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each bit constant is the exact uapi value — a `<<`→`>>` flip (which would
    /// zero the higher bits) changes the asserted value and dies here.
    #[test]
    fn bit_constants_are_the_exact_uapi_values() {
        assert_eq!(PERF_TYPE_RAW, 4);
        assert_eq!(RAW_BR_COND, 0x1c4);
        assert_eq!(ATTR_SIZE_VER5, 112);
        assert_eq!(F_DISABLED, 0x1); // bit 0
        assert_eq!(F_PINNED, 0x4); // bit 2
        assert_eq!(F_EXCLUDE_HOST, 0x8_0000); // bit 19
        assert_eq!(FORMAT_TOTAL_TIME_ENABLED, 0x1); // bit 0
        assert_eq!(FORMAT_TOTAL_TIME_RUNNING, 0x2); // bit 1
        assert_eq!(DISARM_PERIOD, 0x0100_0000_0000_0000); // 1 << 56
        // The composed words (built with `+` over disjoint bits — see the const
        // comment); these exact values kill the surviving `+`→`*`/`/`/`%` mutants.
        assert_eq!(READ_FORMAT, 0b11);
        assert_eq!(ATTR_FLAGS, 0x8_0005);
        // And the composition is exactly OR of the documented bits.
        assert_eq!(
            READ_FORMAT,
            FORMAT_TOTAL_TIME_ENABLED | FORMAT_TOTAL_TIME_RUNNING
        );
        assert_eq!(ATTR_FLAGS, F_DISABLED | F_PINNED | F_EXCLUDE_HOST);
    }

    const PAGE: usize = 4096;
    const DATA: usize = 4096;

    /// A u64-aligned TEST-OWNED stand-in for the perf mmap ring (1 control page +
    /// `DATA` data bytes), with a writer for fabricated `perf_event_header`s.
    struct FakeRing {
        buf: Vec<u64>,
    }
    impl FakeRing {
        fn new() -> Self {
            Self {
                buf: vec![0u64; (PAGE + DATA) / 8],
            }
        }
        fn base(&mut self) -> *mut u8 {
            self.buf.as_mut_ptr().cast::<u8>()
        }
        /// Write a header {type, size} at ring offset `off` (mod DATA) and return
        /// `off + size` (the next record's offset).
        fn push(&mut self, off: u64, ty: u32, size: u16) -> u64 {
            let pos = PAGE + (off % DATA as u64) as usize;
            let bytes = &mut self.buf;
            // header word: u32 type | u16 misc | u16 size, little-endian.
            bytes[pos / 8] = u64::from(ty) | (u64::from(size) << 48);
            off + u64::from(size)
        }
        fn set(&mut self, head: u64, tail: u64) {
            self.buf[DATA_HEAD_OFF / 8] = head;
            self.buf[DATA_TAIL_OFF / 8] = tail;
        }
        fn head(&self) -> u64 {
            self.buf[DATA_HEAD_OFF / 8]
        }
        fn tail(&self) -> u64 {
            self.buf[DATA_TAIL_OFF / 8]
        }
    }

    /// `drain_ring_counting_at` counts record types while draining, over a
    /// TEST-OWNED ring, so the box-only drain's offset math + pointer access runs
    /// under `cargo miri test` (real provenance + alignment) and the coverage +
    /// mutation gates — no longer a vacuous unsafe gate reachable only via the
    /// `cfg(miri)`-stubbed `PmuBranchCounter::open`.
    #[test]
    fn drain_counts_each_record_type_and_advances_tail_to_head() {
        let mut ring = FakeRing::new();
        let mut off = 0u64;
        off = ring.push(off, PERF_RECORD_SAMPLE, 8);
        off = ring.push(off, PERF_RECORD_THROTTLE, 24);
        off = ring.push(off, PERF_RECORD_SAMPLE, 8);
        off = ring.push(off, PERF_RECORD_LOST, 16);
        off = ring.push(off, 3 /* PERF_RECORD_COMM: "other" */, 8);
        ring.set(off, 0);
        // SAFETY: `buf` is u64-aligned and covers PAGE + DATA bytes; `base` is its
        // sole live pointer for the duration of the call.
        let stats = unsafe { drain_ring_counting_at(ring.base(), PAGE, DATA) };
        assert_eq!(
            stats,
            PmuOverflowStats {
                samples: 2,
                lost: 1,
                throttle: 1,
                other: 1,
            }
        );
        assert_eq!(ring.tail(), off, "tail advanced to head (records drained)");
        assert_eq!(
            ring.head(),
            off,
            "head is untouched (drain only writes tail)"
        );
        // A second drain sees nothing new.
        let again = unsafe { drain_ring_counting_at(ring.base(), PAGE, DATA) };
        assert_eq!(again, PmuOverflowStats::default(), "empty ring counts zero");
        // The offsets + record types are the documented uapi values.
        assert_eq!(DATA_HEAD_OFF, 1024);
        assert_eq!(DATA_TAIL_OFF, 1032);
        assert_eq!(PERF_RECORD_LOST, 2);
        assert_eq!(PERF_RECORD_THROTTLE, 5);
        assert_eq!(PERF_RECORD_UNTHROTTLE, 6);
        assert_eq!(PERF_RECORD_SAMPLE, 9);
    }

    /// The byte offsets wrap modulo the data size (head/tail are monotonic byte
    /// counters): records around the wrap point are still counted.
    #[test]
    fn drain_counts_across_the_ring_wrap() {
        let mut ring = FakeRing::new();
        let start = DATA as u64 - 8; // last slot before the wrap
        let mut off = start;
        off = ring.push(off, PERF_RECORD_SAMPLE, 8); // at DATA-8
        off = ring.push(off, PERF_RECORD_SAMPLE, 8); // wraps to pos 0
        ring.set(off, start);
        // SAFETY: as above.
        let stats = unsafe { drain_ring_counting_at(ring.base(), PAGE, DATA) };
        assert_eq!(stats.samples, 2, "both sides of the wrap counted");
        assert_eq!(ring.tail(), off);
    }

    /// A corrupt header (size < 8) can never spin the walk: it is counted under
    /// `other`, the walk stops, and the ring still drains (tail := head).
    #[test]
    fn drain_stops_defensively_on_a_corrupt_header_but_still_drains() {
        let mut ring = FakeRing::new();
        let mut off = 0u64;
        off = ring.push(off, PERF_RECORD_SAMPLE, 8);
        ring.push(off, PERF_RECORD_SAMPLE, 0); // corrupt: size 0
        let head = off + 16;
        ring.set(head, 0);
        // SAFETY: as above.
        let stats = unsafe { drain_ring_counting_at(ring.base(), PAGE, DATA) };
        assert_eq!(
            stats.samples, 1,
            "records before the corruption are counted"
        );
        assert_eq!(stats.other, 1, "the corrupt header is counted, not spun on");
        assert_eq!(ring.tail(), head, "the ring still drains fully");
    }

    /// Corrupt ring cursors (tail > head, or more than `data_size` bytes
    /// outstanding) are refused BEFORE any record deref: one `other` counted,
    /// no walk, and the ring still drains (tail := head).
    #[test]
    fn drain_refuses_corrupt_cursors_without_walking() {
        for (head, tail) in [(8u64, 16u64), (DATA as u64 + 16, 0u64)] {
            let mut ring = FakeRing::new();
            ring.push(0, PERF_RECORD_SAMPLE, 8);
            ring.set(head, tail);
            // SAFETY: as above.
            let stats = unsafe { drain_ring_counting_at(ring.base(), PAGE, DATA) };
            assert_eq!(
                (stats.samples, stats.other),
                (0, 1),
                "corrupt cursors (head={head}, tail={tail}) are refused, not walked"
            );
            assert_eq!(ring.tail(), head, "the ring still drains (tail := head)");
        }
    }

    /// `PmuOverflowStats::add` accumulates field-wise (the box counter folds each
    /// drain into its running totals).
    #[test]
    fn overflow_stats_add_is_field_wise() {
        let mut a = PmuOverflowStats {
            samples: 1,
            lost: 2,
            throttle: 3,
            other: 4,
        };
        a.add(PmuOverflowStats {
            samples: 10,
            lost: 20,
            throttle: 30,
            other: 40,
        });
        assert_eq!(
            a,
            PmuOverflowStats {
                samples: 11,
                lost: 22,
                throttle: 33,
                other: 44,
            }
        );
    }

    /// The built attr is the exact `BR_INST_RETIRED.CONDITIONAL` sampling config —
    /// a deleted/changed field is caught by the precise assertion (the mutation
    /// survivors PR #15 flagged: delete `type_`/`size`/`config`/`sample_period`…).
    #[test]
    fn branch_counter_attr_is_the_exact_br_cond_sampling_config() {
        let a = branch_counter_attr();
        assert_eq!(a.type_, 4, "PERF_TYPE_RAW");
        assert_eq!(a.size, 112, "perf_event_attr ver5 size");
        assert_eq!(
            a.config, 0x1c4,
            "BR_INST_RETIRED.CONDITIONAL (event 0xC4 umask 0x01)"
        );
        assert_eq!(
            a.sample_period, 0x0100_0000_0000_0000,
            "disarmed sampling period (sampling mode, no early overflow)"
        );
        assert_eq!(a.sample_type, 0, "no sample records — wakeup only");
        assert_eq!(
            a.read_format, 0b11,
            "TOTAL_TIME_ENABLED|RUNNING (multiplex check)"
        );
        assert_eq!(a.flags, 0x8_0005, "disabled | pinned | exclude_host");
        assert_eq!(a.wakeup_events, 1, "wake after one overflow");
        // Every field we do NOT set defaults to 0 (a stray non-zero would change
        // the perf semantics).
        assert_eq!(
            (
                a.bp_type,
                a.sample_stack_user,
                a.aux_watermark,
                a.sample_max_stack,
                a.reserved_2,
            ),
            (0, 0, 0, 0, 0)
        );
        assert_eq!(a.clockid, 0);
        for z in [
            a.config1,
            a.config2,
            a.branch_sample_type,
            a.sample_regs_user,
            a.sample_regs_intr,
        ] {
            assert_eq!(z, 0, "reserved perf_event_attr field must be zero");
        }
    }
}
