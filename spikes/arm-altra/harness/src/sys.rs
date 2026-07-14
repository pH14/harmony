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
    /// `KVM_GET_ONE_REG` — `_IOR(KVMIO, 0xab, struct kvm_one_reg)`.
    pub const GET_ONE_REG: u64 = 0x8010_AEAB;
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
    /// `KVM_DEV_ARM_VGIC_CTRL_INIT`.
    pub const DEV_ARM_VGIC_CTRL_INIT: u64 = 0;
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

/// Hash a landed guest state — every architectural register (in sorted id order) plus
/// all of guest RAM — into the digest AA-3's replay-identity and AA-6's bit-identity
/// floors compare.
///
/// Registers are a `BTreeMap` so iteration order (which reaches the hashed bytes) is
/// the register id, never insertion order (Conventions rule 4). Pure and Miri-testable:
/// `machine` reads the registers by ioctl and forms the RAM slice from its mapping,
/// then hands both here — so the hashing itself, and the order discipline, are
/// interpreter-checked.
#[must_use]
pub fn digest_state(regs: &std::collections::BTreeMap<u64, Vec<u8>>, ram: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"arm-spike-state-v1");
    for (id, value) in regs {
        h.update(id.to_le_bytes());
        h.update(value);
    }
    h.update(ram);
    format!("sha256:{}", crate::evidence::hex_lower(&h.finalize()))
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
            Capability::DeterministicIntercepts => "kvm-cap-arm-deterministic-intercepts",
        }
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

    use super::{BR_RETIRED_RAW, Capability, PerfEventAttr, SysError, br_retired_attr, kvm};

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

    /// Whether raw `BR_RETIRED` opens as a pinned, guest-only event (AA-0's PMU
    /// row, and the precondition for the entire work-clock bet).
    fn probe_br_retired() -> Result<bool, SysError> {
        let attr = br_retired_attr(None);
        // SAFETY: `attr` is a fully initialized perf_event_attr on this frame; the
        // pointer is valid for the call. Counting this thread (pid 0) on whatever
        // CPU it runs on (-1), no group.
        let fd = unsafe { perf_event_open(&raw const attr, 0, -1, -1, 0) };
        if fd < 0 {
            let e = errno();
            // ENOENT/EOPNOTSUPP mean the event is not implemented here — a real
            // "no". Any other errno is a failure to probe, and must not be
            // flattened into one.
            if e == libc::ENOENT || e == libc::EOPNOTSUPP {
                return Ok(false);
            }
            return Err(SysError::Errno {
                call: "perf_event_open(BR_RETIRED)",
                errno: e,
            });
        }
        // SAFETY: `fd` is a valid descriptor returned just above.
        unsafe { libc::close(fd as i32) };
        let _ = BR_RETIRED_RAW;
        Ok(true)
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

        let mut a = BTreeMap::new();
        a.insert(2u64, vec![0xAA]);
        a.insert(1u64, vec![0xBB]);
        // Insertion order differs; the BTreeMap makes the digest identical anyway.
        let mut b = BTreeMap::new();
        b.insert(1u64, vec![0xBB]);
        b.insert(2u64, vec![0xAA]);
        assert_eq!(digest_state(&a, &ram), digest_state(&b, &ram));

        // Different RAM → different digest (the RAM really is hashed).
        let mut other_ram = ram.clone();
        other_ram[0] = 1;
        assert_ne!(digest_state(&a, &ram), digest_state(&a, &other_ram));

        // Different register value → different digest.
        let mut c = a.clone();
        c.insert(1u64, vec![0xCC]);
        assert_ne!(digest_state(&a, &ram), digest_state(&c, &ram));

        assert!(digest_state(&a, &ram).starts_with("sha256:"));
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
        assert_eq!(kvm::GET_ONE_REG, ior(0xab, 16));
        assert_eq!(kvm::SET_ONE_REG, iow(0xac, 16));
        assert_eq!(kvm::ARM_VCPU_INIT, iow(0xae, 32));
        assert_eq!(kvm::ARM_PREFERRED_TARGET, ior(0xaf, 32));
        assert_eq!(kvm::GET_REG_LIST, iowr(0xb0, 8));
        // struct kvm_enable_cap is 104 bytes; kvm_create_device 12; kvm_device_attr 24.
        assert_eq!(kvm::ENABLE_CAP, iow(0xa3, 104));
        assert_eq!(kvm::CREATE_DEVICE, iowr(0xe0, 12));
        assert_eq!(kvm::SET_DEVICE_ATTR, iow(0xe1, 24));
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
    fn the_vgic_addresses_are_the_ones_the_payload_runtime_programs() {
        // If these drift from `payloads/runtime/src/gic.rs`, the guest's GIC stores
        // become MMIO exits the measurement loop refuses — and no payload boots.
        assert_eq!(kvm::GICD_BASE, 0x0800_0000);
        assert_eq!(kvm::GICR_BASE, 0x080A_0000);
        assert_eq!(kvm::DEV_TYPE_ARM_VGIC_V3, 7);
    }

    #[test]
    fn only_the_patch_row_is_expected_absent() {
        // AA-0's expect column: three rows must be present on any usable box; the
        // determinism cap appears only once the patched kernel boots (AA-3).
        assert!(Capability::DevKvm.expect_present());
        assert!(Capability::PerfBrRetired.expect_present());
        assert!(Capability::GuestDebug.expect_present());
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
            Capability::DeterministicIntercepts,
        ] {
            assert!(matches!(probe(cap), Err(SysError::Unsupported)));
        }
    }
}
