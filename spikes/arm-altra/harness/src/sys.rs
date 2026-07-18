// SPDX-License-Identifier: AGPL-3.0-or-later
//! The syscall seam: `perf_event_open` and the KVM ioctls.
//!
//! This is the whole crate's `unsafe`, and the syscalls are Linux-only by
//! construction — the development Mac cannot issue them. Everything above this
//! line (scanning, ELF reading, console decoding, planning, the `KVM_RUN`
//! orchestration loop, evidence emission) is pure logic and is tested natively.
//!
//! # The split inside this module
//!
//! The module has two halves, and the line between them is deliberate:
//!
//! - **The ABI, portable.** The `perf_event_attr` layout and its packed flag bits,
//!   the KVM ioctl numbers, and the `kvm_run` field offsets are *data*. They are
//!   compiled on every host and pinned by unit tests that run natively on the Mac
//!   — because the one bug this seam cannot afford is arming the wrong thing: a
//!   flag constant off by a bit opens an event that is neither pinned nor
//!   host-excluding and reports the AA-0 row green anyway (a multiplexed,
//!   host-inclusive counter mislabelled as the work clock). The tests below assert
//!   every flag against its documented bit position, so a future struct-shuffle
//!   cannot silently reintroduce that.
//! - **The syscalls, Linux-only.** [`Machine`] (KVM: create VM, map memory, create
//!   the single vCPU, `KVM_RUN`) and [`PerfCounter`] (raw `BR_RETIRED`, armed
//!   guest-only and pinned) implement the [`crate::run`] seams with real ioctls.
//!   They compile for `aarch64-unknown-linux-gnu` and **have never run**: the Altra
//!   is not yet in hand. Written out so arrival day is `scp + run`, not authoring.
//!
//! # Why the seam is this thin
//!
//! `docs/ARM-ALTRA.md` §Evidence integrity #4 requires that a silent fallback
//! (signal-kick instead of the patched exit) be *structurally unable* to masquerade
//! as the mechanism under test. Keeping this layer to "issue the ioctl, return
//! exactly what the kernel returned" — no interpretation, no retry, no smoothing —
//! is what lets the layers above attest the mechanism honestly: the exit reason in
//! an evidence record is the one the kernel actually returned, not one this code
//! decided was close enough.

// This module is the crate's sole `unsafe`. The crate is `deny(unsafe_code)`; this
// is the one explicit, audited opt-in.
#![allow(unsafe_code)]

use crate::evidence::PerfConfig;

/// The raw `BR_RETIRED` event on aarch64 PMUv3: all architecturally executed branch
/// instructions, taken or not (N1 finding AA1-F1; `docs/ARM-PORT.md`,
/// `docs/ARM-ALTRA.md` §2). This ARM binding does not change the x86 clock. The event is
/// surfaced as a constant so the harness cannot silently arm a different one.
pub const BR_RETIRED_RAW: u64 = 0x21;

/// `PERF_TYPE_RAW` — the event is a raw PMU event number, not a generic alias.
/// (The generic `PERF_COUNT_HW_BRANCH_INSTRUCTIONS` is *not* this event: it maps to
/// a different aarch64 counter. Arming it would be the silent substitution the
/// evidence rules forbid.)
pub const PERF_TYPE_RAW: u32 = 4;

/// The `perf_event_attr` size this crate builds: `PERF_ATTR_SIZE_VER6`. The kernel
/// validates `attr.size` against the ABI's known sizes, so [`PerfEventAttr`] must
/// be a byte-exact prefix of the kernel's struct — pinned by a test below.
pub const PERF_ATTR_SIZE_VER6: u32 = 120;

/// The packed bitfield of `struct perf_event_attr`, by **documented bit position**.
///
/// The field order in `include/uapi/linux/perf_event.h` is
/// `disabled(0), inherit(1), pinned(2), exclusive(3), exclude_user(4),
/// exclude_kernel(5), exclude_hv(6), exclude_idle(7), mmap(8), comm(9), …,
/// exclude_host(19), exclude_guest(20)`. Every constant here is pinned to its bit
/// by [`tests::perf_flag_bits_match_the_kernel_abi`]: an off-by-one in this table
/// does not fail loudly, it opens a *different event* and reports it green.
pub mod perf_flags {
    /// The event starts disabled and must be enabled before it counts.
    pub const DISABLED: u64 = 1 << 0;
    /// Children inherit the event.
    pub const INHERIT: u64 = 1 << 1;
    /// **Pinned**: the event must always be on the PMU. A non-pinned event can be
    /// *multiplexed*, and a multiplexed counter scales its count — which would
    /// silently corrupt every measurement the work clock rests on.
    pub const PINNED: u64 = 1 << 2;
    /// Exclusive PMU access. Not what [`PINNED`] means, and the bit next door to
    /// it: the confusion this table exists to prevent.
    pub const EXCLUSIVE: u64 = 1 << 3;
    /// Do not count EL0.
    pub const EXCLUDE_USER: u64 = 1 << 4;
    /// Do not count EL1.
    pub const EXCLUDE_KERNEL: u64 = 1 << 5;
    /// Do not count the hypervisor (EL2).
    pub const EXCLUDE_HV: u64 = 1 << 6;
    /// Do not count the idle task.
    pub const EXCLUDE_IDLE: u64 = 1 << 7;
    /// **Do not count host execution** — with [`EXCLUDE_GUEST`] clear, this is what
    /// makes the count guest-only, which is the whole of AA-1(b)'s "count guest-only
    /// (host-excluded attribution)".
    pub const EXCLUDE_HOST: u64 = 1 << 19;
    /// Do not count guest execution. The *inverse* of what the work clock wants;
    /// named here so a manifest that claims it can be checked against it.
    pub const EXCLUDE_GUEST: u64 = 1 << 20;
}

/// `struct perf_event_attr`, the fields this harness sets. Zeroed otherwise.
///
/// A byte-exact prefix of the kernel's struct through `PERF_ATTR_SIZE_VER6`; the
/// kernel zero-extends anything past `size`.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct PerfEventAttr {
    /// `PERF_TYPE_*`.
    pub type_: u32,
    /// `size_of::<PerfEventAttr>()`, checked by the kernel against the ABI sizes.
    pub size: u32,
    /// The event number — [`BR_RETIRED_RAW`] for the work clock.
    pub config: u64,
    /// `sample_period` (or `sample_freq`): the overflow deadline, in events.
    pub sample_period_or_freq: u64,
    /// `sample_type`.
    pub sample_type: u64,
    /// `read_format`.
    pub read_format: u64,
    /// The packed bitfield — see [`perf_flags`].
    pub flags: u64,
    /// `wakeup_events` / `wakeup_watermark`.
    pub wakeup: u32,
    /// `bp_type`.
    pub bp_type: u32,
    /// `bp_addr` / `config1`.
    pub bp_addr_or_config1: u64,
    /// `bp_len` / `config2`.
    pub bp_len_or_config2: u64,
    /// `branch_sample_type`.
    pub branch_sample_type: u64,
    /// `sample_regs_user`.
    pub sample_regs_user: u64,
    /// `sample_stack_user`.
    pub sample_stack_user: u32,
    /// `clockid`.
    pub clockid: i32,
    /// `sample_regs_intr`.
    pub sample_regs_intr: u64,
    /// `aux_watermark`.
    pub aux_watermark: u32,
    /// `sample_max_stack`.
    pub sample_max_stack: u16,
    /// Reserved; must be zero.
    pub reserved_2: u16,
    /// `aux_sample_size`.
    pub aux_sample_size: u32,
    /// Reserved; must be zero.
    pub reserved_3: u32,
}

/// The work-clock event: raw `BR_RETIRED`, **pinned** (never multiplexed) and
/// **guest-only** (`exclude_host`), per AA-1(b/c).
///
/// `sample_period` arms a one-shot overflow that many events out; `None` is
/// counting mode (AA-1(b)), where no deadline is armed.
///
/// The event is opened **disabled** and enabled explicitly, so the counter's zero
/// is a moment the harness chose rather than whenever the fd happened to open.
///
/// Note what is *not* set: `exclude_hv`, `exclude_kernel`, `exclude_user`. Whether
/// N1's PMU filtering additionally needs an EL2 exclusion for a clean guest-only
/// attribution is an **AA-0/AA-1 finding**, not something this apparatus may assume
/// — `docs/ARM-ALTRA.md` §Execution constraints: never silently substitute an
/// arming strategy. If the box says otherwise, the constant to change is here and
/// the manifest will say what was armed, because [`perf_config`] derives the
/// evidence from this very attr.
#[must_use]
pub fn br_retired_attr(sample_period: Option<u64>) -> PerfEventAttr {
    PerfEventAttr {
        type_: PERF_TYPE_RAW,
        size: PERF_ATTR_SIZE_VER6,
        config: BR_RETIRED_RAW,
        sample_period_or_freq: sample_period.unwrap_or(0),
        flags: perf_flags::DISABLED | perf_flags::PINNED | perf_flags::EXCLUDE_HOST,
        ..Default::default()
    }
}

/// The AA-1(a) host-side counting event: raw `BR_RETIRED`, **pinned**, counting
/// **EL0 of this thread only** — `exclude_kernel` + `exclude_hv` so
/// scheduler/IRQ/hypervisor branches never inflate the count, and NO
/// `exclude_host` (there is no guest; a guest-only counter over a host loop reads
/// exactly zero — the round-5 probe lesson). Opened disabled; the tool encloses
/// the window call in an explicit enable/disable pair, whose EL0 tail is part of
/// the per-class constant offset AA-1(a) measures.
#[must_use]
pub fn el0_count_attr() -> PerfEventAttr {
    PerfEventAttr {
        type_: PERF_TYPE_RAW,
        size: PERF_ATTR_SIZE_VER6,
        config: BR_RETIRED_RAW,
        // PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING: read()
        // returns [count, enabled, running], and the checker demands
        // enabled == running (pinned events are never silently multiplexed).
        read_format: 0b11,
        flags: perf_flags::DISABLED
            | perf_flags::PINNED
            | perf_flags::EXCLUDE_KERNEL
            | perf_flags::EXCLUDE_HV,
        ..Default::default()
    }
}

/// The evidence record of what was armed — **derived from the attr itself**, never
/// hand-written beside it.
///
/// This is a structural anti-fabrication measure: the manifest's `perf` block
/// cannot claim `pinned: true, exclude_host: true` while the fd was opened with
/// neither, because the claim is a projection of the bits that went to the kernel.
/// The floor checker then validates those bits independently
/// (`schemas/floor-check`, the `perf-config` check).
#[must_use]
pub fn perf_config(attr: &PerfEventAttr) -> PerfConfig {
    PerfConfig {
        raw_event: attr.config,
        exclude_host: attr.flags & perf_flags::EXCLUDE_HOST != 0,
        exclude_guest: attr.flags & perf_flags::EXCLUDE_GUEST != 0,
        exclude_hv: attr.flags & perf_flags::EXCLUDE_HV != 0,
        pinned: attr.flags & perf_flags::PINNED != 0,
        sample_period: (attr.sample_period_or_freq != 0).then_some(attr.sample_period_or_freq),
    }
}

/// The KVM ioctl numbers this harness issues, and the two constants the
/// 0004-analogue patch adds (`host/patches/`).
///
/// Encoded per `asm-generic/ioctl.h` (`dir << 30 | size << 16 | type << 8 | nr`,
/// with `KVMIO == 0xAE`). Pinned by [`tests::kvm_ioctl_numbers_match_the_abi`]: a
/// wrong ioctl number is an `EINVAL` on the box, which is a bad way to spend
/// arrival day.
pub mod kvm {
    /// `KVM_GET_API_VERSION` — `_IO(KVMIO, 0x00)`.
    pub const GET_API_VERSION: u64 = 0xAE00;
    /// `KVM_CREATE_VM` — `_IO(KVMIO, 0x01)`.
    pub const CREATE_VM: u64 = 0xAE01;
    /// `KVM_CHECK_EXTENSION` — `_IO(KVMIO, 0x03)`.
    pub const CHECK_EXTENSION: u64 = 0xAE03;
    /// `KVM_GET_VCPU_MMAP_SIZE` — `_IO(KVMIO, 0x04)`.
    pub const GET_VCPU_MMAP_SIZE: u64 = 0xAE04;
    /// `KVM_CREATE_VCPU` — `_IO(KVMIO, 0x41)`.
    pub const CREATE_VCPU: u64 = 0xAE41;
    /// `KVM_SET_USER_MEMORY_REGION` — `_IOW(KVMIO, 0x46, struct kvm_userspace_memory_region)`.
    pub const SET_USER_MEMORY_REGION: u64 = 0x4020_AE46;
    /// `KVM_RUN` — `_IO(KVMIO, 0x80)`.
    pub const RUN: u64 = 0xAE80;
    /// `KVM_GET_ONE_REG` — `_IOW(KVMIO, 0xab, struct kvm_one_reg)`. **Note the
    /// direction: `_IOW`, not `_IOR`.** KVM's get-register ioctl is encoded write —
    /// userspace *writes* the `kvm_one_reg` descriptor (id + a pointer to where the
    /// value goes) into the kernel; the kernel fills the pointed-at buffer, not the
    /// struct. A `_IOR` encoding (`0x8010_AEAB`) is simply a different, unknown ioctl
    /// number and returns `ENOTTY`, so every real `state_digest` read fails.
    pub const GET_ONE_REG: u64 = 0x4010_AEAB;
    /// `KVM_SET_ONE_REG` — `_IOW(KVMIO, 0xac, struct kvm_one_reg)`.
    pub const SET_ONE_REG: u64 = 0x4010_AEAC;
    /// `KVM_ARM_VCPU_INIT` — `_IOW(KVMIO, 0xae, struct kvm_vcpu_init)`.
    pub const ARM_VCPU_INIT: u64 = 0x4020_AEAE;
    /// `KVM_ARM_PREFERRED_TARGET` — `_IOR(KVMIO, 0xaf, struct kvm_vcpu_init)`.
    pub const ARM_PREFERRED_TARGET: u64 = 0x8020_AEAF;
    /// `KVM_ARM_VCPU_PSCI_0_2` feature-bit index for `KVM_ARM_VCPU_INIT`.
    pub const ARM_VCPU_PSCI_0_2: u32 = 2;
    /// `KVM_ARM_VCPU_PMU_V3` — `kvm_vcpu_init.features[0]` bit index enabling the vPMU
    /// (uapi `asm/kvm.h`). Only the disposable ID-reading vCPU sets it: without it KVM
    /// masks the guest-visible `ID_AA64DFR0_EL1.PMUVer` to 0, hiding the host PMU
    /// version the truth table records (found on harmony-arm day one). The measurement
    /// vCPU keeps it OFF — the guest contract denies the guest a PMU.
    pub const VCPU_FEATURE_PMU_V3: u32 = 3;
    /// `KVM_GET_REG_LIST` — `_IOWR(KVMIO, 0xb0, struct kvm_reg_list)`.
    pub const GET_REG_LIST: u64 = 0xC008_AEB0;
    /// `KVM_ENABLE_CAP` — `_IOW(KVMIO, 0xa3, struct kvm_enable_cap)`.
    ///
    /// Checking that a capability is *advertised* is not the same as *enabling* it.
    /// The 0004-analogue patch gates `KVM_ARM_PREEMPT_EXIT` on
    /// `KVM_ARCH_FLAG_DETERMINISTIC_INTERCEPTS`, which is set only through this
    /// ioctl — so without it every arm returns `EINVAL`, on the patched kernel.
    pub const ENABLE_CAP: u64 = 0x4068_AEA3;
    /// `KVM_CREATE_DEVICE` — `_IOWR(KVMIO, 0xe0, struct kvm_create_device)`.
    pub const CREATE_DEVICE: u64 = 0xC00C_AEE0;
    /// `KVM_SET_DEVICE_ATTR` — `_IOW(KVMIO, 0xe1, struct kvm_device_attr)`.
    pub const SET_DEVICE_ATTR: u64 = 0x4018_AEE1;
    /// `KVM_ARM_PREEMPT_EXIT` — `_IO(KVMIO, 0xe4)`, **added by the 0004-analogue
    /// patch draft** (`host/patches/0001-…`): arms the one-shot in-kernel
    /// force-exit. Absent from a stock kernel, which is the point.
    pub const ARM_PREEMPT_EXIT: u64 = 0xAEE4;

    /// `KVM_SET_GUEST_DEBUG` — `_IOW(KVMIO, 0x9b, struct kvm_guest_debug)`, the ioctl
    /// AA-2's single-step run path arms guest debug through.
    ///
    /// **The size field is arch-specific, and that is load-bearing.** The kernel
    /// dispatches ioctls with a `switch (ioctl)` over the *full* 32-bit command number,
    /// whose case label is the macro expanded against the target arch's
    /// `struct kvm_guest_debug`. On **arm64** that struct is 520 bytes (`0x208`):
    /// `control:u32 + pad:u32` (8) plus `struct kvm_guest_debug_arch`, which is
    /// `dbg_bcr/bvr/wcr/wvr[16]` — 64×`u64` = 512. So the encoded size is `0x208`, giving
    /// `_IOW(0xAE, 0x9b, 0x208)` = `0x4208_AE9B`. (x86's `kvm_guest_debug_arch` is
    /// `debugreg[8]` → 72 bytes → `0x4048_AE9B`; passing that number on arm64 matches no
    /// case and returns `ENOTTY`.) Derived from — and pinned to — the arm64 struct size by
    /// [`tests::kvm_ioctl_numbers_match_the_abi`] and [`machine::tests`]'s `size_of` check.
    // TODO(box-verify): confirm the running arm64 kernel accepts KVM_SET_GUEST_DEBUG at
    // 0x4208_AE9B (no EINVAL/ENOTTY) and that `sizeof(struct kvm_guest_debug) == 0x208`
    // there (arch/arm64/include/uapi/asm/kvm.h KVM_ARM_MAX_DBG_REGS == 16). AA2-BUILD.md
    // quoted the x86 number (0x4048_AE9B); this is the arm64-correct value.
    pub const SET_GUEST_DEBUG: u64 = 0x4208_AE9B;
    /// `KVM_GUESTDBG_ENABLE` (0x1) — the `kvm_guest_debug.control` bit that turns guest
    /// debug on at all. Generic (identical on every arch, `include/uapi/linux/kvm.h`).
    pub const GUESTDBG_ENABLE: u32 = 0x0000_0001;
    /// `KVM_GUESTDBG_SINGLESTEP` (0x2) — the `control` bit that makes each guest
    /// instruction trap out with `KVM_EXIT_DEBUG`. Generic across arches.
    pub const GUESTDBG_SINGLESTEP: u32 = 0x0000_0002;
    /// The `kvm_guest_debug.control` value AA-2 arms: enable guest debug AND single-step,
    /// with no hardware breakpoints/watchpoints programmed (the `arch` array stays zero).
    pub const GUESTDBG_SINGLESTEP_CONTROL: u32 = GUESTDBG_ENABLE | GUESTDBG_SINGLESTEP;

    /// `KVM_DEV_TYPE_ARM_VGIC_V3` — the in-kernel GICv3 the guest needs to exist at
    /// all. The payload runtime programs the distributor at `0x0800_0000` before it
    /// prints a byte; with no vGIC those stores are MMIO exits to userspace, which
    /// the measurement loop (rightly) refuses as non-console traffic. Nothing boots
    /// without this device.
    pub const DEV_TYPE_ARM_VGIC_V3: u32 = 7;
    /// `KVM_DEV_ARM_VGIC_GRP_ADDR`.
    pub const DEV_ARM_VGIC_GRP_ADDR: u32 = 0;
    /// `KVM_DEV_ARM_VGIC_GRP_CTRL`.
    pub const DEV_ARM_VGIC_GRP_CTRL: u32 = 4;
    /// `KVM_DEV_ARM_VGIC_GRP_DIST_REGS` = **1** — the distributor register save group.
    /// The `attr` is the GICD register offset. The distributor holds the **SPI** state
    /// (interrupt IDs ≥ 32); the SGI/PPI (IDs 0–31) enable/pending/active words are
    /// RAZ/WI here on GICv3 — they live in the redistributor (see [`DEV_ARM_VGIC_GRP_REDIST_REGS`]).
    pub const DEV_ARM_VGIC_GRP_DIST_REGS: u32 = 1;
    /// `KVM_DEV_ARM_VGIC_GRP_REDIST_REGS` = **5** — the redistributor register save
    /// group. The **private-interrupt** (SGI/PPI, IDs 0–31) enable/pending/active state
    /// lives in the redistributor's SGI frame (`RD_base + 0x1_0000`), and that is where
    /// the timer PPIs land — so the injection state AA-6 exercises is read from here,
    /// not the distributor. The `attr` low bits are the offset from `RD_base`; the high
    /// 32 bits are the target vCPU's `mpidr` (0 for the single-affinity spike guest).
    pub const DEV_ARM_VGIC_GRP_REDIST_REGS: u32 = 5;
    /// `KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS` = **6** — the CPU-interface (ICC_* system
    /// register) save group: the priority mask, group enables, and active-priority
    /// registers that decide how a pending interrupt is DELIVERED, and which are not in the
    /// redistributor/distributor groups nor the generic vCPU register list. The `attr` low
    /// 16 bits are the register's `(op0<<14 | op1<<11 | crn<<7 | crm<<3 | op2)` instruction
    /// encoding; the high 32 bits are the target vCPU's `mpidr` (0 for the spike guest).
    /// These registers are 64-bit (unlike the DIST/REDIST offsets, read 32-bit).
    pub const DEV_ARM_VGIC_GRP_CPU_SYSREGS: u32 = 6;
    /// `KVM_DEV_ARM_VGIC_CTRL_INIT`.
    pub const DEV_ARM_VGIC_CTRL_INIT: u64 = 0;
    /// `KVM_GET_DEVICE_ATTR` — `_IOW(KVMIO, 0xe2, struct kvm_device_attr)`, the same
    /// `_IOW` direction as `SET_DEVICE_ATTR`: userspace writes the `kvm_device_attr`
    /// (group/attr + a pointer to the value buffer) and the kernel fills the buffer.
    /// A `_IOWR` encoding (`0xC018_AEE2`) is an unknown ioctl and returns `ENOTTY`,
    /// so the vGIC state read fails and no digest is produced.
    pub const GET_DEVICE_ATTR: u64 = 0x4018_AEE2;
    /// `KVM_VGIC_V3_ADDR_TYPE_DIST`.
    pub const VGIC_V3_ADDR_TYPE_DIST: u64 = 2;
    /// `KVM_VGIC_V3_ADDR_TYPE_REDIST`.
    pub const VGIC_V3_ADDR_TYPE_REDIST: u64 = 3;

    /// The GICv3 distributor base the payload runtime programs
    /// (`payloads/runtime/src/gic.rs`, the QEMU `virt` map).
    pub const GICD_BASE: u64 = 0x0800_0000;
    /// The GICv3 redistributor base (one 128 KiB frame pair per vCPU).
    pub const GICR_BASE: u64 = 0x080A_0000;

    /// `KVM_CAP_SET_GUEST_DEBUG` — the single-step capability AA-2 rests on.
    pub const CAP_SET_GUEST_DEBUG: u64 = 23;
    /// `KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS` (245) — the capability the
    /// 0004-analogue patch adds. Its *presence* is the positive proof that the
    /// patched kernel is the one running (§Evidence integrity #4), and its absence
    /// on a stock kernel is a legitimate, recordable "no".
    pub const CAP_ARM_DETERMINISTIC_INTERCEPTS: u64 = 245;

    /// `KVM_EXIT_DEBUG` — a single-step landed (AA-2).
    pub const EXIT_DEBUG: u32 = 4;
    /// `KVM_EXIT_MMIO` — the guest touched the one MMIO door (the PL011).
    pub const EXIT_MMIO: u32 = 6;
    /// `KVM_EXIT_INTR` — a host signal kicked the vCPU out. AA-1(c)'s pre-patch
    /// mechanism, and AA-3's forbidden fallback.
    pub const EXIT_INTR: u32 = 10;
    /// `KVM_EXIT_PREEMPT` (42) — the patched in-kernel force-exit. Added by the
    /// patch draft; a stock kernel can never return it, which is exactly why the
    /// records may attest the mechanism by it.
    pub const EXIT_PREEMPT: u32 = 42;

    /// `KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM_CORE` — the prefix of the
    /// core-register ids (`user_pt_regs`), whose `pc` this harness sets.
    pub const REG_ARM64_CORE_U64: u64 = 0x6030_0000_0010_0000;

    /// Byte offset of `pc` within `struct kvm_regs`.
    ///
    /// `struct kvm_regs` opens with `struct user_pt_regs { __u64 regs[31]; __u64 sp;
    /// __u64 pc; __u64 pstate; }`, so: `regs[31]` fills `0x00..0xF8`, `sp` sits at
    /// `0xF8`, and **`pc` is at `0x100`**. The next field, `sp_el1`, is at `0x110`.
    pub const REG_CORE_PC_OFFSET: u64 = 0x100;

    /// Byte offset of `pstate` within `struct kvm_regs`.
    pub const REG_CORE_PSTATE_OFFSET: u64 = REG_CORE_PC_OFFSET + 8;

    /// `KVM_REG_ARM_CORE_REG(regs.regs[0])`.
    pub const REG_CORE_X0: u64 = 0;

    /// Distance between adjacent 64-bit GPR register indices. KVM's arm-core
    /// register macro divides byte offsets by `sizeof(u32)`, so each `u64` GPR
    /// advances by two indices, not one.
    pub const REG_CORE_X_STRIDE: u64 = 8 / 4;

    /// `KVM_REG_ARM_CORE_REG(regs.pc)` — the register index, which the macro defines
    /// as the byte offset divided by four: `0x100 / 4 == 0x40`.
    ///
    /// This was `0x44` — the index of the field at byte `0x110`, which is `sp_el1`.
    /// Setting the entry point therefore wrote the EL1 stack pointer and left `PC` at
    /// its reset value, so the guest never entered the payload at all. The constant is
    /// now *derived* from the offset and pinned by a test, because an off-by-one in a
    /// register index does not fail loudly — it writes a different register.
    pub const REG_CORE_PC: u64 = REG_CORE_PC_OFFSET / 4;

    /// `KVM_REG_ARM_CORE_REG(regs.pstate)`.
    pub const REG_CORE_PSTATE: u64 = REG_CORE_PSTATE_OFFSET / 4;
    /// The size field of a register id (`KVM_REG_SIZE_MASK`).
    pub const REG_SIZE_MASK: u64 = 0x00F0_0000_0000_0000;
    /// Shift of the size field.
    pub const REG_SIZE_SHIFT: u32 = 52;

    /// `ID_AA64PFR0_EL1` as a `KVM_GET/SET_ONE_REG` id.
    ///
    /// `KVM_REG_ARM64 (0x6…) | KVM_REG_SIZE_U64 (0x…30…) | KVM_REG_ARM64_SYSREG | enc`.
    /// The SYSREG selector is `0x0013 << KVM_REG_ARM_COPROC_SHIFT(16)`; `enc` packs the
    /// architected `(op0,op1,crn,crm,op2) = (3,0,0,4,0)` of `ID_AA64PFR0_EL1` by the
    /// KVM shifts (`op0<<14 | op1<<11 | crn<<7 | crm<<3 | op2<<0`), which is
    /// `(3<<14)|(4<<3) = 0xC020`. The AA-0 `writable-id-registers` probe reads this
    /// register and writes the same value back: a kernel that pins ID registers
    /// read-only fails the `SET` with `EINVAL`, and that failure *is* the (absent) row —
    /// so the value is derived here and pinned by a test rather than typed as a magic
    /// literal, since a wrong id would `SET` a *different* register and read green.
    pub const REG_ID_AA64PFR0_EL1: u64 =
        0x6000_0000_0000_0000 | 0x0030_0000_0000_0000 | (0x0013 << 16) | (3 << 14) | (4 << 3);

    /// The common prefix of a 64-bit `KVM_GET_ONE_REG` id for an AArch64 sysreg:
    /// `KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM64_SYSREG`.
    const SYSREG_U64: u64 = 0x6000_0000_0000_0000 | 0x0030_0000_0000_0000 | (0x0013 << 16);
    /// Encode an AArch64 system register id from its `(op0,op1,crn,crm,op2)`.
    #[must_use]
    pub const fn arm64_sysreg(op0: u64, op1: u64, crn: u64, crm: u64, op2: u64) -> u64 {
        SYSREG_U64 | (op0 << 14) | (op1 << 11) | (crn << 7) | (crm << 3) | op2
    }

    /// The feature ID registers AA-0's truth table reads (and `MIDR_EL1` for identity), by
    /// their architected `(op0,op1,crn,crm,op2)`.
    pub const REG_MIDR_EL1: u64 = arm64_sysreg(3, 0, 0, 0, 0);
    /// `ID_AA64PFR1_EL1` — processor feature register 1 (BT, SSBS, MTE, …).
    pub const REG_ID_AA64PFR1_EL1: u64 = arm64_sysreg(3, 0, 0, 4, 1);
    /// `ID_AA64DFR0_EL1` — PMUVer lives here.
    pub const REG_ID_AA64DFR0_EL1: u64 = arm64_sysreg(3, 0, 0, 5, 0);
    /// `ID_AA64DFR1_EL1` — debug feature register 1.
    pub const REG_ID_AA64DFR1_EL1: u64 = arm64_sysreg(3, 0, 0, 5, 1);
    /// `ID_AA64ISAR0_EL1` — Atomic (LSE) lives here.
    pub const REG_ID_AA64ISAR0_EL1: u64 = arm64_sysreg(3, 0, 0, 6, 0);
    /// `ID_AA64ISAR1_EL1` — instruction-set attrs 1 (DPB, JSCVT, LRCPC, GPA, …).
    pub const REG_ID_AA64ISAR1_EL1: u64 = arm64_sysreg(3, 0, 0, 6, 1);
    /// `ID_AA64ISAR2_EL1` — instruction-set attrs 2.
    pub const REG_ID_AA64ISAR2_EL1: u64 = arm64_sysreg(3, 0, 0, 6, 2);
    /// `ID_AA64MMFR0_EL1` — ECV lives here.
    pub const REG_ID_AA64MMFR0_EL1: u64 = arm64_sysreg(3, 0, 0, 7, 0);
    /// `ID_AA64MMFR1_EL1` — VH (VHE) lives here (bits[11:8]).
    pub const REG_ID_AA64MMFR1_EL1: u64 = arm64_sysreg(3, 0, 0, 7, 1);
    /// `ID_AA64MMFR2_EL1` — NV (FEAT_NV, nested virt) lives here (bits[35:32]).
    pub const REG_ID_AA64MMFR2_EL1: u64 = arm64_sysreg(3, 0, 0, 7, 2);

    /// `VBAR_EL1` — the EL1 exception vector base (`S3_0_C12_C0_0`, i.e.
    /// `(op0,op1,crn,crm,op2) = (3,0,12,0,0)`). AA-2's single-step transition classifier
    /// reads it to tell an SVC/abort **exception entry** (`pc_after` in the synchronous
    /// vector slot) from an injected-IRQ **injection** boundary (`pc_after` in the IRQ
    /// slot) — a distinction PC arithmetic alone cannot make.
    pub const REG_VBAR_EL1: u64 = arm64_sysreg(3, 0, 12, 0, 0);
}

/// `struct kvm_run`, through the MMIO arm of its exit union.
///
/// Only the fields this loop reads are modelled; the kernel's struct is larger and
/// the mapping is `KVM_GET_VCPU_MMAP_SIZE` bytes long. The field offsets are pinned
/// by [`tests::kvm_run_field_offsets_match_the_abi`], because an offset that drifts
/// reads `exit_reason` out of the padding and turns every exit into a
/// mystery — or worse, into a plausible one.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct KvmRun {
    /// In: request an interrupt window.
    pub request_interrupt_window: u8,
    /// In: exit immediately, without entering the guest.
    pub immediate_exit: u8,
    /// Padding to the `exit_reason` word.
    pub padding1: [u8; 6],
    /// Out: `KVM_EXIT_*`.
    pub exit_reason: u32,
    /// Out.
    pub ready_for_interrupt_injection: u8,
    /// Out.
    pub if_flag: u8,
    /// Out.
    pub flags: u16,
    /// In/out (x86; present in the shared prefix).
    pub cr8: u64,
    /// In/out (x86; present in the shared prefix).
    pub apic_base: u64,
    /// The MMIO arm of the exit union, at offset 32.
    pub mmio: KvmRunMmio,
}

/// The `mmio` arm of `kvm_run`'s exit union.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct KvmRunMmio {
    /// The guest-physical address touched.
    pub phys_addr: u64,
    /// The bytes written (or to be filled, on a read).
    pub data: [u8; 8],
    /// The access width.
    pub len: u32,
    /// Nonzero for a write.
    pub is_write: u8,
    /// Padding to the union's alignment.
    pub padding: [u8; 3],
}

// -- The vCPU-exit pointer logic, portable and Miri-reachable --
//
// The KVM harness ([`machine`]) is Linux-only, so the interpreter — which runs on the
// Mac — cannot execute its ioctls. The pointer/field logic those ioctls hand off to,
// though, is pure: decoding a `kvm_run` snapshot into an exit, staging an MMIO read
// value into the shared buffer, and hashing a state. Those live here, operate on
// plain references, and are driven under Miri by [`tests`] against an in-process
// `KvmRun` — the in-process loopback the unsafe⇒Miri contract asks for. `machine`
// forms the references from its mapped pointer and calls straight through.

/// Decode a `kvm_run` snapshot into a [`crate::run::VcpuExit`], with no interpretation
/// beyond the exit-reason match — the mechanism a record attests is the one the kernel
/// set (`docs/ARM-ALTRA.md` §Evidence integrity #4).
///
/// Pure and Miri-testable: the field reads and the bounded `data` slice are exactly
/// the operations that would be unsafe against a live mapping, driven here against a
/// value.
#[must_use]
pub fn decode_kvm_run(run: &KvmRun) -> crate::run::VcpuExit {
    use crate::run::VcpuExit;
    match run.exit_reason {
        kvm::EXIT_MMIO => {
            let len = run.mmio.len as usize;
            if len == 0 || len > run.mmio.data.len() {
                return VcpuExit::MalformedMmio {
                    addr: run.mmio.phys_addr,
                    width: run.mmio.len,
                };
            }
            VcpuExit::Mmio {
                addr: run.mmio.phys_addr,
                data: run.mmio.data[..len].to_vec(),
                is_write: run.mmio.is_write != 0,
            }
        }
        kvm::EXIT_PREEMPT => VcpuExit::Preempt,
        kvm::EXIT_INTR => VcpuExit::SignalKick,
        kvm::EXIT_DEBUG => VcpuExit::Debug,
        other => VcpuExit::Other(other),
    }
}

/// Stage the value of an MMIO **read** into a `kvm_run`'s shared `mmio.data`, so the
/// next `KVM_RUN` resumes the guest with it. Copies at most `data.len()` bytes, capped
/// at the 8-byte buffer — the bounds check that keeps a wide read from writing past the
/// field. Pure and Miri-testable.
pub fn stage_mmio_read(run: &mut KvmRun, data: &[u8]) {
    let n = data.len().min(run.mmio.data.len());
    run.mmio.data[..n].copy_from_slice(&data[..n]);
}

/// Volatile read-modify-write of an MMIO-read result into a mapped `kvm_run`, so the next
/// `KVM_RUN` resumes the guest with it. Split out of `Machine::complete_mmio_read` — which is
/// Linux-only and whose `self.run` is a real `MAP_SHARED` mapping the interpreter cannot build
/// — so the raw-pointer path (the `read_volatile` of the mapped struct, the `mmio.data` field
/// projection and cast, and the byte-wise `write_volatile`) is exercisable under **Miri**
/// against an allocation-backed `kvm_run` (see `tests::write_mmio_read_stages_over_an_allocation`).
///
/// # Safety
/// `run` must point at a live, writable [`KvmRun`] (a `MAP_SHARED` mapping on the box, or a
/// boxed one under test) that no other thread writes concurrently while this runs.
pub unsafe fn write_mmio_read(run: *mut KvmRun, data: &[u8]) {
    // SAFETY: the caller guarantees `run` is a live writable `KvmRun`. Snapshot it, stage the
    // read into the snapshot via the bounded portable seam, and write the `mmio.data` bytes
    // back volatilely — the kernel reads them on re-entry.
    unsafe {
        let mut snapshot = core::ptr::read_volatile(run);
        stage_mmio_read(&mut snapshot, data);
        let dst = (&raw mut (*run).mmio.data).cast::<u8>();
        for (i, b) in snapshot.mmio.data.iter().enumerate() {
            core::ptr::write_volatile(dst.add(i), *b);
        }
    }
}

/// Borrow guest RAM as a byte slice for hashing. Split out of `Machine::state_digest` so the
/// `from_raw_parts` — its provenance and length reasoning — is Miri-exercisable against an
/// allocation-backed buffer (see `tests::guest_ram_borrows_the_allocation`).
///
/// # Safety
/// `mem` must point at `len` initialised, readable bytes that no other thread writes while the
/// borrow is live (on the box the vCPU is stopped between exits).
#[must_use]
pub unsafe fn guest_ram<'a>(mem: *const u8, len: usize) -> &'a [u8] {
    // SAFETY: the caller guarantees `len` readable bytes at `mem`, unwritten for the borrow.
    unsafe { core::slice::from_raw_parts(mem, len) }
}

/// Read the little-endian 32-bit instruction word at guest-physical `addr` from a borrow of
/// guest RAM based at `ram_base`. `None` when `addr` is below the base, or the 4-byte word
/// would run past the mapping — the AA-2 stepper decodes the *stepped* opcode from this word,
/// and a `pc_before` that fell outside the mapped slot (a wild PC after a bad step) must fail
/// closed, never read plausible bytes from padding or panic.
///
/// Pure and bounds-checked, so the pointer path that produces `ram` ([`guest_ram`]) is the only
/// `unsafe`; this decode runs against a plain slice and is exercised under Miri
/// (`tests::guest_word_reads_the_bounded_opcode`).
#[must_use]
pub fn guest_word(ram: &[u8], ram_base: u64, addr: u64) -> Option<u32> {
    let off = usize::try_from(addr.checked_sub(ram_base)?).ok()?;
    let end = off.checked_add(4)?;
    let bytes = ram.get(off..end)?;
    // `bytes` is exactly 4 long, so the array conversion cannot fail.
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Extract the GNU build-id (lowercase hex) from a little-endian ELF64 image's `PT_NOTE`
/// segments — the build-id of the artifact ON DISK, so the caller can bind the hashed kernel
/// image to the RUNNING kernel's build-id (`/sys/kernel/notes`). Hashing the file and reading
/// the running build-id are otherwise independent facts; the file's own build-id is what links
/// the pinned artifact to the kernel that booted.
///
/// `None` if the bytes are not a little-endian ELF64 or carry no build-id note (e.g. a stripped
/// `/boot/Image`, which is a flat binary with no notes) — the caller treats that as "cannot
/// bind: supply the vmlinux ELF". Every field access is bounds- and overflow-checked: the input
/// is an untrusted file.
#[must_use]
pub fn elf_gnu_build_id(bytes: &[u8]) -> Option<String> {
    // ELF magic, ELFCLASS64 (2), ELFDATA2LSB (1).
    if bytes.get(0..4)? != b"\x7fELF" || *bytes.get(4)? != 2 || *bytes.get(5)? != 1 {
        return None;
    }
    // Every offset+width is `checked_add`ed before forming a slice range: `bytes` is an
    // untrusted operator-supplied kernel image, and a hostile `e_phoff`/`p_offset` near
    // `usize::MAX` would otherwise overflow the `o + width` and panic in an overflow-checked
    // build. The contract is return-None, never panic.
    let u16le = |o: usize| -> Option<usize> {
        Some(u16::from_le_bytes(bytes.get(o..o.checked_add(2)?)?.try_into().ok()?) as usize)
    };
    let u32le = |o: usize| -> Option<u32> {
        Some(u32::from_le_bytes(
            bytes.get(o..o.checked_add(4)?)?.try_into().ok()?,
        ))
    };
    let uoff = |o: usize| -> Option<usize> {
        usize::try_from(u64::from_le_bytes(
            bytes.get(o..o.checked_add(8)?)?.try_into().ok()?,
        ))
        .ok()
    };
    // ELF64 header: e_phoff@32, e_phentsize@54, e_phnum@56.
    let phoff = uoff(32)?;
    let phentsize = u16le(54)?;
    let phnum = u16le(56)?;
    if phentsize < 56 {
        return None; // a well-formed ELF64 program header is 56 bytes
    }
    for i in 0..phnum {
        let ph = phoff.checked_add(i.checked_mul(phentsize)?)?;
        // Program header: p_type@0, p_offset@8, p_filesz@32 — every field offset checked.
        if u32le(ph)? != 4 {
            continue; // not PT_NOTE
        }
        let p_offset = uoff(ph.checked_add(8)?)?;
        let p_filesz = uoff(ph.checked_add(32)?)?;
        let notes = bytes.get(p_offset..p_offset.checked_add(p_filesz)?)?;
        if let Some(id) = parse_gnu_build_id(notes) {
            return Some(id);
        }
    }
    None
}

/// Hash a landed guest state — every architectural register (in sorted id order),
/// all of guest RAM, and the in-kernel vGIC's distributor state — into the digest
/// AA-3's replay-identity and AA-6's bit-identity floors compare.
///
/// The `vgic` bytes are the vGIC distributor register save-state (the injection-
/// relevant enable/pending/active bits). Two AA-6 repetitions that differ only in
/// pending/active/injected interrupt state carry identical vCPU registers and RAM, so
/// without the vGIC state they would digest identically and a real injection
/// divergence would be accepted as replay-identical. (AA-6's own investigation is
/// which vGIC state round-trips bit-identically, `KVM_DEV_ARM_VGIC_GRP_*`; this hashes
/// the distributor registers it retrieves.)
///
/// Registers are a `BTreeMap` so iteration order (which reaches the hashed bytes) is
/// the register id, never insertion order (Conventions rule 4). Pure and Miri-testable:
/// `machine` reads the registers by ioctl, forms the RAM slice from its mapping, and
/// dumps the vGIC state, then hands all three here — so the hashing itself, and the
/// order discipline, are interpreter-checked.
#[must_use]
pub fn digest_state(
    regs: &std::collections::BTreeMap<u64, Vec<u8>>,
    ram: &[u8],
    vgic: &[u8],
) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"arm-spike-state-v2");
    for (id, value) in regs {
        // Host-time-derived registers (the generic-timer counters) advance with real
        // elapsed time, so hashing them would make two otherwise-identical same-seed
        // runs digest differently the moment host scheduling differs — replay identity
        // dead on arrival. They are excluded: they are not contract-visible
        // deterministic state (the paravirt-clock design closes raw counter access at
        // the contract level, §1), and what AA-3/AA-6 compare is the guest's
        // deterministic state, not the wall clock.
        if is_host_time_register(*id) {
            continue;
        }
        h.update(id.to_le_bytes());
        h.update(value);
    }
    h.update(ram);
    // The vGIC distributor state (enable/pending/active) — the injection axis AA-6
    // exercises. Length-prefixed so an empty dump can never collide with a one-byte one.
    h.update((vgic.len() as u64).to_le_bytes());
    h.update(vgic);
    format!("sha256:{}", crate::evidence::hex_lower(&h.finalize()))
}

/// Hash a guest state's **registers and vGIC only** — the cheap per-step replay key
/// AA-2's single-step run path stamps on every step but the last, WITHOUT the
/// guest-RAM slice [`digest_state`] hashes.
///
/// This exists because a full [`digest_state`] reads the *whole* guest-RAM slot every
/// call (4 MiB — the AA-1(c) measurement that shrank [`crate::sys::machine::RAM_SIZE`]),
/// and single-stepping calls the digest once **per instruction**: full-hashing RAM every
/// step makes a stepped run infeasible. So an intermediate step hashes only the register
/// (and vGIC) state — the same inputs [`digest_state`] reads *minus* the RAM slice — and
/// only the run's FINAL step pays the full-RAM cost (`step_run`), so memory divergence
/// anywhere in the stepped window is still caught end-to-end by that boundary hash.
///
/// A **distinct domain tag** (`arm-spike-regs-v1`, not `digest_state`'s
/// `arm-spike-state-v2`) so a registers-only digest can never collide with a full one
/// even over byte-identical registers — the two are compared side by side (intermediate
/// vs final step), and they must be structurally unequal, not merely unequal-in-practice.
/// The same host-time-register exclusion and length-prefixed vGIC discipline as
/// [`digest_state`]; registers in sorted id order (a `BTreeMap`, Conventions rule 4).
/// Pure and Miri-testable — no `unsafe`; `machine` reads the registers/vGIC by ioctl and
/// hands them here, so the hashing and order discipline are interpreter-checked.
#[must_use]
pub fn digest_regs_only(regs: &std::collections::BTreeMap<u64, Vec<u8>>, vgic: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"arm-spike-regs-v1");
    for (id, value) in regs {
        // The generic-timer counters advance with real time; hashing them would make two
        // same-seed steps digest differently the moment host scheduling differs — the same
        // exclusion `digest_state` makes, for the same reason.
        if is_host_time_register(*id) {
            continue;
        }
        h.update(id.to_le_bytes());
        h.update(value);
    }
    // The vGIC injection state, length-prefixed exactly as `digest_state` does, so an
    // empty dump can never collide with a one-byte one.
    h.update((vgic.len() as u64).to_le_bytes());
    h.update(vgic);
    format!("sha256:{}", crate::evidence::hex_lower(&h.finalize()))
}

/// Whether a `KVM_GET_ONE_REG` id names a **host-time-derived** register — one whose
/// value advances with elapsed real time and so must not enter a determinism digest.
///
/// These are the generic-timer *counters*: the physical `CNTPCT_EL0`, the two
/// self-synchronized `…SS` variants, and the live `KVM_REG_ARM_TIMER_CNT` pseudo-register.
/// All live at the arm64 system-register coordinates `op0=3, op1=3, CRn=14`. The
/// *comparators* (`…CVAL`), *controls* (`…CTL`, `CNTKCTL`), and the constant
/// `CNTFRQ` are deterministic guest-programmed state and are kept — and note the KVM
/// one-reg remap: id `ARM64_SYS_REG(3,3,14,0,2)`, the architectural `CNTVCT_EL0` slot, is
/// `KVM_REG_ARM_TIMER_CVAL` (the programmed deadline) in a real register list, so it too is
/// kept; the live counter is `KVM_REG_ARM_TIMER_CNT` at `(3,3,14,3,2)`.
///
/// Pinned by [`tests::host_time_registers_are_the_generic_timer_counters`]; a wrong
/// mask would either leak the wall clock into the digest (replay dead) or drop real
/// guest state (divergence hidden), and neither fails loudly.
#[must_use]
pub fn is_host_time_register(id: u64) -> bool {
    // Only arm64 system registers carry timer counters. The register id packs, from
    // the top: the architecture (`KVM_REG_ARM64`, bits 56–63), the value size
    // (`KVM_REG_SIZE_U64`, bits 52–55), and the coprocessor selector
    // (`KVM_REG_ARM_COPROC_MASK`, bits **16–27**) — `KVM_REG_ARM64_SYSREG` is
    // `0x0013 << 16`, in the LOW selector bits, NOT bit 48. Encoding it at bit 48 (as
    // an earlier draft did) makes this predicate match no real register id, so every
    // live CNTVCT/CNTPCT read leaks into the digest and same-seed replay dies.
    const KVM_REG_ARM64: u64 = 0x6000_0000_0000_0000;
    const KVM_REG_ARCH_MASK: u64 = 0xFF00_0000_0000_0000;
    const KVM_REG_SIZE_U64: u64 = 0x0030_0000_0000_0000;
    const KVM_REG_SIZE_MASK: u64 = 0x00F0_0000_0000_0000;
    const KVM_REG_ARM_COPROC_MASK: u64 = 0x0FFF_0000;
    const KVM_REG_ARM64_SYSREG: u64 = 0x0013 << 16;
    if id & KVM_REG_ARCH_MASK != KVM_REG_ARM64 {
        return false;
    }
    if id & KVM_REG_SIZE_MASK != KVM_REG_SIZE_U64 {
        return false;
    }
    if id & KVM_REG_ARM_COPROC_MASK != KVM_REG_ARM64_SYSREG {
        return false;
    }
    let op0 = (id >> 14) & 0x3;
    let op1 = (id >> 11) & 0x7;
    let crn = (id >> 7) & 0xF;
    let crm = (id >> 3) & 0xF;
    let op2 = id & 0x7;
    if (op0, op1, crn) != (3, 3, 14) {
        return false;
    }
    // The live host-time counters to exclude, by (CRm, op2):
    //   CNTPCT_EL0   = (0, 1)   CNTPCTSS_EL0 = (0, 5)   CNTVCTSS_EL0 = (0, 6)
    //   KVM_REG_ARM_TIMER_CNT = ARM64_SYS_REG(3,3,14,3,2) = (3, 2)
    // NOT (0, 2): the KVM one-reg ABI REMAPS the timer pseudo-registers off the architectural
    // sysreg encodings, so id ARM64_SYS_REG(3,3,14,0,2) — which architecturally names
    // CNTVCT_EL0 — is KVM_REG_ARM_TIMER_CVAL, the guest's PROGRAMMED virtual-timer deadline
    // (deterministic guest state), while the live counter is KVM_REG_ARM_TIMER_CNT at (3, 2).
    // KVM enumerates the timer as these pseudo-registers, not the architectural counters, so
    // the only (0, 2) entry in a real register list is CVAL and it is KEPT — excluding it (an
    // earlier draft did) drops the programmed deadline, so two guests whose only difference is
    // their virtual-timer deadline would hash identically. The controls/comparators/frequency
    // (CNTFRQ (0,0), *CTL, CVAL, CNTKCTL) are deterministic guest state and are NOT excluded.
    matches!((crm, op2), (0, 1) | (0, 5) | (0, 6) | (3, 2))
}

/// Whether a PMU `events/<name>` sysfs value encodes the given raw event number.
///
/// The sysfs contents are a comma-separated term list like `event=0x21` (or
/// `event=0x21,umask=0x0`); only the `event=` term carries the event id. This is the
/// PMCEID-backed proof `probe_br_retired_implemented` uses, factored out here so it is
/// unit-testable off the box. (Only the Linux probe calls it; on other hosts it is reached
/// solely by its native test.)
#[must_use]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn sysfs_event_encodes(contents: &str, raw_event: u64) -> bool {
    contents
        .trim()
        .split(',')
        .filter_map(|term| term.trim().strip_prefix("event="))
        .any(|v| parse_hex_or_dec(v.trim()) == Some(raw_event))
}

/// Parse an unsigned integer written as `0x..` hex or plain decimal.
#[must_use]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_hex_or_dec(s: &str) -> Option<u64> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u64>().ok()
    }
}

/// Parse the GNU build-id (hex) out of an ELF notes blob (`/sys/kernel/notes`).
///
/// The kernel exposes its own `.notes` section here; the `NT_GNU_BUILD_ID` note (type 3,
/// name `"GNU"`) is a per-build fingerprint of the RUNNING image — the boot measurement that
/// identifies which kernel is actually executing, independent of any file on disk. Notes are
/// `namesz|descsz|type|name(4-padded)|desc(4-padded)` in native endianness (the harness runs
/// on the box, so native == the kernel's). Factored out here so it is testable off the box.
#[must_use]
pub fn parse_gnu_build_id(notes: &[u8]) -> Option<String> {
    let mut off = 0usize;
    while off + 12 <= notes.len() {
        let namesz = u32::from_ne_bytes(notes[off..off + 4].try_into().ok()?) as usize;
        let descsz = u32::from_ne_bytes(notes[off + 4..off + 8].try_into().ok()?) as usize;
        let ntype = u32::from_ne_bytes(notes[off + 8..off + 12].try_into().ok()?);
        let name_start = off + 12;
        let name_end = name_start.checked_add(namesz)?;
        let name = notes.get(name_start..name_end)?;
        // 4-byte alignment padding after the name.
        let desc_start = name_end.checked_add(namesz.wrapping_neg() & 3)?;
        let desc_end = desc_start.checked_add(descsz)?;
        let desc = notes.get(desc_start..desc_end)?;
        if ntype == 3 && name.starts_with(b"GNU") && !desc.is_empty() {
            let mut hex = String::with_capacity(desc.len() * 2);
            for b in desc {
                use core::fmt::Write;
                let _ = write!(hex, "{b:02x}");
            }
            return Some(hex);
        }
        off = desc_end.checked_add(descsz.wrapping_neg() & 3)?;
    }
    None
}

/// The GNU build-id of the RUNNING kernel, from `/sys/kernel/notes`. `Ok(None)` when the
/// file is absent (not a Linux box, or a kernel built without a build-id).
///
/// # Errors
/// [`SysError`] if the file exists but cannot be read.
pub fn running_kernel_build_id() -> Result<Option<String>, SysError> {
    match std::fs::read("/sys/kernel/notes") {
        Ok(notes) => Ok(parse_gnu_build_id(&notes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(SysError::Protocol(
            "cannot read /sys/kernel/notes to identify the running kernel".to_string(),
        )),
    }
}

/// Count the CPUs in a Linux CPU-list string (`/sys/devices/system/cpu/online`), e.g.
/// `"0-79"` or `"0,2-4,7"`. Factored out so it is testable off the box.
///
/// # Errors
/// [`SysError::Protocol`] if the counted set overflows a `u32` — the content is host input,
/// so a full-width range like `0-4294967295` (whose `b - a + 1` overflows), or many valid
/// ranges that sum past `u32::MAX`, is a protocol error, never a panic.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_cpu_list(s: &str) -> Result<u32, SysError> {
    let overflow = || SysError::Protocol(format!("online-CPU list {s:?} overflows a u32 count"));
    let mut count = 0u32;
    for part in s.trim().split(',') {
        if part.is_empty() {
            continue;
        }
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<u32>(), b.trim().parse::<u32>())
                    && b >= a
                {
                    // `b - a` cannot underflow (b >= a); `+ 1` and the running sum can both
                    // overflow on hostile input, so both are checked.
                    count = (b - a)
                        .checked_add(1)
                        .and_then(|span| count.checked_add(span))
                        .ok_or_else(overflow)?;
                }
            }
            None if part.trim().parse::<u32>().is_ok() => {
                count = count.checked_add(1).ok_or_else(overflow)?;
            }
            None => {}
        }
    }
    Ok(count)
}

/// The machine's ONLINE CPU count, from `/sys/devices/system/cpu/online`.
///
/// NOT `available_parallelism()`, which reflects the calling process's affinity or cgroup
/// CPU allowance — under `taskset`/a systemd CPU set/a leased housekeeping partition that
/// records the lease size (possibly 1), not the Altra's real topology.
///
/// # Errors
/// [`SysError`] if the online-CPU set cannot be read.
pub fn online_cpu_count() -> Result<u32, SysError> {
    match std::fs::read_to_string("/sys/devices/system/cpu/online") {
        Ok(s) => parse_cpu_list(&s),
        Err(_) => Err(SysError::Protocol(
            "cannot read /sys/devices/system/cpu/online to count the machine's online CPUs"
                .to_string(),
        )),
    }
}

/// The host's EFFECTIVE KVM mode (`"vhe"`/`"nvhe"`/`"protected"`) — the mode KVM actually
/// selected at boot, not the architectural VHE feature bit.
///
/// Two surfaces, in order:
///
/// 1. `/sys/module/kvm_arm/parameters/mode` — where the kernel exposes it. On the kernels
///    this apparatus has actually met (Ubuntu 6.8 arm64), `kvm-arm.mode` is an
///    `early_param` and **no such sysfs node exists**, so this path alone reported every
///    real box as "unknown".
/// 2. The kernel's own boot log via `klogctl(SYSLOG_ACTION_READ_ALL)`, parsed by
///    [`parse_kvm_mode_from_log`]: `kvm_arm_init()` prints exactly one of three
///    mode-initialized lines at boot, and that line *is* the effective mode, said by the
///    kernel itself — not an operator claim. Reading the ring buffer needs
///    `kernel.dmesg_restrict=0` or `CAP_SYSLOG`; a permission refusal (or the line having
///    rotated out of the buffer) degrades to `Ok(None)`, never to a guess.
///
/// `Ok(None)` when neither surface can say.
///
/// # Errors
/// [`SysError`] if the sysfs parameter exists but cannot be read.
pub fn kvm_mode() -> Result<Option<String>, SysError> {
    match std::fs::read_to_string("/sys/module/kvm_arm/parameters/mode") {
        Ok(s) => Ok(Some(s.trim().to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(imp_kernel_log().and_then(|log| parse_kvm_mode_from_log(&log)))
        }
        Err(_) => Err(SysError::Protocol(
            "cannot read /sys/module/kvm_arm/parameters/mode".to_string(),
        )),
    }
}

/// Parse the effective KVM mode out of the kernel boot log.
///
/// `arch/arm64/kvm/arm.c:kvm_arm_init()` prints exactly one of three lines when KVM
/// initializes (6.8's set, unchanged for years):
///
/// - `kvm [1]: VHE mode initialized successfully` → `vhe`
/// - `kvm [1]: Hyp mode initialized successfully` → `nvhe` (the nVHE print)
/// - `kvm [1]: Protected nVHE mode initialized successfully` → `protected`
///
/// Only these exact phrases map; anything else is `None` — an unrecognized line is not
/// evidence of a mode, and inventing one here would poison every manifest downstream.
/// The LAST match wins: `SYSLOG_ACTION_READ_ALL` returns the buffer in order, and a
/// wrapped buffer could in principle hold a stale partial line only earlier, never later.
pub fn parse_kvm_mode_from_log(log: &str) -> Option<String> {
    let mut mode = None;
    for line in log.lines() {
        // The kvm prefix guards against an unrelated line quoting the phrase.
        if !line.contains("kvm") {
            continue;
        }
        if line.contains("Protected nVHE mode initialized successfully") {
            mode = Some("protected".to_string());
        } else if line.contains("VHE mode initialized successfully") {
            mode = Some("vhe".to_string());
        } else if line.contains("Hyp mode initialized successfully") {
            mode = Some("nvhe".to_string());
        }
    }
    mode
}

/// Read the kernel ring buffer (`klogctl(SYSLOG_ACTION_READ_ALL)`), or `None` when it
/// cannot be read (no privilege, or off Linux). Never an error: the caller treats an
/// unreadable log as "this surface cannot say", exactly like an absent sysfs node.
#[cfg(target_os = "linux")]
fn imp_kernel_log() -> Option<String> {
    const SYSLOG_ACTION_READ_ALL: libc::c_int = 3;
    const SYSLOG_ACTION_SIZE_BUFFER: libc::c_int = 10;
    // SAFETY: SIZE_BUFFER takes no buffer; READ_ALL writes at most `len` bytes into the
    // provided buffer and returns how many it wrote.
    unsafe {
        let size = libc::klogctl(SYSLOG_ACTION_SIZE_BUFFER, std::ptr::null_mut(), 0);
        if size <= 0 {
            return None;
        }
        // Bound the allocation: a hostile/corrupt size must not OOM the harness.
        let len = (size as usize).min(1 << 24);
        let mut buf = vec![0u8; len];
        let read = libc::klogctl(
            SYSLOG_ACTION_READ_ALL,
            buf.as_mut_ptr().cast::<libc::c_char>(),
            len as libc::c_int,
        );
        if read < 0 {
            return None;
        }
        buf.truncate(read as usize);
        Some(String::from_utf8_lossy(&buf).into_owned())
    }
}

#[cfg(not(target_os = "linux"))]
fn imp_kernel_log() -> Option<String> {
    None
}

/// The running kernel's `uname -r` release string.
///
/// # Errors
/// [`SysError`] if `uname` failed.
#[cfg(target_os = "linux")]
pub fn running_kernel_release() -> Result<String, SysError> {
    // SAFETY: a zeroed utsname is valid; uname fills it, and we read only the NUL-terminated
    // `release` field.
    unsafe {
        let mut u: libc::utsname = core::mem::zeroed();
        if libc::uname(&raw mut u) != 0 {
            return Err(SysError::Protocol("uname() failed".to_string()));
        }
        let release = core::ffi::CStr::from_ptr(u.release.as_ptr());
        Ok(release.to_string_lossy().into_owned())
    }
}

/// A capability the running kernel either has or does not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Capability {
    /// `/dev/kvm` is present and openable.
    DevKvm,
    /// `perf_event_open` of raw `BR_RETIRED` as a pinned, guest-only event
    /// succeeds.
    PerfBrRetired,
    /// `KVM_CAP_SET_GUEST_DEBUG` (single-step) is advertised.
    GuestDebug,
    /// `BR_RETIRED` (event 0x21) is a **PMCEID-implemented** PMU event — the PMU exposes
    /// an `events/br_retired` (= `event=0x21`) file, which the PMUv3 driver gates on the
    /// PMCEID bitmap. NOT merely a clean `perf_event_open` (ARM perf accepts arbitrary raw
    /// encodings that then never count). The truth table's `br-retired-pmceid1` row: the
    /// whole work-clock bet rests on this event existing on N1's PMU.
    Pmceid,
    /// A **host-side overflow actually delivers**: arm a small `BR_RETIRED` sample period,
    /// run a branchy loop past it, and confirm the kernel wrote an overflow sample. AA-1's
    /// existential row — a counter that increments but never *overflows a sample* cannot
    /// arm a deadline.
    HostOverflowDelivers,
    /// The in-kernel **GICv3 is creatable** (`KVM_CREATE_DEVICE`, `KVM_DEV_TYPE_ARM_VGIC_V3`).
    /// No payload boots without it; its absence is existential for every stage.
    Vgicv3Creatable,
    /// The guest's **ID registers are writable** (`KVM_SET_ONE_REG` on an `ID_AA64*`).
    /// AA-6(a) installs a synthetic ID-register model, so a kernel that rejects the write
    /// makes that verification impossible.
    WritableIdRegisters,
    /// The 0004-analogue determinism capability (`host/patches/`) is advertised —
    /// the positive probe that the patched kernel is actually running.
    DeterministicIntercepts,
}

impl Capability {
    /// The capability's name, as it appears in AA-0's truth table.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Capability::DevKvm => "dev-kvm",
            Capability::PerfBrRetired => "perf-raw-0x21-pinned",
            Capability::GuestDebug => "kvm-cap-set-guest-debug",
            Capability::Pmceid => "br-retired-pmceid1",
            Capability::HostOverflowDelivers => "host-overflow-delivers",
            Capability::Vgicv3Creatable => "vgicv3-creatable",
            Capability::WritableIdRegisters => "writable-id-registers",
            Capability::DeterministicIntercepts => "kvm-cap-arm-deterministic-intercepts",
        }
    }

    /// The mandatory AA-0 rows the host-side `arm-spike probe` command must confirm — the
    /// truth-table rows probed on the host (as opposed to the guest ID-register facts the
    /// `ident` payload reads). Every one must be `Present` or the probe exits nonzero; a
    /// row absent or unprobed is a host missing an existential mechanism, not a pass.
    /// [`Capability::DeterministicIntercepts`] is NOT here — it is the expect-*absent*
    /// patch marker, reported separately.
    #[must_use]
    pub const fn mandatory_aa0() -> &'static [Capability] {
        &[
            Capability::DevKvm,
            Capability::PerfBrRetired,
            Capability::GuestDebug,
            Capability::Pmceid,
            Capability::HostOverflowDelivers,
            Capability::Vgicv3Creatable,
            Capability::WritableIdRegisters,
        ]
    }

    /// Whether `docs/ARM-ALTRA.md` §AA-0 expects this capability **present**.
    ///
    /// [`Capability::DeterministicIntercepts`] is the one row expected *absent* on a
    /// stock kernel — it appears only once the patch draft is built and booted
    /// (AA-3). Absent is therefore a legitimate finding for it and a blocking one
    /// for the others. What is never legitimate for any of them is *unprobed*.
    #[must_use]
    pub const fn expect_present(self) -> bool {
        !matches!(self, Capability::DeterministicIntercepts)
    }
}

/// Why a syscall could not be issued, or an ioctl failed.
///
/// Portable so the callers need no `cfg`: off Linux every entry point returns
/// [`SysError::Unsupported`], which is a **"cannot probe"**, never a "no".
#[derive(Debug, thiserror::Error)]
pub enum SysError {
    /// A syscall set errno.
    #[error("{call} failed: errno {errno}")]
    Errno {
        /// The call that failed.
        call: &'static str,
        /// The errno value.
        errno: i32,
    },
    /// The kernel returned something the harness cannot work with.
    #[error("{0}")]
    Protocol(String),
    /// This host is not Linux: the perf/KVM layer does not exist here.
    #[error(
        "the KVM/perf syscall layer is Linux-only and unavailable on this host \
         (by design: the pure logic is tested here, the syscalls run on the Altra box)"
    )]
    Unsupported,
}

/// Probe a capability.
///
/// # Errors
///
/// [`SysError`] when the probe **could not be issued** — which is categorically
/// different from a clean `Ok(false)` ("the kernel does not have it"). A stage
/// disposition may never rest on a probe that could not run, so the two must never
/// collapse into one value; [`crate::sys::SysError::Unsupported`] off Linux is the
/// same "cannot probe", said honestly.
pub fn probe(cap: Capability) -> Result<bool, SysError> {
    imp::probe(cap)
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::{Capability, SysError};

    /// Off Linux nothing can be probed. Never `Ok(false)` — that would be a "no"
    /// this host is in no position to give.
    pub fn probe(_cap: Capability) -> Result<bool, SysError> {
        Err(SysError::Unsupported)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    //! The real Linux syscalls. **Untested on silicon.**
    //!
    //! Compiled on Linux so the cross-build gate proves they *build* for
    //! aarch64-linux; they have not been *run*, because the box is not yet here.

    use super::{
        BR_RETIRED_RAW, Capability, PerfEventAttr, SysError, br_retired_attr, kvm, perf_flags,
        sysfs_event_encodes,
    };

    /// The last errno, read immediately after a failed libc call.
    fn errno() -> i32 {
        // SAFETY: `__errno_location` returns a valid pointer to this thread's errno.
        unsafe { *libc::__errno_location() }
    }

    fn err(call: &'static str) -> SysError {
        SysError::Errno {
            call,
            errno: errno(),
        }
    }

    /// `perf_event_open` is not in libc; issue the raw syscall.
    ///
    /// # Safety
    /// `attr` must point at a valid, fully initialized `perf_event_attr`.
    pub(super) unsafe fn perf_event_open(
        attr: *const PerfEventAttr,
        pid: libc::pid_t,
        cpu: i32,
        group_fd: i32,
        flags: libc::c_ulong,
    ) -> libc::c_long {
        // SAFETY: SYS_perf_event_open with the architected argument order; `attr` is
        // a valid pointer per this function's contract.
        unsafe { libc::syscall(libc::SYS_perf_event_open, attr, pid, cpu, group_fd, flags) }
    }

    /// Whether raw `BR_RETIRED` can be **scheduled** as a pinned, non-multiplexed
    /// event (AA-0's PMU row, and the precondition for the entire work-clock bet).
    ///
    /// Opening the descriptor is not enough: a pinned event that cannot actually be
    /// placed on the PMU (too many competing counters) fails only once *scheduling* is
    /// attempted — `perf_event_open` succeeds, the fd is valid, and yet the counter
    /// never runs. So this enables the event, does a little branch work, and reads it
    /// back with `TOTAL_TIME_ENABLED`/`TOTAL_TIME_RUNNING`: the row is green only if the
    /// counter actually advanced and ran for the whole time it was enabled
    /// (`enabled == running` — not multiplexed). This is the "non-multiplexed counter"
    /// the AA-0 row is about, not just an openable descriptor.
    ///
    /// The probe workload is a **host-userspace** loop — there is no guest here — so
    /// the probe event must NOT set `exclude_host`: the measurement-loop attr
    /// ([`br_retired_attr`]) is guest-only (`exclude_host`), and a guest-only counter
    /// measuring a host loop would read exactly zero, making `count > 0` impossible and
    /// this mandatory row always fail. So the probe opens a host-userspace-counting
    /// variant (still pinned, raw `0x21`). Whether the *guest-only* attribution works
    /// on N1 is AA-1(b)'s measurement; this row is only "can raw 0x21 be scheduled,
    /// pinned and non-multiplexed" — which is answered by counting host work.
    fn probe_br_retired() -> Result<bool, SysError> {
        // PERF_FORMAT_TOTAL_TIME_ENABLED (1<<0) | PERF_FORMAT_TOTAL_TIME_RUNNING (1<<1):
        // read() then returns [count, time_enabled, time_running].
        const READ_FORMAT: u64 = 0b11;
        let mut attr = br_retired_attr(None);
        attr.read_format = READ_FORMAT;
        // Count host userspace (this thread's loop below): clear exclude_host, and
        // exclude the kernel so scheduler/IRQ branches do not inflate the count. The
        // point is only that the pinned raw event schedules and advances.
        attr.flags &= !perf_flags::EXCLUDE_HOST;
        attr.flags |= perf_flags::EXCLUDE_KERNEL;

        // SAFETY: `attr` is a fully initialized perf_event_attr on this frame; the
        // pointer is valid for the call. Counting this thread (pid 0) on whatever CPU
        // it runs on (-1), no group.
        let fd = unsafe { perf_event_open(&raw const attr, 0, -1, -1, 0) };
        if fd < 0 {
            let e = errno();
            // ENOENT/EOPNOTSUPP mean the event is not implemented here — a real "no".
            // Any other errno is a failure to probe, and must not be flattened into one.
            if e == libc::ENOENT || e == libc::EOPNOTSUPP {
                return Ok(false);
            }
            return Err(SysError::Errno {
                call: "perf_event_open(BR_RETIRED)",
                errno: e,
            });
        }
        let fd = fd as i32;
        let _ = BR_RETIRED_RAW;

        // Enable, run a little branch-y work, read back.
        const PERF_IOC_ENABLE: libc::c_ulong = 0x2400;
        const PERF_IOC_RESET: libc::c_ulong = 0x2403;
        // SAFETY: `fd` is a valid perf descriptor; these ioctls take an integer arg.
        let scheduled = unsafe {
            libc::ioctl(fd, PERF_IOC_RESET, 0_u64);
            if libc::ioctl(fd, PERF_IOC_ENABLE, 0_u64) < 0 {
                libc::close(fd);
                return Err(err("PERF_EVENT_IOC_ENABLE(BR_RETIRED probe)"));
            }
            // A small, volatile loop so the branches actually retire and cannot be
            // optimized away.
            let mut acc: u64 = 0;
            for i in 0..10_000u64 {
                acc = core::hint::black_box(acc.wrapping_add(i));
            }
            core::hint::black_box(acc);

            // read() -> [count, time_enabled, time_running].
            let mut buf = [0u64; 3];
            let n = libc::read(fd, buf.as_mut_ptr().cast::<libc::c_void>(), 24);
            libc::close(fd);
            if n != 24 {
                return Err(SysError::Errno {
                    call: "read(BR_RETIRED probe)",
                    errno: errno(),
                });
            }
            let (count, enabled, running) = (buf[0], buf[1], buf[2]);
            // Scheduled and non-multiplexed: the counter ran for the whole time it was
            // enabled, and it actually advanced. A running < enabled means the pinned
            // event was multiplexed off — not the guaranteed-on work clock the row
            // requires; running == 0 means it never scheduled at all.
            running > 0 && running == enabled && count > 0
        };
        Ok(scheduled)
    }

    /// The truth-table `br-retired-pmceid1` row: is raw `BR_RETIRED` (0x21) an
    /// **implemented** PMU event, PMCEID-backed?
    ///
    /// A clean `perf_event_open` of the raw event is NOT proof: the ARM PMUv3 driver accepts
    /// arbitrary raw event encodings without checking whether the event's PMCEID bit is set,
    /// so an unimplemented 0x21 opens successfully and then reads zero — reporting the
    /// architectural row present when it is false. The PMU's `events/<name>` sysfs files are
    /// exposed ONLY for events whose PMCEID bit is implemented
    /// (`armv8pmu_event_attr_is_visible` gates them on the PMCEID bitmap), so the presence of
    /// an `events/br_retired` file encoding `event=0x21` on a PMUv3 device is a direct,
    /// implementation-specific PMCEID proof. Search every PMU device for it.
    fn probe_br_retired_implemented() -> Result<bool, SysError> {
        let devices = match std::fs::read_dir("/sys/bus/event_source/devices") {
            Ok(d) => d,
            // No perf device tree at all: a clean "no", not a failure to probe.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => {
                return Err(SysError::Errno {
                    call: "read_dir(/sys/bus/event_source/devices)",
                    errno: errno(),
                });
            }
        };
        for entry in devices {
            // A dir-iteration error (a read failure on the directory) is an inability to
            // probe, not an absence — surface it (`unprobed`), never flatten it away.
            let entry = entry.map_err(|e| SysError::Errno {
                call: "read_dir entry (/sys/bus/event_source/devices)",
                errno: e.raw_os_error().unwrap_or(0),
            })?;
            let name = entry.file_name();
            // PMUv3 CPU-PMU devices are named `armv8_*` (`armv8_pmuv3_0`, `armv8_cortex_*`, …).
            if !name.to_string_lossy().starts_with("armv8") {
                continue;
            }
            // The driver exposes this file only if BR_RETIRED's PMCEID bit is set here.
            let path = entry.path().join("events").join("br_retired");
            match std::fs::read_to_string(&path) {
                Ok(contents) if sysfs_event_encodes(&contents, BR_RETIRED_RAW) => {
                    return Ok(true);
                }
                Ok(_) => {}
                // The file is absent here (this PMU's PMCEID bit is clear): keep searching.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                // The file exists but could not be READ (EACCES/EIO/…): an inability to
                // probe, not absence — the AA-0 row must read `unprobed`, never `absent`.
                Err(e) => {
                    return Err(SysError::Errno {
                        call: "read(events/br_retired)",
                        errno: e.raw_os_error().unwrap_or(0),
                    });
                }
            }
        }
        // Searched every PMU device and none exposes a PMCEID-backed BR_RETIRED: absent.
        Ok(false)
    }

    /// The truth-table `host-overflow-delivers` row: arm a small `BR_RETIRED` sample
    /// period, run a branchy loop well past it, and confirm the kernel **delivered** an
    /// overflow sample into the ring buffer. A counter that increments but never
    /// overflows a sample cannot arm a deadline — AA-1's existential row, and not
    /// something [`probe_br_retired`]'s counting check establishes.
    fn probe_host_overflow_delivers() -> Result<bool, SysError> {
        const PERIOD: u64 = 1_000;
        let mut attr = br_retired_attr(Some(PERIOD));
        attr.flags &= !perf_flags::EXCLUDE_HOST;
        attr.flags |= perf_flags::EXCLUDE_KERNEL;
        // SAFETY: `attr` is fully initialized; count this thread, any cpu, no group.
        let fd = unsafe { perf_event_open(&raw const attr, 0, -1, -1, 0) };
        if fd < 0 {
            let e = errno();
            if e == libc::ENOENT || e == libc::EOPNOTSUPP {
                return Ok(false);
            }
            return Err(SysError::Errno {
                call: "perf_event_open(overflow probe)",
                errno: e,
            });
        }
        let fd = fd as i32;

        // A perf ring buffer: one metadata page + a power-of-two count of data pages.
        // The perf ABI counts these in HOST pages, and arm64 hosts run 4 KiB, 16 KiB, or
        // 64 KiB pages. A hard-coded 4 KiB would round a 64 KiB-page host's 36 KiB mapping
        // down to a single (metadata-only) page with ZERO data pages, so `mmap` rejects it
        // and this mandatory AA-0 row could never be probed. Query the running page size.
        // SAFETY: `sysconf` takes an integer name and returns a `c_long`; no pointers.
        let page = match unsafe { libc::sysconf(libc::_SC_PAGESIZE) } {
            n if n >= 4096 => n as usize,
            // sysconf failed (-1) or returned an implausibly small value: fall back to the
            // universal minimum rather than build a malformed (zero-data-page) geometry.
            _ => 4096,
        };
        let len = page * (1 + 8);
        // SAFETY: mmap a MAP_SHARED perf ring over the event fd, as the perf ABI requires.
        let map = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if map == libc::MAP_FAILED {
            let e = err("mmap(perf ring)");
            // SAFETY: `fd` is valid and owned here.
            unsafe { libc::close(fd) };
            return Err(e);
        }

        const PERF_IOC_ENABLE: libc::c_ulong = 0x2400;
        const PERF_IOC_RESET: libc::c_ulong = 0x2403;
        // `data_head` sits at offset 1024 in `struct perf_event_mmap_page` (stable ABI);
        // it advances when the kernel writes an overflow sample.
        const DATA_HEAD_OFFSET: usize = 1024;
        // SAFETY: `fd` is valid and `map` is a live mapping of `len` bytes; the ioctls take
        // integer args, the loop retires branches past the period, and `data_head` is read
        // volatilely from its ABI offset within the mapped header page.
        let delivered = unsafe {
            libc::ioctl(fd, PERF_IOC_RESET, 0_u64);
            if libc::ioctl(fd, PERF_IOC_ENABLE, 0_u64) < 0 {
                let e = err("PERF_EVENT_IOC_ENABLE(overflow probe)");
                libc::munmap(map, len);
                libc::close(fd);
                return Err(e);
            }
            let mut acc: u64 = 0;
            for i in 0..1_000_000u64 {
                acc = core::hint::black_box(acc.wrapping_add(i));
            }
            core::hint::black_box(acc);
            let data_head =
                core::ptr::read_volatile((map.cast::<u8>()).add(DATA_HEAD_OFFSET).cast::<u64>());
            libc::munmap(map, len);
            libc::close(fd);
            // A nonzero data_head means the kernel wrote at least one overflow sample.
            data_head > 0
        };
        Ok(delivered)
    }

    /// Open `/dev/kvm`.
    fn open_kvm() -> Result<i32, SysError> {
        let path = c"/dev/kvm";
        // SAFETY: opening a device with a valid NUL-terminated path.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(err("open(/dev/kvm)"));
        }
        Ok(fd)
    }

    fn probe_dev_kvm() -> Result<bool, SysError> {
        match open_kvm() {
            Ok(fd) => {
                // SAFETY: `fd` is valid.
                unsafe { libc::close(fd) };
                Ok(true)
            }
            Err(SysError::Errno { errno, .. })
                if errno == libc::ENOENT || errno == libc::EACCES =>
            {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// `KVM_CHECK_EXTENSION` on a **VM fd**.
    ///
    /// The VM-level check is the one that tells the truth for arm64 caps, and it is
    /// precisely what a probe with no VM could not do — which is why these two rows
    /// used to be stubbed. Creating the VM is cheap and is the same call the
    /// measurement loop makes.
    fn probe_vm_capability(cap: u64) -> Result<bool, SysError> {
        let kvm_fd = open_kvm()?;
        // SAFETY: `kvm_fd` is a valid /dev/kvm descriptor; KVM_CREATE_VM takes a
        // machine type (0 = default) and returns a VM fd.
        let vm_fd = unsafe { libc::ioctl(kvm_fd, kvm::CREATE_VM as libc::c_ulong, 0_u64) };
        if vm_fd < 0 {
            let e = err("ioctl(KVM_CREATE_VM)");
            // SAFETY: `kvm_fd` is valid.
            unsafe { libc::close(kvm_fd) };
            return Err(e);
        }
        // SAFETY: `vm_fd` is a valid VM descriptor; KVM_CHECK_EXTENSION returns
        // 0 (absent) or a positive value (present) and never writes through a
        // pointer.
        let rc = unsafe { libc::ioctl(vm_fd, kvm::CHECK_EXTENSION as libc::c_ulong, cap) };
        let out = if rc < 0 {
            Err(err("ioctl(KVM_CHECK_EXTENSION)"))
        } else {
            Ok(rc > 0)
        };
        // SAFETY: both descriptors are valid and owned here.
        unsafe {
            libc::close(vm_fd);
            libc::close(kvm_fd);
        }
        out
    }

    /// Probe a capability.
    ///
    /// # Errors
    /// [`SysError`] if the probe could not be *issued* (as opposed to a clean "no",
    /// which is `Ok(false)`).
    pub fn probe(cap: Capability) -> Result<bool, SysError> {
        match cap {
            Capability::DevKvm => probe_dev_kvm(),
            Capability::PerfBrRetired => probe_br_retired(),
            Capability::GuestDebug => probe_vm_capability(kvm::CAP_SET_GUEST_DEBUG),
            Capability::Pmceid => probe_br_retired_implemented(),
            Capability::HostOverflowDelivers => probe_host_overflow_delivers(),
            Capability::Vgicv3Creatable => super::machine::probe_vgicv3_creatable(),
            Capability::WritableIdRegisters => super::machine::probe_writable_id_registers(),
            Capability::DeterministicIntercepts => {
                probe_vm_capability(kvm::CAP_ARM_DETERMINISTIC_INTERCEPTS)
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub mod machine;

#[cfg(target_os = "linux")]
pub use machine::{
    HostIdRegisters, Machine, Mechanism, MigrationChurner, ParamsPage, PerfCounter, allowed_cores,
    current_tid, pin_to_core, read_host_id_registers,
};

/// AA-1(a)'s host-side EL0 counter: raw `BR_RETIRED` over THIS thread's userspace
/// execution ([`el0_count_attr`]), read with the enabled/running times so the
/// checker can prove the pinned event was never multiplexed off.
#[cfg(target_os = "linux")]
mod el0counter {
    use super::{PerfEventAttr, SysError, el0_count_attr, imp};

    fn errno() -> i32 {
        // SAFETY: `__errno_location` returns a valid pointer to this thread's errno.
        unsafe { *libc::__errno_location() }
    }

    /// A host-thread `BR_RETIRED` counter (AA-1(a)). Opened disabled; the caller
    /// brackets each window call with [`HostCounter::reset_enable`] /
    /// [`HostCounter::disable_read`].
    pub struct HostCounter {
        fd: i32,
        attr: PerfEventAttr,
    }

    impl HostCounter {
        /// Open on the current thread (pid 0), whatever CPU it is pinned to
        /// (cpu −1), disabled.
        ///
        /// # Errors
        /// [`SysError`] if `perf_event_open` refuses.
        pub fn open() -> Result<Self, SysError> {
            let attr = el0_count_attr();
            // SAFETY: `attr` is fully initialized on this frame and valid for the call.
            let fd = unsafe { imp::perf_event_open(&raw const attr, 0, -1, -1, 0) };
            if fd < 0 {
                return Err(SysError::Errno {
                    call: "perf_event_open(EL0 BR_RETIRED)",
                    errno: errno(),
                });
            }
            Ok(Self {
                fd: fd as i32,
                attr,
            })
        }

        /// The attr the fd was actually opened with — the manifest's `perf` block
        /// derives from this, never from a hand-written description.
        #[must_use]
        pub fn attr(&self) -> &PerfEventAttr {
            &self.attr
        }

        /// Zero the counter and enable it.
        ///
        /// # Errors
        /// [`SysError`] if either ioctl refuses.
        pub fn reset_enable(&mut self) -> Result<(), SysError> {
            const PERF_IOC_ENABLE: libc::c_ulong = 0x2400;
            const PERF_IOC_RESET: libc::c_ulong = 0x2403;
            // SAFETY: `self.fd` is a valid perf descriptor; both ioctls take an int arg.
            unsafe {
                if libc::ioctl(self.fd, PERF_IOC_RESET, 0_u64) < 0 {
                    return Err(SysError::Errno {
                        call: "PERF_EVENT_IOC_RESET(EL0)",
                        errno: errno(),
                    });
                }
                if libc::ioctl(self.fd, PERF_IOC_ENABLE, 0_u64) < 0 {
                    return Err(SysError::Errno {
                        call: "PERF_EVENT_IOC_ENABLE(EL0)",
                        errno: errno(),
                    });
                }
            }
            Ok(())
        }

        /// Disable, then read `(count, time_enabled, time_running)`.
        ///
        /// # Errors
        /// [`SysError`] if the ioctl or the 24-byte read refuses.
        pub fn disable_read(&mut self) -> Result<(u64, u64, u64), SysError> {
            const PERF_IOC_DISABLE: libc::c_ulong = 0x2401;
            // SAFETY: `self.fd` is valid; the read buffer is 24 bytes for
            // [count, enabled, running] per the attr's read_format.
            unsafe {
                if libc::ioctl(self.fd, PERF_IOC_DISABLE, 0_u64) < 0 {
                    return Err(SysError::Errno {
                        call: "PERF_EVENT_IOC_DISABLE(EL0)",
                        errno: errno(),
                    });
                }
                let mut buf = [0u64; 3];
                let n = libc::read(self.fd, buf.as_mut_ptr().cast::<libc::c_void>(), 24);
                if n != 24 {
                    return Err(SysError::Errno {
                        call: "read(EL0 BR_RETIRED)",
                        errno: errno(),
                    });
                }
                Ok((buf[0], buf[1], buf[2]))
            }
        }
    }

    impl Drop for HostCounter {
        fn drop(&mut self) {
            // SAFETY: `self.fd` is owned and valid; close-on-drop only.
            unsafe {
                libc::close(self.fd);
            }
        }
    }
}
#[cfg(target_os = "linux")]
pub use el0counter::HostCounter;

#[cfg(test)]
mod tests {
    //! The ABI, pinned.
    //!
    //! These run natively on the Mac — they are the reason the ABI half of this
    //! module is portable. A flag constant on the wrong bit does not fail loudly on
    //! the box: it opens a *different event* (an unpinned, host-inclusive counter)
    //! and reports the AA-0 row green. That is the "instrument that goes green
    //! without measuring the thing" failure the whole apparatus exists to prevent,
    //! so the bit positions are asserted, not trusted.

    use super::*;

    #[test]
    fn kvm_mode_log_parse_recognizes_exactly_the_three_kernel_lines() {
        // The real line from the first box this apparatus met (harmony-arm, Ubuntu
        // 6.8.0-134, where no /sys/module/kvm_arm/parameters/mode exists).
        let vhe = "[    9.960266] kvm [1]: VHE mode initialized successfully";
        assert_eq!(parse_kvm_mode_from_log(vhe).as_deref(), Some("vhe"));
        let nvhe = "[    4.1] kvm [1]: Hyp mode initialized successfully";
        assert_eq!(parse_kvm_mode_from_log(nvhe).as_deref(), Some("nvhe"));
        let prot = "[    4.1] kvm [1]: Protected nVHE mode initialized successfully";
        assert_eq!(parse_kvm_mode_from_log(prot).as_deref(), Some("protected"));

        // "Protected nVHE" contains neither of the other two phrases' prefixes by
        // accident: the discriminating check must not misread it as vhe/nvhe.
        assert_ne!(parse_kvm_mode_from_log(prot).as_deref(), Some("vhe"));

        // Unrecognized lines are NOT evidence: no kvm prefix, a different phrase, or
        // an empty log all say None rather than inventing a mode.
        assert_eq!(
            parse_kvm_mode_from_log("VHE mode initialized successfully quoted elsewhere"),
            None
        );
        assert_eq!(
            parse_kvm_mode_from_log("[1.0] kvm [1]: some other line"),
            None
        );
        assert_eq!(parse_kvm_mode_from_log(""), None);

        // The last match wins across a multi-line buffer.
        let log = format!("{nvhe}\nnoise\n{vhe}\n");
        assert_eq!(parse_kvm_mode_from_log(&log).as_deref(), Some("vhe"));
    }

    #[test]
    fn perf_flag_bits_match_the_kernel_abi() {
        // include/uapi/linux/perf_event.h, in declaration order.
        assert_eq!(perf_flags::DISABLED, 1 << 0);
        assert_eq!(perf_flags::INHERIT, 1 << 1);
        assert_eq!(perf_flags::PINNED, 1 << 2);
        assert_eq!(perf_flags::EXCLUSIVE, 1 << 3);
        assert_eq!(perf_flags::EXCLUDE_USER, 1 << 4);
        assert_eq!(perf_flags::EXCLUDE_KERNEL, 1 << 5);
        assert_eq!(perf_flags::EXCLUDE_HV, 1 << 6);
        assert_eq!(perf_flags::EXCLUDE_IDLE, 1 << 7);
        assert_eq!(perf_flags::EXCLUDE_HOST, 1 << 19);
        assert_eq!(perf_flags::EXCLUDE_GUEST, 1 << 20);

        // The two confusions that would arm the wrong event, named as such: `pinned`
        // is NOT `exclusive`, and `exclude_host` is NOT `comm` (bit 9).
        assert_ne!(perf_flags::PINNED, perf_flags::EXCLUSIVE);
        assert_ne!(perf_flags::EXCLUDE_HOST, 1 << 9);
    }

    #[test]
    fn the_work_clock_event_is_pinned_guest_only_br_retired() {
        let attr = br_retired_attr(None);
        assert_eq!(attr.type_, PERF_TYPE_RAW);
        assert_eq!(attr.config, 0x21, "BR_RETIRED, the event the docs name");
        assert_eq!(attr.size, PERF_ATTR_SIZE_VER6);
        assert!(attr.flags & perf_flags::PINNED != 0, "never multiplexed");
        assert!(attr.flags & perf_flags::EXCLUDE_HOST != 0, "guest-only");
        assert!(
            attr.flags & perf_flags::EXCLUDE_GUEST == 0,
            "excluding the guest would count exactly the wrong thing"
        );
        assert!(attr.flags & perf_flags::EXCLUSIVE == 0);
        assert_eq!(
            attr.sample_period_or_freq, 0,
            "counting mode arms no deadline"
        );
    }

    #[test]
    fn an_armed_attr_carries_its_sample_period() {
        let attr = br_retired_attr(Some(4096));
        assert_eq!(attr.sample_period_or_freq, 4096);
        assert_eq!(perf_config(&attr).sample_period, Some(4096));
    }

    #[test]
    fn the_evidence_perf_block_is_derived_from_the_armed_attr() {
        // The anti-fabrication property: a manifest cannot claim a configuration the
        // fd was not opened with, because the claim is a projection of those bits.
        let cfg = perf_config(&br_retired_attr(None));
        assert_eq!(cfg.raw_event, BR_RETIRED_RAW);
        assert!(cfg.pinned);
        assert!(cfg.exclude_host);
        assert!(!cfg.exclude_guest);
        assert_eq!(cfg.sample_period, None);

        // And a wrongly-armed event is *visible* as such, rather than being
        // described by a hand-written manifest that says what the operator meant.
        let mut bad = br_retired_attr(None);
        bad.flags = perf_flags::EXCLUDE_GUEST;
        let cfg = perf_config(&bad);
        assert!(!cfg.pinned);
        assert!(!cfg.exclude_host);
        assert!(cfg.exclude_guest);
    }

    #[test]
    fn perf_event_attr_is_a_byte_exact_abi_prefix() {
        // PERF_ATTR_SIZE_VER6 == 120. The kernel validates `attr.size` against the
        // known ABI sizes and zero-extends the tail; a struct that is not a prefix
        // is an EINVAL at best and a misread field at worst.
        assert_eq!(
            core::mem::size_of::<PerfEventAttr>(),
            PERF_ATTR_SIZE_VER6 as usize
        );
        assert_eq!(core::mem::align_of::<PerfEventAttr>(), 8);
        assert_eq!(core::mem::offset_of!(PerfEventAttr, config), 8);
        assert_eq!(
            core::mem::offset_of!(PerfEventAttr, sample_period_or_freq),
            16
        );
        assert_eq!(core::mem::offset_of!(PerfEventAttr, flags), 40);
    }

    #[test]
    fn kvm_run_field_offsets_match_the_abi() {
        assert_eq!(core::mem::offset_of!(KvmRun, exit_reason), 8);
        assert_eq!(core::mem::offset_of!(KvmRun, mmio), 32);
        assert_eq!(core::mem::offset_of!(KvmRunMmio, phys_addr), 0);
        assert_eq!(core::mem::offset_of!(KvmRunMmio, data), 8);
        assert_eq!(core::mem::offset_of!(KvmRunMmio, len), 16);
        assert_eq!(core::mem::offset_of!(KvmRunMmio, is_write), 20);
    }

    /// A zeroed `KvmRun` to build exit snapshots from — the in-process loopback the
    /// machine layer's pointer logic is driven against under Miri.
    fn blank_kvm_run() -> KvmRun {
        KvmRun {
            request_interrupt_window: 0,
            immediate_exit: 0,
            padding1: [0; 6],
            exit_reason: 0,
            ready_for_interrupt_injection: 0,
            if_flag: 0,
            flags: 0,
            cr8: 0,
            apic_base: 0,
            mmio: KvmRunMmio {
                phys_addr: 0,
                data: [0; 8],
                len: 0,
                is_write: 0,
                padding: [0; 3],
            },
        }
    }

    #[test]
    fn decode_kvm_run_maps_each_exit_reason() {
        use crate::run::VcpuExit;

        let mut run = blank_kvm_run();
        run.exit_reason = kvm::EXIT_MMIO;
        run.mmio.phys_addr = 0x0900_0000;
        run.mmio.len = 1;
        run.mmio.is_write = 1;
        run.mmio.data = [0x42, 0, 0, 0, 0, 0, 0, 0];
        match decode_kvm_run(&run) {
            VcpuExit::Mmio {
                addr,
                data,
                is_write,
            } => {
                assert_eq!(addr, 0x0900_0000);
                assert_eq!(data, vec![0x42]); // bounded by len=1
                assert!(is_write);
            }
            other => panic!("expected Mmio, got {other:?}"),
        }

        // A zero/too-wide host length is explicit malformed input, never clamped
        // into a plausible access that a higher layer could service.
        for width in [0, 9, 999] {
            run.mmio.len = width;
            assert_eq!(
                decode_kvm_run(&run),
                VcpuExit::MalformedMmio {
                    addr: 0x0900_0000,
                    width,
                }
            );
        }

        for (reason, want) in [
            (kvm::EXIT_PREEMPT, VcpuExit::Preempt),
            (kvm::EXIT_INTR, VcpuExit::SignalKick),
            (kvm::EXIT_DEBUG, VcpuExit::Debug),
            (7777, VcpuExit::Other(7777)),
        ] {
            run.exit_reason = reason;
            assert_eq!(decode_kvm_run(&run), want);
        }
    }

    #[test]
    fn stage_mmio_read_writes_the_bounded_value_into_the_shared_buffer() {
        let mut run = blank_kvm_run();
        stage_mmio_read(&mut run, &0u32.to_le_bytes());
        assert_eq!(run.mmio.data, [0, 0, 0, 0, 0, 0, 0, 0]);

        stage_mmio_read(&mut run, &[0xAA, 0xBB]);
        assert_eq!(&run.mmio.data[..2], &[0xAA, 0xBB]);

        // A value wider than the 8-byte buffer is clamped — no write past the field.
        let wide = [1u8; 16];
        stage_mmio_read(&mut run, &wide);
        assert_eq!(run.mmio.data, [1; 8]);
    }

    #[test]
    fn write_mmio_read_stages_over_an_allocation() {
        // The Machine seam's volatile read-modify-write, exercised against a HEAP-backed
        // kvm_run so the raw-pointer path (read_volatile of the mapped struct, the mmio.data
        // field projection/cast, byte-wise write_volatile) runs under Miri — a real KVM
        // MAP_SHARED mapping is unavailable to the interpreter.
        let mut run = Box::new(blank_kvm_run());
        run.mmio.data = [0xFF; 8]; // preexisting bytes the stage must overwrite
        let value = [0xDE, 0xAD, 0xBE, 0xEF];
        // SAFETY: `run` is a live, uniquely-owned boxed KvmRun.
        unsafe { write_mmio_read(&raw mut *run, &value) };
        assert_eq!(&run.mmio.data[..4], &value);
        // A width over 8 is clamped by the bounded seam — no write past the field.
        // SAFETY: as above.
        unsafe { write_mmio_read(&raw mut *run, &[7u8; 16]) };
        assert_eq!(run.mmio.data, [7; 8]);
    }

    #[test]
    fn guest_ram_borrows_the_allocation() {
        // `state_digest`'s from_raw_parts, exercised against a heap buffer under Miri.
        let buf = vec![1u8, 2, 3, 4, 5];
        // SAFETY: `buf` owns `len` initialised bytes, unwritten for the borrow's lifetime.
        let slice = unsafe { guest_ram(buf.as_ptr(), buf.len()) };
        assert_eq!(slice, &[1, 2, 3, 4, 5]);
        // Feeds the portable digest seam exactly as the Machine does.
        let empty: &[u8] = &[];
        assert_eq!(
            digest_state(&std::collections::BTreeMap::new(), slice, empty),
            digest_state(&std::collections::BTreeMap::new(), &buf, empty)
        );
    }

    #[test]
    fn elf_gnu_build_id_reads_the_note_and_rejects_non_elf() {
        // A minimal little-endian ELF64 whose single PT_NOTE segment carries an NT_GNU_BUILD_ID
        // note with a two-byte build-id (0xAB, 0xCD).
        let mut note = Vec::new();
        note.extend_from_slice(&4u32.to_le_bytes()); // namesz = "GNU\0"
        note.extend_from_slice(&2u32.to_le_bytes()); // descsz = 2 build-id bytes
        note.extend_from_slice(&3u32.to_le_bytes()); // NT_GNU_BUILD_ID
        note.extend_from_slice(b"GNU\0"); // name (already 4-aligned)
        note.extend_from_slice(&[0xAB, 0xCD]); // desc
        note.extend_from_slice(&[0, 0]); // 4-align the desc

        let ehsize = 64usize;
        let phoff = ehsize;
        let phentsize = 56usize;
        let note_off = phoff + phentsize;

        let mut elf = vec![0u8; note_off];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2; // ELFCLASS64
        elf[5] = 1; // ELFDATA2LSB
        elf[32..40].copy_from_slice(&(phoff as u64).to_le_bytes()); // e_phoff
        elf[54..56].copy_from_slice(&(phentsize as u16).to_le_bytes()); // e_phentsize
        elf[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        // Program header: p_type = PT_NOTE(4) @0, p_offset @8, p_filesz @32.
        elf[phoff..phoff + 4].copy_from_slice(&4u32.to_le_bytes());
        elf[phoff + 8..phoff + 16].copy_from_slice(&(note_off as u64).to_le_bytes());
        elf[phoff + 32..phoff + 40].copy_from_slice(&(note.len() as u64).to_le_bytes());
        elf.extend_from_slice(&note);

        assert_eq!(elf_gnu_build_id(&elf).as_deref(), Some("abcd"));
        // A non-ELF (a stripped /boot/Image is a flat binary) has no note → None (cannot bind).
        assert_eq!(elf_gnu_build_id(b"MZ not an elf at all"), None);
        assert_eq!(elf_gnu_build_id(&[]), None);

        // Hostile, operator-supplied offsets must return None, NEVER panic in an
        // overflow-checked build: `e_phoff` near usize::MAX would overflow `o + width` when
        // forming a slice bound. checked_add makes it a clean None.
        let mut hostile = elf.clone();
        hostile[32..40].copy_from_slice(&u64::MAX.to_le_bytes()); // e_phoff = usize::MAX
        assert_eq!(elf_gnu_build_id(&hostile), None);
        let mut hostile = elf.clone();
        hostile[32..40].copy_from_slice(&(u64::MAX - 1).to_le_bytes());
        assert_eq!(elf_gnu_build_id(&hostile), None);
    }

    #[test]
    fn digest_state_is_order_stable_and_input_sensitive() {
        use std::collections::BTreeMap;
        let ram = vec![0u8; 64];

        let vgic: &[u8] = &[0x00; 16];

        let mut a = BTreeMap::new();
        a.insert(2u64, vec![0xAA]);
        a.insert(1u64, vec![0xBB]);
        // Insertion order differs; the BTreeMap makes the digest identical anyway.
        let mut b = BTreeMap::new();
        b.insert(1u64, vec![0xBB]);
        b.insert(2u64, vec![0xAA]);
        assert_eq!(digest_state(&a, &ram, vgic), digest_state(&b, &ram, vgic));

        // Different RAM → different digest (the RAM really is hashed).
        let mut other_ram = ram.clone();
        other_ram[0] = 1;
        assert_ne!(
            digest_state(&a, &ram, vgic),
            digest_state(&a, &other_ram, vgic)
        );

        // Different register value → different digest.
        let mut c = a.clone();
        c.insert(1u64, vec![0xCC]);
        assert_ne!(digest_state(&a, &ram, vgic), digest_state(&c, &ram, vgic));

        // Different vGIC state (an interrupt now pending/active) → different digest.
        // This is the AA-6 injection axis: same registers and RAM, different vGIC.
        let mut other_vgic = vgic.to_vec();
        other_vgic[8] = 1;
        assert_ne!(
            digest_state(&a, &ram, vgic),
            digest_state(&a, &ram, &other_vgic),
            "the vGIC distributor state must reach the digest"
        );

        assert!(digest_state(&a, &ram, vgic).starts_with("sha256:"));
    }

    #[test]
    fn regs_only_digest_ignores_ram_and_never_collides_with_the_full_digest() {
        use std::collections::BTreeMap;
        let vgic: &[u8] = &[0x00; 16];

        let mut a = BTreeMap::new();
        a.insert(2u64, vec![0xAA]);
        a.insert(1u64, vec![0xBB]);
        // Insertion order differs; the BTreeMap makes the registers-only digest identical.
        let mut b = BTreeMap::new();
        b.insert(1u64, vec![0xBB]);
        b.insert(2u64, vec![0xAA]);
        assert_eq!(digest_regs_only(&a, vgic), digest_regs_only(&b, vgic));

        // The whole point: the registers-only digest does NOT depend on guest RAM — two
        // states differing only in RAM digest identically here (that is the cheap per-step
        // key), while `digest_state` (which hashes RAM) separates them.
        let ram = vec![0u8; 64];
        let mut other_ram = ram.clone();
        other_ram[0] = 1;
        assert_ne!(
            digest_state(&a, &ram, vgic),
            digest_state(&a, &other_ram, vgic),
            "the full digest hashes RAM"
        );

        // A different register value or vGIC state still moves the registers-only digest —
        // it is real evidence, not a constant.
        let mut c = a.clone();
        c.insert(1u64, vec![0xCC]);
        assert_ne!(digest_regs_only(&a, vgic), digest_regs_only(&c, vgic));
        let mut other_vgic = vgic.to_vec();
        other_vgic[8] = 1;
        assert_ne!(
            digest_regs_only(&a, vgic),
            digest_regs_only(&a, &other_vgic)
        );

        // Distinct domain separation: the registers-only digest can NEVER equal the full
        // digest of the same registers over empty RAM — the two are compared side by side
        // (intermediate step vs the full-payload final step), so they must be structurally
        // unequal, not merely unequal because RAM happened to differ.
        let empty_ram: &[u8] = &[];
        assert_ne!(
            digest_regs_only(&a, vgic),
            digest_state(&a, empty_ram, vgic),
            "the domain tags must keep the two digest kinds from ever colliding"
        );

        assert!(digest_regs_only(&a, vgic).starts_with("sha256:"));
    }

    #[test]
    fn host_time_registers_are_the_generic_timer_counters() {
        // ARM64_SYS_REG(op0,op1,crn,crm,op2) id builder.
        // KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM64_SYSREG (0x0013 << 16 — the
        // coprocessor selector lives in the LOW bits, not bit 48). A real timer-counter
        // id has this exact prefix; the earlier bit-48 form matched nothing.
        const P: u64 = 0x6000_0000_0000_0000 | 0x0030_0000_0000_0000 | (0x0013 << 16);
        let sysreg = |op0: u64, op1: u64, crn: u64, crm: u64, op2: u64| {
            P | (op0 << 14) | (op1 << 11) | (crn << 7) | (crm << 3) | op2
        };

        // The live host-time counters — excluded.
        assert!(is_host_time_register(sysreg(3, 3, 14, 0, 1)), "CNTPCT_EL0");
        assert!(
            is_host_time_register(sysreg(3, 3, 14, 0, 5)),
            "CNTPCTSS_EL0"
        );
        assert!(
            is_host_time_register(sysreg(3, 3, 14, 0, 6)),
            "CNTVCTSS_EL0"
        );
        assert!(
            is_host_time_register(sysreg(3, 3, 14, 3, 2)),
            "KVM_REG_ARM_TIMER_CNT"
        );

        // Deterministic timer state — KEPT. In the KVM one-reg ABI id (3,3,14,0,2) is
        // KVM_REG_ARM_TIMER_CVAL — the programmed virtual-timer deadline, NOT the live
        // CNTVCT_EL0 the architectural encoding names — so it must survive into the digest.
        assert!(
            !is_host_time_register(sysreg(3, 3, 14, 0, 2)),
            "KVM_REG_ARM_TIMER_CVAL"
        );
        assert!(!is_host_time_register(sysreg(3, 3, 14, 0, 0)), "CNTFRQ_EL0");
        assert!(
            !is_host_time_register(sysreg(3, 3, 14, 3, 1)),
            "KVM_REG_ARM_TIMER_CTL"
        );
        assert!(
            !is_host_time_register(sysreg(3, 0, 14, 1, 0)),
            "CNTKCTL_EL1"
        );

        // A core register (the pc) is not a sysreg and is kept.
        assert!(!is_host_time_register(
            kvm::REG_ARM64_CORE_U64 | kvm::REG_CORE_PC
        ));

        // The load-bearing anti-regression: *literal* real KVM register ids, not ones built
        // from the same prefix the predicate uses, so the SYSREG selector position is pinned
        // independently. KVM_REG_ARM_TIMER_CNT = ARM64_SYS_REG(3,3,14,3,2) is the live counter
        // and MUST be excluded; its CVAL alias ARM64_SYS_REG(3,3,14,0,2) is the deadline and
        // must NOT be. The coprocessor selector `0x0013` sits at bits 16–27 (`…_0013_DF1A` /
        // `…_0013_DF02`), never bit 48.
        assert_eq!(
            sysreg(3, 3, 14, 3, 2),
            0x6030_0000_0013_DF1A,
            "TIMER_CNT id"
        );
        assert!(
            is_host_time_register(0x6030_0000_0013_DF1A),
            "the live KVM_REG_ARM_TIMER_CNT id must be excluded"
        );
        assert_eq!(
            sysreg(3, 3, 14, 0, 2),
            0x6030_0000_0013_DF02,
            "TIMER_CVAL id"
        );
        assert!(
            !is_host_time_register(0x6030_0000_0013_DF02),
            "the KVM_REG_ARM_TIMER_CVAL deadline must be retained in the digest"
        );
    }

    #[test]
    fn a_digest_ignores_the_live_clock_so_replay_survives_scheduling_jitter() {
        // The flagship fix: two same-seed runs whose ONLY difference is the live counter
        // must digest identically, or replay-identity is dead on real hardware where the
        // counter advances between runs — but a difference in the PROGRAMMED timer deadline
        // (CVAL) is real guest state and MUST diverge.
        use std::collections::BTreeMap;
        // KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM64_SYSREG (0x0013 << 16 — the
        // coprocessor selector lives in the LOW bits, not bit 48). The live counter is
        // KVM_REG_ARM_TIMER_CNT = (3,3,14,3,2); the programmed deadline is
        // KVM_REG_ARM_TIMER_CVAL = (3,3,14,0,2) — the KVM one-reg remap.
        const P: u64 = 0x6000_0000_0000_0000 | 0x0030_0000_0000_0000 | (0x0013 << 16);
        let timer_cnt = P | (3 << 14) | (3 << 11) | (14 << 7) | (3 << 3) | 2;
        let timer_cval = P | (3 << 14) | (3 << 11) | (14 << 7) | 2;

        let ram = vec![7u8; 32];
        let mut run_a = BTreeMap::new();
        run_a.insert(kvm::REG_ARM64_CORE_U64 | kvm::REG_CORE_PC, vec![0xAA; 8]);
        run_a.insert(timer_cnt, 1_000u64.to_le_bytes().to_vec());
        run_a.insert(timer_cval, 5_000u64.to_le_bytes().to_vec());
        let mut run_b = run_a.clone();
        run_b.insert(timer_cnt, 9_999_999u64.to_le_bytes().to_vec()); // the clock moved

        let vgic: &[u8] = &[];
        assert_eq!(
            digest_state(&run_a, &ram, vgic),
            digest_state(&run_b, &ram, vgic),
            "the live counter must not reach the digest"
        );

        // The programmed virtual-timer deadline (CVAL) IS guest state — changing it must
        // change the digest, or two guests with different deadlines would replay-alias.
        let mut run_d = run_a.clone();
        run_d.insert(timer_cval, 6_000u64.to_le_bytes().to_vec());
        assert_ne!(
            digest_state(&run_a, &ram, vgic),
            digest_state(&run_d, &ram, vgic),
            "the programmed timer deadline (CVAL) must reach the digest"
        );

        // And a real guest-state difference (the pc) still diverges.
        let mut run_c = run_a.clone();
        run_c.insert(kvm::REG_ARM64_CORE_U64 | kvm::REG_CORE_PC, vec![0xBB; 8]);
        assert_ne!(
            digest_state(&run_a, &ram, vgic),
            digest_state(&run_c, &ram, vgic)
        );
    }

    #[test]
    fn kvm_ioctl_numbers_match_the_abi() {
        // dir << 30 | size << 16 | type << 8 | nr, KVMIO = 0xAE.
        const fn io(nr: u64) -> u64 {
            0xAE << 8 | nr
        }
        const fn iow(nr: u64, size: u64) -> u64 {
            1 << 30 | size << 16 | 0xAE << 8 | nr
        }
        const fn ior(nr: u64, size: u64) -> u64 {
            2 << 30 | size << 16 | 0xAE << 8 | nr
        }
        const fn iowr(nr: u64, size: u64) -> u64 {
            3 << 30 | size << 16 | 0xAE << 8 | nr
        }

        assert_eq!(kvm::CREATE_VM, io(0x01));
        assert_eq!(kvm::CHECK_EXTENSION, io(0x03));
        assert_eq!(kvm::GET_VCPU_MMAP_SIZE, io(0x04));
        assert_eq!(kvm::CREATE_VCPU, io(0x41));
        assert_eq!(kvm::RUN, io(0x80));
        // struct kvm_userspace_memory_region is 32 bytes; kvm_one_reg 16; kvm_vcpu_init 32.
        assert_eq!(kvm::SET_USER_MEMORY_REGION, iow(0x46, 32));
        // Both ONE_REG ioctls are _IOW: the get form encodes write because userspace
        // writes the descriptor and the kernel fills a pointed-at buffer. Pinning them
        // to their literal ABI numbers so the direction can't drift back to _IOR.
        assert_eq!(kvm::GET_ONE_REG, iow(0xab, 16));
        assert_eq!(kvm::GET_ONE_REG, 0x4010_AEAB);
        assert_eq!(kvm::SET_ONE_REG, iow(0xac, 16));
        assert_eq!(kvm::ARM_VCPU_INIT, iow(0xae, 32));
        assert_eq!(kvm::ARM_PREFERRED_TARGET, ior(0xaf, 32));
        assert_eq!(kvm::ARM_VCPU_PSCI_0_2, 2);
        assert_eq!(kvm::GET_REG_LIST, iowr(0xb0, 8));
        // struct kvm_enable_cap is 104 bytes; kvm_create_device 12; kvm_device_attr 24.
        assert_eq!(kvm::ENABLE_CAP, iow(0xa3, 104));
        assert_eq!(kvm::CREATE_DEVICE, iowr(0xe0, 12));
        // Both DEVICE_ATTR ioctls are _IOW (same reason as ONE_REG): get is not _IOWR.
        assert_eq!(kvm::SET_DEVICE_ATTR, iow(0xe1, 24));
        assert_eq!(kvm::GET_DEVICE_ATTR, iow(0xe2, 24));
        assert_eq!(kvm::GET_DEVICE_ATTR, 0x4018_AEE2);
        // The patch draft's two additions (host/patches/0001-…).
        assert_eq!(kvm::ARM_PREEMPT_EXIT, io(0xe4));
        assert_eq!(kvm::EXIT_PREEMPT, 42);
        assert_eq!(kvm::CAP_ARM_DETERMINISTIC_INTERCEPTS, 245);

        // KVM_SET_GUEST_DEBUG is _IOW(0x9b, sizeof(struct kvm_guest_debug)). On arm64 that
        // struct is 0x208 bytes (control+pad = 8, plus a 64×u64 kvm_guest_debug_arch = 512),
        // NOT x86's 72 — and the kernel dispatches on the FULL command number, so the size
        // field is load-bearing. Pin it to the arm64 number, derived and literal both.
        assert_eq!(kvm::SET_GUEST_DEBUG, iow(0x9b, 0x208));
        assert_eq!(kvm::SET_GUEST_DEBUG, 0x4208_AE9B);
        // The single-step control bits (generic across arches).
        assert_eq!(kvm::GUESTDBG_ENABLE, 0x1);
        assert_eq!(kvm::GUESTDBG_SINGLESTEP, 0x2);
        assert_eq!(kvm::GUESTDBG_SINGLESTEP_CONTROL, 0x3);
    }

    #[test]
    fn the_vbar_register_id_names_vbar_el1() {
        // VBAR_EL1 = S3_0_C12_C0_0 = (op0,op1,crn,crm,op2) = (3,0,12,0,0). A wrong id would
        // read a different register and misclassify every exception/injection step, so derive
        // it from the architected encoding rather than trusting a bare literal.
        let enc = (3u64 << 14) | (12 << 7); // op0=3, crn=12; op1/crm/op2 all zero
        assert_eq!(kvm::REG_VBAR_EL1, kvm::arm64_sysreg(3, 0, 12, 0, 0));
        assert_eq!(
            kvm::REG_VBAR_EL1,
            0x6000_0000_0000_0000 | 0x0030_0000_0000_0000 | (0x0013 << 16) | enc
        );
        assert_eq!(kvm::REG_VBAR_EL1, 0x6030_0000_0013_C600);
    }

    #[test]
    fn guest_word_reads_the_bounded_opcode() {
        // The AA-2 stepper decodes the stepped opcode from a 4-byte guest-RAM read. Drive the
        // read through the same `guest_ram` pointer seam the machine uses (a heap allocation
        // standing in for the mapping, so Miri exercises the slice formation), then decode.
        let base = 0x4000_0000u64;
        let mut ram = [0u8; 64];
        // `ret` (0xD65F03C0) at base+0x10.
        ram[0x10..0x14].copy_from_slice(&0xD65F_03C0u32.to_le_bytes());
        // SAFETY: `ram` is a live, uniquely-owned allocation of `ram.len()` readable bytes.
        let borrowed = unsafe { guest_ram(ram.as_ptr(), ram.len()) };
        assert_eq!(guest_word(borrowed, base, base + 0x10), Some(0xD65F_03C0));
        // A word wholly inside the mapping but not at a written offset reads as zero.
        assert_eq!(guest_word(borrowed, base, base), Some(0));
        // Below the base: no such guest address.
        assert_eq!(guest_word(borrowed, base, base - 4), None);
        // The last whole word fits; one past the end does not (fail closed, never read padding).
        assert_eq!(guest_word(borrowed, base, base + 60), Some(0));
        assert_eq!(guest_word(borrowed, base, base + 61), None);
        assert_eq!(guest_word(borrowed, base, base + 64), None);
    }

    #[test]
    fn the_pc_core_register_index_is_the_one_that_names_pc() {
        // The bug this pins: 0x44 is `sp_el1`, not `pc`. Writing it set the EL1 stack
        // pointer and left PC at reset, so the guest never entered the payload — and
        // nothing failed loudly, because writing a different register succeeds.
        //
        // Derive the index the way the kernel's macro does, from the field layout of
        // `struct kvm_regs`, rather than asserting a number against itself.
        const U64: u64 = 8;
        const N_GP: u64 = 31; // user_pt_regs.regs[31]
        let sp_offset = N_GP * U64; // 0xF8
        let pc_offset = sp_offset + U64; // 0x100
        let pstate_offset = pc_offset + U64; // 0x108
        let sp_el1_offset = pstate_offset + U64; // 0x110 — the field that was written

        assert_eq!(pc_offset, 0x100);
        assert_eq!(kvm::REG_CORE_PC_OFFSET, pc_offset);
        // KVM_REG_ARM_CORE_REG(name) == offsetof(struct kvm_regs, name) / sizeof(u32).
        assert_eq!(kvm::REG_CORE_PC, pc_offset / 4);
        assert_eq!(kvm::REG_CORE_PC, 0x40);
        assert_eq!(kvm::REG_CORE_X0, 0);
        assert_eq!(kvm::REG_CORE_X_STRIDE, 2);
        assert_eq!(kvm::REG_CORE_PSTATE_OFFSET, pstate_offset);
        assert_eq!(kvm::REG_CORE_PSTATE, pstate_offset / 4);
        assert_eq!(kvm::REG_CORE_PSTATE, 0x42);
        assert_ne!(
            kvm::REG_CORE_PC,
            sp_el1_offset / 4,
            "0x44 names sp_el1; setting it is not setting the entry point"
        );

        // And the full register id the harness sends.
        assert_eq!(
            kvm::REG_ARM64_CORE_U64 | kvm::REG_CORE_PC,
            0x6030_0000_0010_0040
        );
    }

    #[test]
    fn the_id_register_id_names_id_aa64pfr0_el1() {
        // The writable-id-registers probe SETs this register; a wrong id would SET a
        // different register and read the row green. Derive it the way the kernel's
        // KVM_REG_ARM64_SYSREG macro does, from the architected (op0,op1,crn,crm,op2)
        // of ID_AA64PFR0_EL1 = (3,0,0,4,0), rather than asserting a literal.
        const ARM64: u64 = 0x6000_0000_0000_0000;
        const SIZE_U64: u64 = 0x0030_0000_0000_0000;
        const SYSREG: u64 = 0x0013 << 16; // KVM_REG_ARM_COPROC_SHIFT == 16
        let (op0, op1, crn, crm, op2) = (3u64, 0u64, 0u64, 4u64, 0u64);
        let enc = (op0 << 14) | (op1 << 11) | (crn << 7) | (crm << 3) | op2;
        assert_eq!(enc, 0xC020);
        assert_eq!(kvm::REG_ID_AA64PFR0_EL1, ARM64 | SIZE_U64 | SYSREG | enc);
        assert_eq!(kvm::REG_ID_AA64PFR0_EL1, 0x6030_0000_0013_C020);
        // It is a U64-sized register, like the core regs the harness already sets.
        assert_eq!(
            kvm::REG_ID_AA64PFR0_EL1 & kvm::REG_SIZE_MASK,
            kvm::REG_ARM64_CORE_U64 & kvm::REG_SIZE_MASK
        );
    }

    #[test]
    fn sysfs_event_string_encodes_the_raw_event() {
        // The PMCEID-backed proof reads the PMU's `events/br_retired` file; it must accept
        // exactly the event encoding, in either radix, and reject a different event or a
        // string with no `event=` term (which would false-green on an unrelated file).
        assert!(sysfs_event_encodes("event=0x21", 0x21));
        assert!(sysfs_event_encodes("event=0x21\n", 0x21)); // trailing newline (sysfs)
        assert!(sysfs_event_encodes("event=0x21,umask=0x0", 0x21)); // extra terms ignored
        assert!(sysfs_event_encodes("event=33", 0x21)); // decimal 33 == 0x21
        assert!(!sysfs_event_encodes("event=0x22", 0x21)); // a different event
        assert!(!sysfs_event_encodes("config=0x21", 0x21)); // no `event=` term
        assert!(!sysfs_event_encodes("", 0x21));
    }

    #[test]
    fn parse_cpu_list_counts_the_online_set() {
        // The /sys/devices/system/cpu/online formats: a single range, disjoint ranges +
        // singletons, and a lone CPU. This is the MACHINE's online count, which the topology
        // must record — not the process's affinity allowance.
        assert_eq!(parse_cpu_list("0-79").unwrap(), 80);
        assert_eq!(parse_cpu_list("0-79\n").unwrap(), 80);
        assert_eq!(parse_cpu_list("0,2-4,7").unwrap(), 5);
        assert_eq!(parse_cpu_list("0").unwrap(), 1);
        assert_eq!(parse_cpu_list("").unwrap(), 0);

        // Host input: a full-width range whose `b - a + 1` overflows u32 is a protocol
        // error, not a panic. So is a set of valid ranges that sums past u32::MAX.
        assert!(matches!(
            parse_cpu_list("0-4294967295"),
            Err(SysError::Protocol(_))
        ));
        assert!(matches!(
            parse_cpu_list("0-3000000000,0-3000000000"),
            Err(SysError::Protocol(_))
        ));
    }

    #[test]
    fn parse_gnu_build_id_extracts_the_running_kernel_fingerprint() {
        // One NT_GNU_BUILD_ID note (type 3, name "GNU"), desc = the build-id bytes.
        let note = |ntype: u32, name: &[u8], desc: &[u8]| {
            let mut n = Vec::new();
            n.extend_from_slice(&(name.len() as u32).to_ne_bytes());
            n.extend_from_slice(&(desc.len() as u32).to_ne_bytes());
            n.extend_from_slice(&ntype.to_ne_bytes());
            n.extend_from_slice(name);
            while n.len() % 4 != 0 {
                n.push(0);
            }
            n.extend_from_slice(desc);
            while n.len() % 4 != 0 {
                n.push(0);
            }
            n
        };
        assert_eq!(
            parse_gnu_build_id(&note(3, b"GNU\0", &[0xde, 0xad, 0xbe, 0xef])).as_deref(),
            Some("deadbeef")
        );
        // A build-id note found after an unrelated note.
        let mut two = note(1, b"GNU\0", &[0, 0, 0, 0]);
        two.extend_from_slice(&note(3, b"GNU\0", &[0x01, 0x23]));
        assert_eq!(parse_gnu_build_id(&two).as_deref(), Some("0123"));
        // No build-id note; and malformed/empty input never panics.
        assert_eq!(parse_gnu_build_id(&note(1, b"GNU\0", &[0xaa])), None);
        assert_eq!(parse_gnu_build_id(&[]), None);
        assert_eq!(parse_gnu_build_id(&[1, 2, 3]), None);
    }

    #[test]
    fn the_vgic_addresses_are_the_ones_the_payload_runtime_programs() {
        // If these drift from `payloads/runtime/src/gic.rs`, the guest's GIC stores
        // become MMIO exits the measurement loop refuses — and no payload boots.
        assert_eq!(kvm::GICD_BASE, 0x0800_0000);
        assert_eq!(kvm::GICR_BASE, 0x080A_0000);
        assert_eq!(kvm::DEV_TYPE_ARM_VGIC_V3, 7);
    }

    #[test]
    fn only_the_patch_row_is_expected_absent() {
        // AA-0's expect column: every mandatory row must be present on any usable box;
        // the determinism cap appears only once the patched kernel boots (AA-3), so it
        // is the one row expected absent — and it is NOT in the mandatory set.
        for cap in Capability::mandatory_aa0() {
            assert!(
                cap.expect_present(),
                "{} is mandatory, so it must be expect-present",
                cap.name()
            );
            assert!(
                !matches!(cap, Capability::DeterministicIntercepts),
                "the expect-absent patch marker must not be in the mandatory set"
            );
        }
        // The seven host-probeable mandatory rows, exactly.
        assert_eq!(Capability::mandatory_aa0().len(), 7);
        assert!(Capability::DevKvm.expect_present());
        assert!(Capability::Pmceid.expect_present());
        assert!(Capability::HostOverflowDelivers.expect_present());
        assert!(Capability::Vgicv3Creatable.expect_present());
        assert!(Capability::WritableIdRegisters.expect_present());
        assert!(!Capability::DeterministicIntercepts.expect_present());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn off_linux_every_probe_is_unprobed_never_a_no() {
        // The distinction a disposition rests on: "I could not ask" is not "no".
        for cap in [
            Capability::DevKvm,
            Capability::PerfBrRetired,
            Capability::GuestDebug,
            Capability::Pmceid,
            Capability::HostOverflowDelivers,
            Capability::Vgicv3Creatable,
            Capability::WritableIdRegisters,
            Capability::DeterministicIntercepts,
        ] {
            assert!(matches!(probe(cap), Err(SysError::Unsupported)));
        }
    }
}
