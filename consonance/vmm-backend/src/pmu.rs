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

/// Drain consumed overflow records from a perf ring-buffer control page at `base`:
/// copy `data_head` → `data_tail` so the single kernel producer never sees a full
/// buffer. A plain volatile load/store suffices (single producer = the kernel, single
/// consumer = the owning thread; we only need the tail to advance).
///
/// Factored HERE — the pure, gate-covered half — rather than inline in the box-only
/// [`crate::pmu_sys`], so the offset math + volatile pointer provenance is exercised
/// by `cargo miri test` (and coverage + mutation) over a TEST-OWNED page; `pmu_sys`
/// only supplies the real `mmap`'d `base`. A bad offset or a swapped head/tail is then
/// caught by Miri + the unit test, not just on the box (it was previously reachable
/// only via the `cfg(miri)`-stubbed `PmuBranchCounter::open`, a vacuous unsafe gate).
///
/// # Safety
/// `base` must point to at least `DATA_TAIL_OFF + 8` bytes of a valid, writable,
/// 8-aligned mapping (the perf control page is page-aligned and ≥ 4 KiB).
pub(crate) unsafe fn drain_ring_at(base: *mut u8) {
    // SAFETY: the caller guarantees `base` covers the control page; head/tail are at
    // the documented uapi offsets, 8-aligned. Volatile to defeat reordering against
    // the kernel's writes.
    unsafe {
        let head = std::ptr::read_volatile(base.add(DATA_HEAD_OFF).cast::<u64>());
        std::ptr::write_volatile(base.add(DATA_TAIL_OFF).cast::<u64>(), head);
    }
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

    /// `drain_ring_at` copies `data_head` → `data_tail` over a TEST-OWNED control
    /// page, so the box-only ring drain's offset math + volatile pointer access runs
    /// under `cargo miri test` (real provenance + alignment) and the coverage +
    /// mutation gates — no longer a vacuous unsafe gate reachable only via the
    /// `cfg(miri)`-stubbed `PmuBranchCounter::open`.
    #[test]
    fn drain_ring_at_copies_head_to_tail() {
        // A u64-aligned in-process stand-in for the perf control page (≥ DATA_TAIL_OFF
        // + 8 bytes). All accesses go through `base` (single provenance for Miri).
        let mut page = vec![0u64; (DATA_TAIL_OFF / 8) + 1];
        let base = page.as_mut_ptr().cast::<u8>();
        let head_val = 0xdead_beef_0000_1234u64;
        // SAFETY: `page` is u64-aligned and covers DATA_TAIL_OFF + 8 bytes; `base` is
        // its sole live pointer for the duration of this block.
        unsafe {
            std::ptr::write_volatile(base.add(DATA_HEAD_OFF).cast::<u64>(), head_val);
            std::ptr::write_volatile(base.add(DATA_TAIL_OFF).cast::<u64>(), 0);
            drain_ring_at(base);
            assert_eq!(
                std::ptr::read_volatile(base.add(DATA_TAIL_OFF).cast::<u64>()),
                head_val,
                "tail advanced to head (records drained)"
            );
            assert_eq!(
                std::ptr::read_volatile(base.add(DATA_HEAD_OFF).cast::<u64>()),
                head_val,
                "head is untouched (drain only writes the tail)"
            );
        }
        // The offsets are the documented uapi layout.
        assert_eq!(DATA_HEAD_OFF, 1024);
        assert_eq!(DATA_TAIL_OFF, 1032);
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
