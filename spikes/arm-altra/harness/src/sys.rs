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

/// The raw `BR_RETIRED` event on aarch64 PMUv3: retired *taken* branches
/// (`docs/ARM-PORT.md`, `docs/ARM-ALTRA.md` §2). Not invented here — it is the
/// event those documents name, surfaced as a constant so the harness cannot
/// silently arm a different one.
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

    /// `KVM_REG_ARM_CORE_REG(regs.pc)` — the register index, which the macro defines
    /// as the byte offset divided by four: `0x100 / 4 == 0x40`.
    ///
    /// This was `0x44` — the index of the field at byte `0x110`, which is `sp_el1`.
    /// Setting the entry point therefore wrote the EL1 stack pointer and left `PC` at
    /// its reset value, so the guest never entered the payload at all. The constant is
    /// now *derived* from the offset and pinned by a test, because an off-by-one in a
    /// register index does not fail loudly — it writes a different register.
    pub const REG_CORE_PC: u64 = REG_CORE_PC_OFFSET / 4;
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
            let len = (run.mmio.len as usize).min(run.mmio.data.len());
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

/// Whether a `KVM_GET_ONE_REG` id names a **host-time-derived** register — one whose
/// value advances with elapsed real time and so must not enter a determinism digest.
///
/// These are the generic-timer *counters*: `CNTPCT_EL0`, `CNTVCT_EL0`, their
/// self-synchronized `…SS` variants, and the `KVM_REG_ARM_TIMER_CNT` pseudo-register.
/// All live at the arm64 system-register coordinates `op0=3, op1=3, CRn=14`. The
/// *comparators* (`…CVAL`), *controls* (`…CTL`, `CNTKCTL`), and the constant
/// `CNTFRQ` are deterministic guest-programmed state and are kept.
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
    // The counter registers, by (CRm, op2):
    //   CNTPCT_EL0   = (0, 1)   CNTVCT_EL0   = (0, 2)
    //   CNTPCTSS_EL0 = (0, 5)   CNTVCTSS_EL0 = (0, 6)
    //   KVM_REG_ARM_TIMER_CNT = ARM64_SYS_REG(3,3,14,3,2) = (3, 2)
    // The controls/comparators/frequency (CNTFRQ (0,0), *CTL, *CVAL, CNTKCTL) are
    // deterministic guest state and are NOT excluded.
    matches!((crm, op2), (0, 1) | (0, 2) | (0, 5) | (0, 6) | (3, 2))
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
    /// `BR_RETIRED` (event 0x21) is an **implemented** PMU event — `perf_event_open` of
    /// the raw event does not return `ENOENT`. The truth table's `br-retired-pmceid0` row:
    /// the whole work-clock bet rests on this event existing on N1's PMU.
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
            Capability::Pmceid => "br-retired-pmceid0",
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

    /// The truth-table `br-retired-pmceid0` row: is raw `BR_RETIRED` (0x21) an
    /// **implemented** PMU event at all? `perf_event_open` returns `ENOENT`/`EOPNOTSUPP`
    /// for an event the PMU does not implement; any clean open means it exists (this is
    /// the weaker "implemented" row — [`probe_br_retired`] then checks it can be
    /// *scheduled* pinned and non-multiplexed).
    fn probe_br_retired_implemented() -> Result<bool, SysError> {
        let mut attr = br_retired_attr(None);
        attr.flags &= !perf_flags::EXCLUDE_HOST;
        attr.flags |= perf_flags::EXCLUDE_KERNEL;
        // SAFETY: `attr` is fully initialized; count this thread (pid 0), any cpu, no group.
        let fd = unsafe { perf_event_open(&raw const attr, 0, -1, -1, 0) };
        if fd < 0 {
            let e = errno();
            if e == libc::ENOENT || e == libc::EOPNOTSUPP {
                return Ok(false);
            }
            return Err(SysError::Errno {
                call: "perf_event_open(BR_RETIRED pmceid)",
                errno: e,
            });
        }
        // SAFETY: `fd` is a valid descriptor this function owns.
        unsafe { libc::close(fd as i32) };
        Ok(true)
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
        let page = 4096usize;
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
pub use machine::{Machine, Mechanism, ParamsPage, PerfCounter, pin_to_core};

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

        // A too-wide `len` must be clamped to the 8-byte buffer, never read past it.
        run.mmio.len = 999;
        if let VcpuExit::Mmio { data, .. } = decode_kvm_run(&run) {
            assert_eq!(data.len(), 8, "len clamped to the data buffer");
        } else {
            panic!("expected Mmio");
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
    fn host_time_registers_are_the_generic_timer_counters() {
        // ARM64_SYS_REG(op0,op1,crn,crm,op2) id builder.
        // KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM64_SYSREG (0x0013 << 16 — the
        // coprocessor selector lives in the LOW bits, not bit 48). A real timer-counter
        // id has this exact prefix; the earlier bit-48 form matched nothing.
        const P: u64 = 0x6000_0000_0000_0000 | 0x0030_0000_0000_0000 | (0x0013 << 16);
        let sysreg = |op0: u64, op1: u64, crn: u64, crm: u64, op2: u64| {
            P | (op0 << 14) | (op1 << 11) | (crn << 7) | (crm << 3) | op2
        };

        // The live counters — excluded.
        assert!(is_host_time_register(sysreg(3, 3, 14, 0, 1)), "CNTPCT_EL0");
        assert!(is_host_time_register(sysreg(3, 3, 14, 0, 2)), "CNTVCT_EL0");
        assert!(
            is_host_time_register(sysreg(3, 3, 14, 0, 5)),
            "CNTPCTSS_EL0"
        );
        assert!(
            is_host_time_register(sysreg(3, 3, 14, 0, 6)),
            "CNTVCTSS_EL0"
        );
        assert!(is_host_time_register(sysreg(3, 3, 14, 3, 2)), "TIMER_CNT");

        // Deterministic timer state — KEPT.
        assert!(!is_host_time_register(sysreg(3, 3, 14, 0, 0)), "CNTFRQ_EL0");
        assert!(!is_host_time_register(sysreg(3, 3, 14, 3, 1)), "CNTV_CTL");
        assert!(!is_host_time_register(sysreg(3, 3, 14, 0, 3)), "a CVAL");
        assert!(
            !is_host_time_register(sysreg(3, 0, 14, 1, 0)),
            "CNTKCTL_EL1"
        );

        // A core register (the pc) is not a sysreg and is kept.
        assert!(!is_host_time_register(
            kvm::REG_ARM64_CORE_U64 | kvm::REG_CORE_PC
        ));

        // The load-bearing anti-regression: a *literal* real KVM register id, not one
        // built from the same prefix the predicate uses, so the SYSREG selector position
        // is pinned independently. `CNTVCT_EL0` = ARM64_SYS_REG(3,3,14,0,2): the
        // coprocessor selector `0x0013` sits at bits 16–27 (`…_0013_DF02`), never bit 48.
        assert_eq!(
            sysreg(3, 3, 14, 0, 2),
            0x6030_0000_0013_DF02,
            "CNTVCT_EL0 id"
        );
        assert!(
            is_host_time_register(0x6030_0000_0013_DF02),
            "the real CNTVCT_EL0 id must be recognized"
        );
    }

    #[test]
    fn a_digest_ignores_the_live_clock_so_replay_survives_scheduling_jitter() {
        // The flagship fix: two same-seed runs whose ONLY difference is the live
        // virtual counter must digest identically, or replay-identity is dead on real
        // hardware where the counter advances between runs.
        use std::collections::BTreeMap;
        // KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM64_SYSREG (0x0013 << 16 — the
        // coprocessor selector lives in the LOW bits, not bit 48). A real timer-counter
        // id has this exact prefix; the earlier bit-48 form matched nothing.
        const P: u64 = 0x6000_0000_0000_0000 | 0x0030_0000_0000_0000 | (0x0013 << 16);
        let cntvct = P | (3 << 14) | (3 << 11) | (14 << 7) | 2; // op0=3,op1=3,crn=14,crm=0,op2=2

        let ram = vec![7u8; 32];
        let mut run_a = BTreeMap::new();
        run_a.insert(kvm::REG_ARM64_CORE_U64 | kvm::REG_CORE_PC, vec![0xAA; 8]);
        run_a.insert(cntvct, 1_000u64.to_le_bytes().to_vec());
        let mut run_b = run_a.clone();
        run_b.insert(cntvct, 9_999_999u64.to_le_bytes().to_vec()); // the clock moved

        let vgic: &[u8] = &[];
        assert_eq!(
            digest_state(&run_a, &ram, vgic),
            digest_state(&run_b, &ram, vgic),
            "the live counter must not reach the digest"
        );

        // But a real guest-state difference (the pc) still diverges.
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
