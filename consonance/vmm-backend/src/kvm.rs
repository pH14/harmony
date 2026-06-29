// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **pure** KVM exit-mapping + state-conversion logic for `KvmBackend`
//! (`#[cfg(target_os = "linux")]`).
//!
//! Everything here issues **no syscall**: the `kvm_run` ⇄ `Exit`/completion
//! translation (`RunPage` + `decode_*`/`apply_*`), the `kvm_bindings` ⇄
//! [`crate::state::VcpuState`] conversions, and the snapshot-shape / CPUID-table /
//! MSR-count / capability helpers. It is driven by **non-`#[ignore]` unit tests
//! with synthetic `kvm_run`/`kvm_*` structs** (`#[cfg(test)] mod tests`), so the
//! Linux CI runner exercises it under `nextest`/`llvm-cov`/`mutants` and Miri
//! scrutinizes the raw-pointer reads for UB — all without `/dev/kvm`.
//!
//! The box-only **syscall orchestration** (the `KvmBackend` struct, its
//! `Backend` impl, and the raw `mmap`/`ioctl` wrappers) lives in
//! [`crate::kvm_sys`], which is the one module excluded from the coverage and
//! mutation gates (it cannot run without `/dev/kvm`); it calls the pure helpers
//! below and is otherwise just KVM ioctls.

use std::collections::BTreeMap;

use kvm_bindings::{
    // NB: `SIGNIFCANT` (missing the 'I') is an upstream typo in kvm-bindings /
    // the kernel uapi, faithfully preserved here.
    KVM_CPUID_FLAG_SIGNIFCANT_INDEX,
    KVM_EXIT_DEBUG,
    KVM_EXIT_FAIL_ENTRY,
    KVM_EXIT_HLT,
    KVM_EXIT_INTERNAL_ERROR,
    KVM_EXIT_INTR,
    KVM_EXIT_IO,
    KVM_EXIT_IO_IN,
    KVM_EXIT_IRQ_WINDOW_OPEN,
    KVM_EXIT_MMIO,
    KVM_EXIT_SHUTDOWN,
    KVM_EXIT_X86_RDMSR,
    KVM_EXIT_X86_WRMSR,
    KVM_MP_STATE_HALTED,
    KVM_MP_STATE_RUNNABLE,
    kvm_cpuid_entry2,
    kvm_debugregs,
    kvm_dtable,
    kvm_msr_entry,
    kvm_regs,
    kvm_run,
    kvm_segment,
    kvm_sregs2,
    kvm_vcpu_events,
    kvm_xcrs,
    kvm_xsave,
};

use crate::config::{CpuidModel, MsrFilter};
use crate::error::{BackendError, Result};
use crate::exit::{Capabilities, Exit};
use crate::run_buf::RunBuf;
use crate::state::{
    DebugRegs, DescriptorTable, MpState, Segment, VcpuEvents, VcpuRegs, VcpuSregs, VcpuState,
};
use crate::types::Gpa;

/// Map a `kvm-ioctls`/`vmm-sys-util` errno into a portable [`BackendError`].
pub(crate) fn kvm_err(e: kvm_ioctls::Error) -> BackendError {
    BackendError::Io(std::io::Error::from_raw_os_error(e.errno()))
}

// ---------------------------------------------------------------------------
// `KVM_EXIT_DETERMINISM` ABI — the patched-KVM determinism-intercept surface
// (`PatchedKvmBackend`). Validated against
// `consonance/vmm-backend/kvm-patches/patches/0001-*.patch`: a new exit reason `41`, a
// `kvm_run.determinism` payload overlaying the exit-info union, and the opt-in
// cap `245`. Stock KVM never enables the cap, so it never produces this exit —
// the decode below is dead for `KvmBackend` and live only for the patched
// backend (and the synthetic-`kvm_run` unit tests). These are NOT in
// `kvm-bindings` (the patch is out-of-tree), so the payload is read/written by
// raw, bounded offset rather than a typed union member.
// ---------------------------------------------------------------------------

/// `KVM_EXIT_DETERMINISM` (patch 0001).
pub(crate) const KVM_EXIT_DETERMINISM: u32 = 41;
/// harmony 0005: MTF single-step exit (one instruction retired, incl. through events).
pub(crate) const KVM_EXIT_DET_STEP: u32 = 43;
/// `KVM_CAP_X86_DETERMINISTIC_INTERCEPTS` (patch 0001) — opt-in, settable only
/// before vCPU creation.
pub(crate) const KVM_CAP_X86_DETERMINISTIC_INTERCEPTS: u32 = 245;

/// `KVM_EXIT_PREEMPT` (patch 0004) — the in-kernel **force-exit** preemption: when
/// the one-shot arm is set, the perf-overflow PMI's NMI VM-exit returns to userspace
/// with this reason **instead of re-entering**, so the V-time deadline is hit with
/// only the bounded hardware-PMI skid (task 55). It carries no payload — the work
/// count is read from the perf counter, exactly like the `SIGIO`-kick path it
/// replaces. Stock KVM never arms it, so it is never produced there.
pub(crate) const KVM_EXIT_PREEMPT: u32 = 42;

/// `determinism.insn` kinds (kernel → user, patch 0001).
const KVM_DETERMINISM_RDTSC: u32 = 0;
const KVM_DETERMINISM_RDTSCP: u32 = 1;
const KVM_DETERMINISM_RDRAND: u32 = 2;
const KVM_DETERMINISM_RDSEED: u32 = 3;
/// `determinism.flags` bit (user → kernel): request `CF = 1` (RNG success).
const KVM_DETERMINISM_FLAG_CF: u8 = 1;

/// Byte offset of the exit-info union (`__bindgen_anon_1`) within `kvm_run`. The
/// `determinism` payload overlays it exactly like `io`/`mmio`. Derived with
/// `offset_of!` rather than hardcoded (the spike used a literal `32`) so a uapi
/// layout change can never silently desync the raw determinism-field access.
const DET_BASE: usize = core::mem::offset_of!(kvm_run, __bindgen_anon_1);
// `determinism` field offsets, relative to `DET_BASE` (patch 0001 struct order):
//   insn u32 @0, width u32 @4, value u64 @8, aux u64 @16, flags u8 @24, dest u8 @25.
const DET_INSN: usize = DET_BASE; // __u32 insn   (kernel -> user)
const DET_WIDTH: usize = DET_BASE + 4; // __u32 width  (kernel -> user)
const DET_VALUE: usize = DET_BASE + 8; // __u64 value  (user -> kernel)
const DET_AUX: usize = DET_BASE + 16; // __u64 aux    (user -> kernel, RDTSCP)
const DET_FLAGS: usize = DET_BASE + 24; // __u8  flags  (user -> kernel, RNG CF)

/// What the last returned exit awaits, with the `kvm_run` context needed to write
/// its completion before the next entry. Stock KVM never surfaces
/// Hypercall/Cpuid (serviced in-kernel), so there is no pending variant for them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Pending {
    None,
    /// `Io { write: None }`: write the IN value into the PIO data buffer.
    IoIn {
        data_offset: u64,
        size: u8,
    },
    /// `Mmio { write: None }`: write the load value into `mmio.data[..len]`.
    MmioLoad {
        len: u32,
    },
    /// `Rdmsr`: `complete_read` sets `msr.data` (+ `error = 0`), `complete_fault`
    /// sets `error != 0`.
    Rdmsr,
    /// `Wrmsr`: `complete_ok` resumes with `error = 0`, `complete_fault` with
    /// `error != 0`.
    Wrmsr,
    /// A `KVM_EXIT_DETERMINISM` intercept (patched KVM): `complete_read(value)`
    /// writes `determinism.value` (→ dest / EDX:EAX). `rdtscp` additionally
    /// writes `IA32_TSC_AUX` → ECX; `rng` (RDRAND/RDSEED) additionally sets
    /// `CF` (deterministic success).
    Determinism {
        /// The intercepted instruction was `RDTSCP` (carry `aux` → ECX).
        rdtscp: bool,
        /// The intercepted instruction was `RDRAND`/`RDSEED` (set `CF`).
        rng: bool,
    },
}

// ---------------------------------------------------------------------------
// The pure KVM exit-mapping seam.
//
// `RunPage` is a raw view over the `mmap`-ed `kvm_run` (or, in the unit tests, a
// synthetic buffer). The `decode_*` / `apply_*` functions are the entire
// `kvm_run` ⇄ `Exit`/completion translation, written against `RunPage` and
// issuing **no syscall**.
// ---------------------------------------------------------------------------

/// A raw view over the `kvm_run` page. All access goes through the single raw
/// pointer — no long-lived `&`/`&mut kvm_run` is created — so the typed-field
/// reads and the byte-offset PIO access never alias as conflicting references.
#[derive(Clone, Copy)]
pub(crate) struct RunPage {
    run: *mut kvm_run,
    len: usize,
}

impl RunPage {
    /// # Safety
    /// `run` must point to a valid, initialized `kvm_run` of at least `len` bytes
    /// (the live `mmap`, or a test buffer ≥ `size_of::<kvm_run>()`), exclusively
    /// owned for the duration of use.
    pub(crate) unsafe fn new(run: *mut kvm_run, len: usize) -> Self {
        Self { run, len }
    }

    pub(crate) fn exit_reason(&self) -> u32 {
        // SAFETY: `run` is a valid `kvm_run` (constructor contract); `exit_reason`
        // is a plain field, always initialized.
        unsafe { (*self.run).exit_reason }
    }

    /// The `KVM_EXIT_IO` fields: `(direction, size, port, count, data_offset)`.
    fn io(&self) -> (u8, u8, u16, u32, u64) {
        // SAFETY: read only for a `KVM_EXIT_IO`, where `io` is the active union
        // member; the struct is `Copy` and read out wholesale.
        let io = unsafe { (*self.run).__bindgen_anon_1.io };
        (io.direction, io.size, io.port, io.count, io.data_offset)
    }

    /// The `KVM_EXIT_MMIO` fields: `(phys_addr, len, is_write, data)`.
    fn mmio(&self) -> (u64, u32, u8, [u8; 8]) {
        // SAFETY: active union member for a `KVM_EXIT_MMIO`; `Copy` read.
        let m = unsafe { (*self.run).__bindgen_anon_1.mmio };
        (m.phys_addr, m.len, m.is_write, m.data)
    }

    /// The `KVM_EXIT_X86_{RD,WR}MSR` fields: `(index, data)`.
    fn msr(&self) -> (u32, u64) {
        // SAFETY: active union member for an MSR exit; `Copy` read.
        let m = unsafe { (*self.run).__bindgen_anon_1.msr };
        (m.index, m.data)
    }

    /// Read the low `size` bytes (≤4) of the first PIO data item at `data_offset`
    /// through the bounded `run_buf` seam.
    fn read_pio(&self, data_offset: u64, size: u8) -> Result<u32> {
        let n = (size as usize).min(4);
        // SAFETY: `run`/`len` describe a live buffer; `RunBuf` bound-checks the
        // offset and rejects anything past `len`.
        let buf = unsafe { RunBuf::new(self.run.cast::<u8>(), self.len) };
        let mut bytes = [0u8; 4];
        buf.read_bytes(data_offset as usize, &mut bytes[..n])?;
        Ok(u32::from_le_bytes(bytes))
    }

    /// Write the low `size` bytes (≤4) of `value` into the PIO data buffer at
    /// `data_offset` (the IN completion), through the bounded `run_buf` seam.
    fn write_pio(&self, data_offset: u64, size: u8, value: u64) -> Result<()> {
        let n = (size as usize).min(4);
        let bytes = (value as u32).to_le_bytes();
        // SAFETY: as `read_pio`; `RunBuf` bound-checks the write.
        let mut buf = unsafe { RunBuf::new(self.run.cast::<u8>(), self.len) };
        buf.write_bytes(data_offset as usize, &bytes[..n])
    }

    /// Write the low `len` bytes (≤8) of `value` into `mmio.data` (the MMIO load
    /// completion).
    fn write_mmio_data(&self, len: u32, value: u64) {
        let n = (len as usize).min(8);
        let bytes = value.to_le_bytes();
        // SAFETY: active union member for a pending MMIO load; `n <= 8 ==
        // data.len()`, so the in-place slice write stays in-bounds. The `&mut` to
        // the union field is made explicit (no implicit autoref through the raw
        // pointer).
        unsafe {
            let data = &mut (*self.run).__bindgen_anon_1.mmio.data;
            data[..n].copy_from_slice(&bytes[..n]);
        }
    }

    /// Set the MSR completion `data` + `error` (the RDMSR value path).
    fn set_msr(&self, data: u64, error: u8) {
        // SAFETY: active union member for a pending MSR exit; in-place field writes.
        unsafe {
            let m = &mut (*self.run).__bindgen_anon_1.msr;
            m.data = data;
            m.error = error;
        }
    }

    /// Set only the MSR completion `error` (the allow/deny-gp resolution).
    fn set_msr_error(&self, error: u8) {
        // SAFETY: active union member for a pending MSR exit; in-place field write.
        unsafe {
            (*self.run).__bindgen_anon_1.msr.error = error;
        }
    }

    /// Read a little-endian `u32` at byte `off` through the bounded `run_buf`
    /// seam (the `determinism` payload is not a `kvm-bindings` union member).
    fn read_u32_at(&self, off: usize) -> Result<u32> {
        // SAFETY: `run`/`len` describe a live buffer; `RunBuf` bound-checks the
        // offset and rejects anything past `len`.
        let buf = unsafe { RunBuf::new(self.run.cast::<u8>(), self.len) };
        let mut b = [0u8; 4];
        buf.read_bytes(off, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// Write a little-endian `u64` at byte `off` through the bounded `run_buf`
    /// seam.
    fn write_u64_at(&self, off: usize, value: u64) -> Result<()> {
        // SAFETY: as `read_u32_at`; `RunBuf` bound-checks the write.
        let mut buf = unsafe { RunBuf::new(self.run.cast::<u8>(), self.len) };
        buf.write_bytes(off, &value.to_le_bytes())
    }

    /// Write a single byte at `off` through the bounded `run_buf` seam.
    fn write_u8_at(&self, off: usize, value: u8) -> Result<()> {
        // SAFETY: as `read_u32_at`; `RunBuf` bound-checks the write.
        let mut buf = unsafe { RunBuf::new(self.run.cast::<u8>(), self.len) };
        buf.write_bytes(off, &[value])
    }

    /// `determinism.insn` (kernel → user): which instruction was intercepted.
    fn det_insn(&self) -> Result<u32> {
        self.read_u32_at(DET_INSN)
    }

    /// `determinism.width` (kernel → user): result width in bytes.
    fn det_width(&self) -> Result<u32> {
        self.read_u32_at(DET_WIDTH)
    }

    /// `kvm_run.ready_for_interrupt_injection` (kernel → user, written by KVM in
    /// `post_kvm_run_save` after **every** `KVM_RUN`): non-zero iff the guest can
    /// accept a maskable interrupt on the next entry. KVM derives it from
    /// `RFLAGS.IF`, the STI / MOV-SS interrupt shadow, and whether an event is
    /// already being injected — so it is the single authoritative injectability
    /// gate (the three conditions the task lists, folded into one byte). A plain
    /// top-level field, not part of the exit-info union, read by typed access.
    fn ready_for_interrupt_injection(&self) -> u8 {
        // SAFETY: `run` is a valid `kvm_run` (constructor contract); this is a
        // plain, always-initialized top-level field.
        unsafe { (*self.run).ready_for_interrupt_injection }
    }

    /// Set `kvm_run.request_interrupt_window` (user → kernel): when non-zero, the
    /// next `KVM_RUN` exits with `KVM_EXIT_IRQ_WINDOW_OPEN` as soon as the guest is
    /// injectable, so a vector that could not be delivered immediately is retried
    /// at that exit. A plain top-level field, written by typed access.
    fn set_request_interrupt_window(&self, on: bool) {
        // SAFETY: as above; in-place write of a plain top-level field.
        unsafe {
            (*self.run).request_interrupt_window = u8::from(on);
        }
    }
}

/// The action [`plan_irq_entry`] decided for the next VM-entry under the
/// userspace irqchip (`KVM_IRQCHIP_NONE`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum IrqEntry {
    /// Queue this vector via the `KVM_INTERRUPT` ioctl, then enter the guest.
    Queue(u8),
    /// Enter the guest directly — nothing pending, or a pending vector is waiting
    /// on the interrupt window (which this call armed).
    Run,
}

/// Decide what to do with a pending maskable IRQ immediately before `KVM_RUN`,
/// and arm/clear the interrupt-window request on the run page to match. This is
/// the standard userspace-irqchip injection handshake: queue the vector now if
/// the guest can take it; otherwise ask KVM to exit (`KVM_EXIT_IRQ_WINDOW_OPEN`)
/// the moment it can, so the caller retries the same vector on that exit.
///
/// Pure: reads `ready_for_interrupt_injection`, writes `request_interrupt_window`,
/// and issues **no syscall** — the orchestration layer performs the
/// `KVM_INTERRUPT` ioctl for [`IrqEntry::Queue`]. (Box-only syscalls cannot live
/// here; this is the part the synthetic-`kvm_run` unit tests + Miri exercise.)
pub(crate) fn plan_irq_entry(page: RunPage, pending_irq: Option<u8>) -> IrqEntry {
    match pending_irq {
        // Injectable now: clear any window request and queue the vector.
        Some(vector) if page.ready_for_interrupt_injection() != 0 => {
            page.set_request_interrupt_window(false);
            IrqEntry::Queue(vector)
        }
        // Pending but not injectable yet: ask KVM to exit when the window opens.
        Some(_) => {
            page.set_request_interrupt_window(true);
            IrqEntry::Run
        }
        // Nothing pending: ensure no stale window request is left armed.
        None => {
            page.set_request_interrupt_window(false);
            IrqEntry::Run
        }
    }
}

/// Decode the current `kvm_run` into an `Exit` (or `None` for a control exit the
/// run loop re-enters on) plus the completion it requires. Pure: reads `page`,
/// issues no syscall.
pub(crate) fn decode_exit(page: RunPage) -> Result<Option<(Exit, Pending)>> {
    match page.exit_reason() {
        KVM_EXIT_IO => decode_io(page).map(Some),
        KVM_EXIT_MMIO => Ok(Some(decode_mmio(page))),
        KVM_EXIT_X86_RDMSR => {
            let (index, _) = page.msr();
            Ok(Some((Exit::Rdmsr { index }, Pending::Rdmsr)))
        }
        KVM_EXIT_X86_WRMSR => {
            let (index, value) = page.msr();
            Ok(Some((Exit::Wrmsr { index, value }, Pending::Wrmsr)))
        }
        KVM_EXIT_DETERMINISM => decode_determinism(page).map(Some),
        KVM_EXIT_HLT => Ok(Some((Exit::Hlt, Pending::None))),
        KVM_EXIT_SHUTDOWN => Ok(Some((Exit::Shutdown, Pending::None))),
        KVM_EXIT_INTERNAL_ERROR => Err(BackendError::Internal("KVM_EXIT_INTERNAL_ERROR")),
        KVM_EXIT_FAIL_ENTRY => Err(BackendError::Internal("KVM_EXIT_FAIL_ENTRY")),
        // Run-loop control exits — consumed internally, never surfaced.
        KVM_EXIT_IRQ_WINDOW_OPEN => Ok(None),
        // harmony 0005 (defense-in-depth): a one-shot MTF single-step is disarmed
        // in-kernel on its own exit (vmx_handle_exit), so a stale KVM_EXIT_DET_STEP
        // cannot normally reach a non-stepping `run`; if one ever races through,
        // swallow it as a transparent re-entry rather than aborting as "unhandled".
        KVM_EXIT_DET_STEP => Ok(None),
        _ => Err(BackendError::Internal("unhandled KVM exit reason")),
    }
}

/// How a `KVM_RUN` during the `run_until` (overflow-early + single-step) path
/// stopped, classified from the raw `kvm_run.exit_reason` **before** [`decode_exit`]
/// (which rejects the debug-trap and signal reasons as "unhandled" — they are not
/// guest-observable exits). Pure: reads `page`, issues no syscall.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StepStop {
    /// `KVM_EXIT_DEBUG`: a single-step trap — exactly one instruction retired, NOT
    /// a guest exit. Read the work counter and advance the planner.
    SingleStepTrap,
    /// `KVM_EXIT_INTR`: a signal (the PMU-overflow kick, or a spurious one) broke
    /// `KVM_RUN` out. The caller checks the counter against the armed point. (This
    /// reason is usually observed as the ioctl's `EINTR` return; handled here too
    /// for the rare in-band case.)
    Interrupted,
    /// `KVM_EXIT_PREEMPT` (patch 0004): the in-kernel **force-exit** fired — the
    /// perf-overflow PMI's NMI VM-exit returned to userspace instead of re-entering.
    /// Handled exactly like [`Interrupted`](Self::Interrupted): read the counter and
    /// stop iff the overflow reached the armed point (it always has — the NMI fires
    /// at the overflow). This is the bounded-skid kick that replaces the unbounded
    /// `SIGIO` for the deterministic preemption (task 55).
    Preempt,
    /// `KVM_EXIT_IRQ_WINDOW_OPEN`: a run-loop control exit; re-enter.
    Reenter,
    /// Any other reason: a genuine guest exit — decode it with [`decode_exit`] and
    /// return it from `run_until` (short of the deadline).
    GuestExit,
}

/// Classify the current `kvm_run` for the `run_until` path. Pure.
pub(crate) fn classify_step_exit(page: RunPage) -> StepStop {
    match page.exit_reason() {
        KVM_EXIT_DEBUG => StepStop::SingleStepTrap,
        KVM_EXIT_DET_STEP => StepStop::SingleStepTrap,
        KVM_EXIT_INTR => StepStop::Interrupted,
        KVM_EXIT_PREEMPT => StepStop::Preempt,
        KVM_EXIT_IRQ_WINDOW_OPEN => StepStop::Reenter,
        _ => StepStop::GuestExit,
    }
}

/// Map a `KVM_EXIT_IO`. OUT carries the value out (read from the PIO data buffer
/// via the `run_buf` seam); IN arms `Pending::IoIn` for completion.
///
/// **Fails closed on string/REP PIO** (`io.count != 1`): such an exit carries
/// `count * size` bytes, but `Exit::Io` models a single scalar. M1/M2 use only
/// single-byte UART access, so rather than silently drop the extra items, this
/// returns `BackendError`.
fn decode_io(page: RunPage) -> Result<(Exit, Pending)> {
    let (direction, size, port, count, data_offset) = page.io();
    if count != 1 {
        return Err(BackendError::Unsupported {
            what: "string/REP port I/O (io.count != 1)",
        });
    }
    if u32::from(direction) != KVM_EXIT_IO_IN {
        let value = page.read_pio(data_offset, size)?;
        Ok((
            Exit::Io {
                port,
                size,
                write: Some(value),
            },
            Pending::None,
        ))
    } else {
        Ok((
            Exit::Io {
                port,
                size,
                write: None,
            },
            Pending::IoIn { data_offset, size },
        ))
    }
}

/// Map a `KVM_EXIT_MMIO`. A store carries the value out; a load arms
/// `Pending::MmioLoad`.
fn decode_mmio(page: RunPage) -> (Exit, Pending) {
    let (phys_addr, len, is_write, data) = page.mmio();
    let gpa = Gpa(phys_addr);
    let size = len.min(8) as u8;
    if is_write != 0 {
        let mut bytes = [0u8; 8];
        let n = (len as usize).min(8);
        bytes[..n].copy_from_slice(&data[..n]);
        (
            Exit::Mmio {
                gpa,
                size,
                write: Some(u64::from_le_bytes(bytes)),
            },
            Pending::None,
        )
    } else {
        (
            Exit::Mmio {
                gpa,
                size,
                write: None,
            },
            Pending::MmioLoad { len },
        )
    }
}

/// Map a `KVM_EXIT_DETERMINISM` (patched KVM) into the matching instruction-read
/// `Exit` plus the `Determinism` completion it arms. `RDTSC`/`RDTSCP` carry no
/// width to the VMM (the value is always 64-bit EDX:EAX); `RDRAND`/`RDSEED`
/// carry the destination `width` (2/4/8) so vmm-core masks the seeded draw.
fn decode_determinism(page: RunPage) -> Result<(Exit, Pending)> {
    let insn = page.det_insn()?;
    // `width` is bounded by the instruction operand size (≤ 8); the cast is
    // lossless for the conforming `2/4/8` the kernel reports.
    let width = page.det_width()?.min(u32::from(u8::MAX)) as u8;
    match insn {
        KVM_DETERMINISM_RDTSC => Ok((
            Exit::Rdtsc,
            Pending::Determinism {
                rdtscp: false,
                rng: false,
            },
        )),
        KVM_DETERMINISM_RDTSCP => Ok((
            Exit::Rdtscp,
            Pending::Determinism {
                rdtscp: true,
                rng: false,
            },
        )),
        KVM_DETERMINISM_RDRAND => Ok((
            Exit::Rdrand { width },
            Pending::Determinism {
                rdtscp: false,
                rng: true,
            },
        )),
        KVM_DETERMINISM_RDSEED => Ok((
            Exit::Rdseed { width },
            Pending::Determinism {
                rdtscp: false,
                rng: true,
            },
        )),
        _ => Err(BackendError::Internal(
            "unknown KVM_EXIT_DETERMINISM insn kind",
        )),
    }
}

/// Complete a pending `KVM_EXIT_DETERMINISM`: write `value` (→ dest / EDX:EAX),
/// and — per the instruction — `aux` (RDTSCP's `IA32_TSC_AUX` → ECX) and the
/// `CF` success flag (RNG). Pure: the orchestration layer supplies `aux` (it
/// reads the guest's `IA32_TSC_AUX` via `KVM_GET_MSRS`, a syscall that cannot
/// live here). Errors [`BackendError::NoPendingRead`] if no determinism exit is
/// pending.
pub(crate) fn apply_complete_determinism(
    page: RunPage,
    pending: Pending,
    value: u64,
    aux: u64,
) -> Result<()> {
    match pending {
        Pending::Determinism { rdtscp, rng } => {
            page.write_u64_at(DET_VALUE, value)?;
            if rdtscp {
                page.write_u64_at(DET_AUX, aux)?;
            }
            if rng {
                // Deterministic success: a seeded draw always "succeeds" (CF=1).
                page.write_u8_at(DET_FLAGS, KVM_DETERMINISM_FLAG_CF)?;
            }
            Ok(())
        }
        _ => Err(BackendError::NoPendingRead),
    }
}

/// Apply a `complete_read` value to the `kvm_run` for the given pending exit.
/// Pure. Errors `NoPendingRead` if no read-style exit is pending.
pub(crate) fn apply_complete_read(page: RunPage, pending: Pending, value: u64) -> Result<()> {
    match pending {
        Pending::IoIn { data_offset, size } => page.write_pio(data_offset, size, value),
        Pending::MmioLoad { len } => {
            page.write_mmio_data(len, value);
            Ok(())
        }
        Pending::Rdmsr => {
            page.set_msr(value, 0);
            Ok(())
        }
        _ => Err(BackendError::NoPendingRead),
    }
}

/// Apply the `deny-gp` disposition to a pending `Rdmsr`/`Wrmsr` (`error != 0`).
/// Pure. Errors `BadCompletion` if the pending exit is not an MSR exit.
pub(crate) fn apply_complete_fault(page: RunPage, pending: Pending) -> Result<()> {
    match pending {
        Pending::Rdmsr | Pending::Wrmsr => {
            page.set_msr_error(1);
            Ok(())
        }
        _ => Err(BackendError::BadCompletion),
    }
}

/// Resolve a pending `Wrmsr` as allow/drop (`error == 0`). Pure. Errors
/// `BadCompletion` if the pending exit is not a `Wrmsr`.
pub(crate) fn apply_complete_ok(page: RunPage, pending: Pending) -> Result<()> {
    match pending {
        Pending::Wrmsr => {
            page.set_msr_error(0);
            Ok(())
        }
        _ => Err(BackendError::BadCompletion),
    }
}

// ---------------------------------------------------------------------------
// Configuration / snapshot helpers (pure — gated by the unit tests below).
// ---------------------------------------------------------------------------

/// Build the `KVM_SET_CPUID2` entry table from the frozen model. The
/// `SIGNIFCANT_INDEX` flag mapping is the part worth gating.
pub(crate) fn cpuid_entries(model: &CpuidModel) -> Vec<kvm_cpuid_entry2> {
    model
        .entries
        .iter()
        .map(|e| kvm_cpuid_entry2 {
            function: e.leaf,
            index: e.subleaf,
            flags: if e.subleaf_significant {
                KVM_CPUID_FLAG_SIGNIFCANT_INDEX
            } else {
                0
            },
            eax: e.eax,
            ebx: e.ebx,
            ecx: e.ecx,
            edx: e.edx,
            ..Default::default()
        })
        .collect()
}

/// A `KVM_GET/SET_MSRS` ioctl returns the count it actually serviced and stops at
/// the first index it rejects. A short count means the MSR set did not fully
/// transfer — fail closed (an incomplete `allow-stateful` set must not look ok).
pub(crate) fn ensure_full_msr_count(serviced: usize, requested: usize) -> Result<()> {
    if serviced != requested {
        return Err(BackendError::Internal(
            "KVM MSR ioctl serviced a short count (an MSR index was rejected)",
        ));
    }
    Ok(())
}

/// Assemble the saved MSR map from a `KVM_GET_MSRS` result, failing closed on a
/// short count. `entries` is the filled `kvm_msr_entry` slice; `got` the returned
/// count; `requested` the number asked for.
pub(crate) fn saved_msrs(
    entries: &[kvm_msr_entry],
    got: usize,
    requested: usize,
) -> Result<BTreeMap<u32, u64>> {
    ensure_full_msr_count(got, requested)?;
    Ok(entries
        .iter()
        .take(got)
        .map(|e| (e.index, e.data))
        .collect())
}

/// Validate a snapshot's cheap shape against this backend's config *before* any
/// `SET_*` ioctl, so a malformed blob cannot half-mutate the live vCPU:
///
/// - the MSR key set must exactly equal the configured `allow-stateful` indices
///   (a missing key would leave that MSR at a stale value; an extra key names an
///   MSR outside the filter), and
/// - the XSAVE image must be the host's image size (`xsave_len`).
///
/// Either mismatch is [`BackendError::InvalidState`].
pub(crate) fn validate_restore_shape(
    state: &VcpuState,
    filter: Option<&MsrFilter>,
    xsave_len: usize,
) -> Result<()> {
    let mut expected: Vec<u32> = filter
        .map(|f| f.allow_indices().collect())
        .unwrap_or_default();
    expected.sort_unstable();
    expected.dedup();
    // `BTreeMap` keys are already ascending and unique.
    let actual: Vec<u32> = state.msrs.keys().copied().collect();
    if actual != expected {
        return Err(BackendError::InvalidState);
    }
    if state.xsave.len() != xsave_len {
        return Err(BackendError::InvalidState);
    }
    Ok(())
}

/// The honest stock-KVM capabilities: every determinism field `false` (the holes
/// are declared, not laundered — see the crate non-determinism posture).
pub(crate) fn kvm_capabilities() -> Capabilities {
    Capabilities {
        name: "kvm-stock",
        deterministic_tsc: false,
        deterministic_rng: false,
        enforces_tsc_deadline_msr: false,
    }
}

/// The patched-KVM capabilities: RDTSC/RDTSCP and RDRAND/RDSEED are surfaced as
/// exits the VMM resolves against V-time / the seeded entropy stream, so both
/// determinism fields are honestly `true`. `enforces_tsc_deadline_msr` stays
/// `false`: the determinism patch touches only the four instruction intercepts,
/// not the `0x6E0` WRMSR fastpath (the contract hides `IA32_TSC_DEADLINE`
/// instead — INTEGRATION.md §7 / R1, no in-kernel LAPIC).
pub(crate) fn patched_capabilities() -> Capabilities {
    Capabilities {
        name: "kvm-patched",
        deterministic_tsc: true,
        deterministic_rng: true,
        enforces_tsc_deadline_msr: false,
    }
}

// ---------------------------------------------------------------------------
// kvm_bindings <-> VcpuState conversions (flat field copies, no host-derived
// values laundered in).
// ---------------------------------------------------------------------------

pub(crate) fn from_kvm_regs(r: &kvm_regs) -> VcpuRegs {
    VcpuRegs {
        rax: r.rax,
        rbx: r.rbx,
        rcx: r.rcx,
        rdx: r.rdx,
        rsi: r.rsi,
        rdi: r.rdi,
        rsp: r.rsp,
        rbp: r.rbp,
        r8: r.r8,
        r9: r.r9,
        r10: r.r10,
        r11: r.r11,
        r12: r.r12,
        r13: r.r13,
        r14: r.r14,
        r15: r.r15,
        rip: r.rip,
        rflags: r.rflags,
    }
}

pub(crate) fn to_kvm_regs(r: &VcpuRegs) -> kvm_regs {
    kvm_regs {
        rax: r.rax,
        rbx: r.rbx,
        rcx: r.rcx,
        rdx: r.rdx,
        rsi: r.rsi,
        rdi: r.rdi,
        rsp: r.rsp,
        rbp: r.rbp,
        r8: r.r8,
        r9: r.r9,
        r10: r.r10,
        r11: r.r11,
        r12: r.r12,
        r13: r.r13,
        r14: r.r14,
        r15: r.r15,
        rip: r.rip,
        rflags: r.rflags,
    }
}

fn from_kvm_segment(s: &kvm_segment) -> Segment {
    Segment {
        base: s.base,
        limit: s.limit,
        selector: s.selector,
        type_: s.type_,
        present: s.present,
        dpl: s.dpl,
        db: s.db,
        s: s.s,
        l: s.l,
        g: s.g,
        avl: s.avl,
        unusable: s.unusable,
    }
}

fn to_kvm_segment(s: &Segment) -> kvm_segment {
    kvm_segment {
        base: s.base,
        limit: s.limit,
        selector: s.selector,
        type_: s.type_,
        present: s.present,
        dpl: s.dpl,
        db: s.db,
        s: s.s,
        l: s.l,
        g: s.g,
        avl: s.avl,
        unusable: s.unusable,
        padding: 0,
    }
}

fn from_kvm_dtable(d: &kvm_dtable) -> DescriptorTable {
    DescriptorTable {
        base: d.base,
        limit: d.limit,
    }
}

fn to_kvm_dtable(d: &DescriptorTable) -> kvm_dtable {
    kvm_dtable {
        base: d.base,
        limit: d.limit,
        padding: [0; 3],
    }
}

pub(crate) fn from_kvm_sregs2(s: &kvm_sregs2) -> VcpuSregs {
    VcpuSregs {
        cs: from_kvm_segment(&s.cs),
        ds: from_kvm_segment(&s.ds),
        es: from_kvm_segment(&s.es),
        fs: from_kvm_segment(&s.fs),
        gs: from_kvm_segment(&s.gs),
        ss: from_kvm_segment(&s.ss),
        tr: from_kvm_segment(&s.tr),
        ldt: from_kvm_segment(&s.ldt),
        gdt: from_kvm_dtable(&s.gdt),
        idt: from_kvm_dtable(&s.idt),
        cr0: s.cr0,
        cr2: s.cr2,
        cr3: s.cr3,
        cr4: s.cr4,
        cr8: s.cr8,
        efer: s.efer,
        apic_base: s.apic_base,
        // Preserved so `restore(save())` round-trips PAE paging state (the
        // PDPTRS_VALID flag + the four PDPTRs).
        flags: s.flags,
        pdptrs: s.pdptrs,
    }
}

pub(crate) fn to_kvm_sregs2(s: &VcpuSregs) -> kvm_sregs2 {
    kvm_sregs2 {
        cs: to_kvm_segment(&s.cs),
        ds: to_kvm_segment(&s.ds),
        es: to_kvm_segment(&s.es),
        fs: to_kvm_segment(&s.fs),
        gs: to_kvm_segment(&s.gs),
        ss: to_kvm_segment(&s.ss),
        tr: to_kvm_segment(&s.tr),
        ldt: to_kvm_segment(&s.ldt),
        gdt: to_kvm_dtable(&s.gdt),
        idt: to_kvm_dtable(&s.idt),
        cr0: s.cr0,
        cr2: s.cr2,
        cr3: s.cr3,
        cr4: s.cr4,
        cr8: s.cr8,
        efer: s.efer,
        apic_base: s.apic_base,
        flags: s.flags,
        pdptrs: s.pdptrs,
    }
}

pub(crate) fn from_kvm_debugregs(d: &kvm_debugregs) -> DebugRegs {
    DebugRegs {
        db: d.db,
        dr6: d.dr6,
        dr7: d.dr7,
        flags: d.flags,
    }
}

pub(crate) fn to_kvm_debugregs(d: &DebugRegs) -> kvm_debugregs {
    kvm_debugregs {
        db: d.db,
        dr6: d.dr6,
        dr7: d.dr7,
        flags: d.flags,
        ..Default::default()
    }
}

pub(crate) fn from_kvm_events(e: &kvm_vcpu_events) -> VcpuEvents {
    VcpuEvents {
        exception_injected: e.exception.injected,
        exception_nr: e.exception.nr,
        exception_has_error_code: e.exception.has_error_code,
        exception_pending: e.exception.pending,
        exception_error_code: e.exception.error_code,
        exception_has_payload: e.exception_has_payload,
        exception_payload: e.exception_payload,
        interrupt_injected: e.interrupt.injected,
        interrupt_nr: e.interrupt.nr,
        interrupt_soft: e.interrupt.soft,
        interrupt_shadow: e.interrupt.shadow,
        nmi_injected: e.nmi.injected,
        nmi_pending: e.nmi.pending,
        nmi_masked: e.nmi.masked,
        sipi_vector: e.sipi_vector,
        flags: e.flags,
        smi_smm: e.smi.smm,
        smi_pending: e.smi.pending,
        smi_inside_nmi: e.smi.smm_inside_nmi,
        smi_latched_init: e.smi.latched_init,
        triple_fault_pending: e.triple_fault.pending,
    }
}

pub(crate) fn to_kvm_events(e: &VcpuEvents) -> kvm_vcpu_events {
    let mut k = kvm_vcpu_events {
        sipi_vector: e.sipi_vector,
        // `flags` carries the VALID_PAYLOAD / VALID_TRIPLE_FAULT bits, preserved
        // from `save`, so the payload + triple-fault fields below round-trip.
        flags: e.flags,
        exception_has_payload: e.exception_has_payload,
        exception_payload: e.exception_payload,
        ..Default::default()
    };
    k.exception.injected = e.exception_injected;
    k.exception.nr = e.exception_nr;
    k.exception.has_error_code = e.exception_has_error_code;
    k.exception.pending = e.exception_pending;
    k.exception.error_code = e.exception_error_code;
    k.interrupt.injected = e.interrupt_injected;
    k.interrupt.nr = e.interrupt_nr;
    k.interrupt.soft = e.interrupt_soft;
    k.interrupt.shadow = e.interrupt_shadow;
    k.nmi.injected = e.nmi_injected;
    k.nmi.pending = e.nmi_pending;
    k.nmi.masked = e.nmi_masked;
    k.smi.smm = e.smi_smm;
    k.smi.pending = e.smi_pending;
    k.smi.smm_inside_nmi = e.smi_inside_nmi;
    k.smi.latched_init = e.smi_latched_init;
    k.triple_fault.pending = e.triple_fault_pending;
    k
}

/// Map KVM's `mp_state` to our [`MpState`] (anything but `HALTED` is runnable).
pub(crate) fn mp_from_kvm(mp_state: u32) -> MpState {
    if mp_state == KVM_MP_STATE_HALTED {
        MpState::Halted
    } else {
        MpState::Runnable
    }
}

/// Map our [`MpState`] back to KVM's `mp_state`.
pub(crate) fn mp_to_kvm(mp: MpState) -> u32 {
    match mp {
        MpState::Halted => KVM_MP_STATE_HALTED,
        MpState::Runnable => KVM_MP_STATE_RUNNABLE,
    }
}

/// Read `XCR0` (the `xcr == 0` entry) out of a `kvm_xcrs`.
pub(crate) fn xcr0_of(x: &kvm_xcrs) -> u64 {
    x.xcrs
        .iter()
        .take(x.nr_xcrs as usize)
        .find(|e| e.xcr == 0)
        .map_or(0, |e| e.value)
}

/// Build a `kvm_xcrs` carrying a single `XCR0` entry.
pub(crate) fn xcrs_of(xcr0: u64) -> kvm_xcrs {
    let mut x = kvm_xcrs {
        nr_xcrs: 1,
        ..Default::default()
    };
    x.xcrs[0].xcr = 0;
    x.xcrs[0].value = xcr0;
    x
}

/// Serialize the fixed 4 KiB `kvm_xsave` region as bytes.
pub(crate) fn xsave_to_bytes(x: &kvm_xsave) -> Vec<u8> {
    let mut out = Vec::with_capacity(x.region.len() * 4);
    for word in &x.region {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

/// Deserialize a 4 KiB byte image back into a `kvm_xsave`. Rejects a wrong-sized
/// image as `InvalidState` (never a panic).
pub(crate) fn xsave_from_bytes(bytes: &[u8]) -> Result<kvm_xsave> {
    let mut x = kvm_xsave::default();
    if bytes.len() != x.region.len() * 4 {
        return Err(BackendError::InvalidState);
    }
    for (word, chunk) in x.region.iter_mut().zip(bytes.chunks_exact(4)) {
        *word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    Ok(x)
}

#[cfg(test)]
mod tests;
