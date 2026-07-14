// SPDX-License-Identifier: AGPL-3.0-or-later
//! The x86-64 vendor's exit dispatch, contract dispositions, interrupt fabric,
//! and platform device models (`docs/ARCH-BOUNDARY.md` §B's vendor column).
//!
//! Everything here names x86: the port-I/O and MMIO maps, the MSR/CPUID
//! dispositions, the xAPIC + 8259/PIT/PCI legacy platform, the 8250 UART, and
//! the `RFLAGS.IF` interruptibility test. The engine ([`crate::vmm`]) reaches
//! all of it **only** through the [`Vendor`] trait, so it never names an x86
//! device or matches an x86 exit — the arch seam is compiler-enforced.
//!
//! Default-deny is structural here too: [`X86Exit`] is matched **exhaustively**
//! by [`dispatch_arch`](Vendor::dispatch_arch); there is no wildcard arm over
//! arch exits.

use hypercall_proto::Service;
use vmm_backend::{Backend, Gpa, VcpuState, X86, X86Completion};
use vtime::VClockConfig;

use crate::snapshot::SnapshotError;
use crate::vendor::x86::contract::{self, MsrDisposition};
use crate::vendor::x86::devices::{ISA_DEBUG_EXIT_PORT, LegacyPlatform, REPORT_PORT, Uart8250};
use crate::vendor::x86::records::{self, DeviceState, LegacyState, UartState};
use crate::vmm::{Step, TerminalReason, Vmm, VmmError};

/// The x86 per-VM device state ([`Vendor::Devices`]): the 8250 UART (always
/// present — the serial console), plus the userspace xAPIC and the legacy PC
/// platform shims, wired together on the Linux boot path
/// ([`Vmm::wire_lapic`]) and absent for the M1/M2/corpus payloads (which touch
/// neither the APIC page nor the legacy ports, so their `state_hash` carries no
/// device chunks).
pub struct X86Devices {
    /// The 8250 UART (serial console + the task-81 `exec` input queue).
    pub(crate) uart: Uart8250,
    /// The userspace xAPIC (ruling R1) — Linux boot path only.
    pub(crate) lapic: Option<lapic::Lapic>,
    /// Minimal legacy PC platform I/O (PCI/PIC/PIT/CMOS/POST) — wired with the
    /// xAPIC on the Linux boot path.
    pub(crate) legacy: Option<LegacyPlatform>,
}

impl X86Devices {
    /// Fresh (reset) x86 device state: a reset UART, no xAPIC, no legacy
    /// platform (the Linux composition root wires those).
    pub(crate) fn new() -> Self {
        Self {
            uart: Uart8250::new(),
            lapic: None,
            legacy: None,
        }
    }
}

/// xAPIC MMIO base (`0xFEE0_0000`, the architectural default the contract fixes
/// `IA32_APIC_BASE` to — its relocation write is deny-ignore, so the page never
/// moves). The Linux boot path routes loads/stores in `[APIC_MMIO_BASE,
/// APIC_MMIO_BASE + 0x1000)` into the userspace [`lapic::Lapic`].
pub(crate) const APIC_MMIO_BASE: u64 = 0xFEE0_0000;

/// One past the xAPIC MMIO page (`APIC_MMIO_BASE` + one 4 KiB page). A literal (not
/// `BASE + SIZE`) so the page-range check carries no arithmetic mutant.
pub(crate) const APIC_MMIO_END: u64 = 0xFEE0_1000;

/// Legacy ISA IRQ line for COM1 (the modeled 8250 at `0x3F8`). The kernel
/// registers `ttyS0` with this IRQ.
pub(crate) const COM1_IRQ: u8 = 4;

/// The interrupt **vector** the guest delivers COM1's IRQ 4 on. With no IO-APIC
/// and a real 8259, Linux maps the legacy ISA IRQs to a static vector window
/// starting at `ISA_IRQ_VECTOR(0) = 0x30` (the master PIC's ICW2 offset), so
/// `ISA_IRQ_VECTOR(4) = 0x30 + 4 = 0x34`. The VMM injects this vector (via the
/// `KVM_INTERRUPT` legacy-injection seam, exactly as a PIC `INTR`/`ExtINT` would)
/// when the 8250 raises its THRE interrupt; the guest IRQ-4 handler then drains
/// the userspace TX and EOIs the 8259. (Verified against the boot log: the guest
/// uses the 8259 in virtual-wire mode, no IO-APIC — see IMPLEMENTATION.md.)
pub(crate) const COM1_IRQ_VECTOR: u8 = 0x34;

/// `IA32_TSC` — the architectural time-stamp-counter MSR. The contract marks it
/// `emulate-vtime`; a guest `RDMSR(0x10)` reads the same V-time TSC the RDTSC
/// instruction returns, and `WRMSR(0x10)` sets it.
pub(crate) const IA32_TSC: u32 = 0x10;

/// `IA32_TSC_ADJUST` — the architectural per-logical-processor TSC offset MSR.
/// Also `emulate-vtime`; backs [`VtimeWiring::tsc_adjust`].
pub(crate) const IA32_TSC_ADJUST: u32 = 0x3b;

/// The hypercall **doorbell** port (task 73 / INTEGRATION.md §1): an `OUT` here
/// is a cooperating guest SDK ringing a hypercall. Mirrors
/// `vmcall_transport::DOORBELL_PORT` (conventions rule 2 — the guest/host
/// protocol pattern; deliberately distinct from the task-04 report channel at
/// `0x0CA2`). Serviced only when an SDK channel is wired ([`Vmm::enable_sdk`]);
/// otherwise an `OUT 0x0CA1` stays the default-deny contract violation, so every
/// non-SDK path is byte-for-byte unchanged.
pub(crate) const DOORBELL_PORT: u16 = 0x0CA1;

/// `RFLAGS.IF` (interrupt-enable flag, bit 9) — the guest's own signal for "I am
/// waiting for an interrupt I can take". [`Vmm::idle_resume_target`] uses it to
/// tell a *resumable idle* `HLT` (`IF == 1`, an armed timer will wake the guest)
/// from a *terminal* one (`IF == 0` — the kernel's final `cli; hlt`, a wait
/// nothing will satisfy).
pub(crate) const RFLAGS_IF: u64 = 1 << 9;

impl<B: Backend<A = X86>> Vmm<B> {
    pub(crate) fn dispatch_out(
        &mut self,
        port: u16,
        size: u8,
        value: u32,
    ) -> Result<Step, VmmError> {
        if port == ISA_DEBUG_EXIT_PORT {
            require_byte_io("OUT", port, size)?;
            return Ok(self.terminate(TerminalReason::DebugExit { code: value as u8 }));
        }
        if Uart8250::owns(port) {
            require_byte_io("OUT", port, size)?;
            self.devices.uart.write(port, value as u8);
            return Ok(Step::Continued);
        }
        if port == REPORT_PORT {
            // The conformance report channel: a 32-bit `OUT REPORT_PORT, EAX`
            // appends `EAX` to the ordered report stream (corpus box-integration).
            // It is a write (no completion); the value is already deterministic
            // (a V-time TSC / seeded-PRNG word / retired-count the guest computed),
            // and the stream is ordered by execution, so it is a pure function of
            // the run. The 4-byte width is the ABI — a non-dword access is unmodeled
            // and fails closed (never a truncated/extended report value).
            require_dword_io("OUT", port, size)?;
            self.report_stream.push(value);
            return Ok(Step::Continued);
        }
        // Task 73: the hypercall doorbell. Serviced only when a channel is
        // wired (SDK / Net, or the task-110 pvclock offer); otherwise it falls
        // through to the default-deny below (so no channel-less path is
        // affected). One `OUT` = one atomic exchange.
        if port == DOORBELL_PORT
            && (self.sdk.is_some() || self.net.is_some() || self.pvclock_offered())
        {
            require_dword_io("OUT", DOORBELL_PORT, size)?;
            return self.service_doorbell(value);
        }
        // Linux path: the curated legacy ISA/PCI ports accept-and-drop.
        if let Some(legacy) = self.devices.legacy.as_mut()
            && LegacyPlatform::owns(port)
        {
            legacy.write(port, size, value);
            return Ok(Step::Continued);
        }
        Err(VmmError::ContractViolation(format!(
            "unmodeled OUT to port {port:#06x} value {value:#x} (size {size})"
        )))
    }

    pub(crate) fn dispatch_in(&mut self, port: u16, size: u8) -> Result<Step, VmmError> {
        if Uart8250::owns(port) {
            require_byte_io("IN", port, size)?;
            // `read_in` (not `read`): a byte read of the RBR consumes the next
            // injected `exec` input byte (task 81), the way real hardware pops the
            // receive FIFO. Inert on every non-`exec` run (the queue is empty).
            if let Some(byte) = self.devices.uart.read_in(port) {
                self.backend.complete_read(u64::from(byte))?;
                return Ok(Step::Continued);
            }
        }
        // Linux path: the curated legacy ISA/PCI ports read back "no device".
        if let Some(legacy) = self.devices.legacy.as_ref()
            && LegacyPlatform::owns(port)
        {
            let value = legacy.read(port, size);
            self.backend.complete_read(value)?;
            return Ok(Step::Continued);
        }
        Err(VmmError::ContractViolation(format!(
            "unmodeled IN from port {port:#06x} (size {size})"
        )))
    }

    /// Service an MMIO exit. On the Linux path (`lapic` wired) a load/store in the
    /// `0xFEE0_0000` xAPIC page is routed to the userspace [`lapic::Lapic`]; every
    /// other MMIO — and **all** MMIO when the LAPIC is unwired (M1/M2/corpus) —
    /// stays the default-deny [`VmmError::ContractViolation`]. xAPIC registers are
    /// 32-bit; a load completes with the register value, a store updates the
    /// register file (no completion). A bad offset / out-of-page access fails
    /// closed (never a silent value).
    pub(crate) fn dispatch_mmio(
        &mut self,
        gpa: Gpa,
        size: u8,
        write: Option<u64>,
    ) -> Result<Step, VmmError> {
        let in_apic_page =
            self.devices.lapic.is_some() && (APIC_MMIO_BASE..APIC_MMIO_END).contains(&gpa.0);
        if !in_apic_page {
            return Err(VmmError::ContractViolation(format!(
                "unmodeled MMIO at {:#x} (size {size}); only the xAPIC page is modeled, and only on \
                 the Linux boot path",
                gpa.0
            )));
        }
        let now_vns = self.now_vns()?;
        let offset = (gpa.0 - APIC_MMIO_BASE) as u32;
        let lapic = self
            .devices
            .lapic
            .as_mut()
            .expect("in_apic_page implies wired");
        match write {
            None => {
                // xAPIC register load (32-bit). `complete_read` masks to `size`.
                let value = lapic.mmio_read(offset, now_vns).map_err(|e| {
                    VmmError::ContractViolation(format!("xAPIC read {offset:#x}: {e}"))
                })?;
                self.backend.complete_read(u64::from(value))?;
                Ok(Step::Continued)
            }
            Some(v) => {
                // xAPIC register store (32-bit); no completion.
                lapic.mmio_write(offset, v as u32, now_vns).map_err(|e| {
                    VmmError::ContractViolation(format!("xAPIC write {offset:#x}: {e}"))
                })?;
                Ok(Step::Continued)
            }
        }
    }

    /// Raise `vector` into the userspace-LAPIC IRR so the existing IRQ
    /// arbitration delivers it — the [`InjectInterrupt`] apply. Fails loud if the
    /// LAPIC is unwired (there is no arbitration path to assert through), the
    /// vector exceeds the xAPIC's 8-bit identity space (the wire field is u32;
    /// per-arch identities exceed 8 bits, ARCH-BOUNDARY §C), or the vector is
    /// architecturally reserved (`< 16`).
    ///
    /// [`InjectInterrupt`]: environment::HostFault::InjectInterrupt
    pub(crate) fn inject_host_interrupt(&mut self, vector: u32) -> Result<(), VmmError> {
        let Ok(vector) = u8::try_from(vector) else {
            return Err(VmmError::ContractViolation(format!(
                "InjectInterrupt vector {vector:#x} exceeds the xAPIC's 8-bit vector space — \
                 refusing to truncate"
            )));
        };
        let Some(lapic) = self.devices.lapic.as_mut() else {
            return Err(VmmError::ContractViolation(format!(
                "InjectInterrupt vector {vector:#x} but the userspace LAPIC is unwired — no IRQ \
                 arbitration path to assert the vector through (task 59 enforces host interrupts \
                 through the Linux-boot xAPIC)"
            )));
        };
        lapic.raise(vector).map_err(|e| {
            VmmError::ContractViolation(format!(
                "InjectInterrupt vector {vector:#x} rejected: {e:?}"
            ))
        })
    }

    /// Arbitrate and hand the backend the one IRQ vector to inject at the next safe
    /// VM-entry — the V-time LAPIC timer **and** the legacy COM1 serial line — via
    /// [`Backend::set_pending_irq`] (the `KVM_INTERRUPT` / interrupt-window handshake
    /// lives below the trait). Runs once before every entry.
    ///
    /// **LAPIC timer.** Advance the timer to the current [`Self::lapic_now_vns`]
    /// (firing the timer vector into IRR when due, re-arming if periodic), then
    /// **peek** the current highest-priority deliverable vector. Peeking
    /// (not taking) leaves it in the IRR; the IRR→ISR transition happens in
    /// [`Self::complete_irq_delivery`] only once the backend confirms acceptance, so
    /// a snapshot/`state_hash` taken while a vector waits on the interrupt window
    /// shows it pending in IRR, not prematurely in-service.
    ///
    /// **Serial COM1 (IRQ 4).** [`Self::pending_serial_vector`] returns
    /// [`COM1_IRQ_VECTOR`] while the 8250 asserts its THRE interrupt and the 8259
    /// has not masked the line — the legacy ExtINT path (no LAPIC IRR/ISR; the guest
    /// EOIs the 8259). It is **edge-driven by the guest's own `IER` write**, so its
    /// timing is a deterministic function of guest execution.
    ///
    /// **Arbitration.** The backend holds **one** slot, so we re-arbitrate every
    /// entry and pass the higher-priority pending vector. Local-APIC interrupts
    /// outrank the legacy ExtINT line, so a deliverable LAPIC vector wins; the serial
    /// vector is injected only when the LAPIC has nothing pending. Re-arbitrating
    /// every entry means the backend never injects a stale vector (the serial line
    /// de-asserts the moment the kernel drains the TX and clears `IER.THRI`).
    ///
    /// A **no-op when the xAPIC is unwired** (M1/M2/corpus/multiboot never wire the
    /// LAPIC *or* the legacy platform), so those paths call neither `set_pending_irq`
    /// nor `advance_to` — their state and `state_hash` are byte-for-byte unchanged.
    pub(crate) fn service_pending_irqs(&mut self) -> Result<(), VmmError> {
        if self.devices.lapic.is_none() {
            return Ok(());
        }
        let now_vns = self.now_vns()?;
        // Scope the `&mut lapic` borrow so it ends before `self.backend`.
        let lapic_vector = {
            let lapic = self.devices.lapic.as_mut().expect("is_some checked above");
            lapic.advance_to(now_vns);
            lapic.peek_interrupt() // re-arbitrate; do NOT move IRR→ISR
        };
        // Local-APIC interrupts outrank the legacy ExtINT serial line.
        let vector = lapic_vector.or_else(|| self.pending_serial_vector());
        self.backend.set_pending_irq(vector)?;
        Ok(())
    }

    /// The COM1 serial interrupt vector ([`COM1_IRQ_VECTOR`]) if the 8250 is
    /// currently asserting its THRE interrupt (the guest enabled `IER.THRI` and THR
    /// is empty) **and** the 8259 has not masked IRQ 4 — else `None`. Gated on the
    /// legacy platform being wired (the Linux path; it is wired together with the
    /// xAPIC), so M1/M2/corpus never see a serial vector.
    pub(crate) fn pending_serial_vector(&self) -> Option<u8> {
        let legacy = self.devices.legacy.as_ref()?;
        // THRE (transmitter-empty) OR received-data-available (task 81's `exec`
        // input): `serial_irq_asserted` folds both. Equal to `thre_irq_asserted`
        // whenever no `exec` input is queued, so a non-`exec` run is unchanged.
        (self.devices.uart.serial_irq_asserted() && !legacy.irq_masked(COM1_IRQ))
            .then_some(COM1_IRQ_VECTOR)
    }

    /// Complete delivery of every vector the backend **accepted** (issued
    /// `KVM_INTERRUPT` for) during the last `backend.run()`. Called after the entry
    /// and before dispatching the exit, so a guest APIC read / EOI in that exit — and
    /// any snapshot — observes a LAPIC vector in-service exactly once KVM accepted it
    /// (never during the interrupt-window wait).
    ///
    /// An accepted **LAPIC** vector moves IRR→ISR ([`lapic::Lapic::take_interrupt`]).
    /// An accepted **legacy COM1** vector is an ExtINT serviced + EOI'd at the 8259,
    /// not the userspace LAPIC, so it must take **no** IRR/ISR transition — and it
    /// doesn't: [`Self::service_pending_irqs`] only injects the serial vector when the
    /// LAPIC has *nothing* deliverable ([`lapic::Lapic::peek_interrupt`] returned
    /// `None`), and the LAPIC IRR cannot change between that arbitration and here (the
    /// timer fires in `service_pending_irqs`, and any guest LAPIC write is a later
    /// exit), so `take_interrupt` is a no-op exactly when a serial vector was the one
    /// accepted. No-op overall when the xAPIC is unwired (the backend never accepts a
    /// maskable IRQ there).
    pub(crate) fn complete_irq_delivery(&mut self) {
        while self.backend.take_accepted_interrupt().is_some() {
            if let Some(lapic) = self.devices.lapic.as_mut() {
                lapic.take_interrupt();
            }
        }
    }

    pub(crate) fn dispatch_rdmsr(&mut self, index: u32) -> Result<Step, VmmError> {
        let disp = contract::rdmsr_disposition(index);
        loud_msr(
            MsrDir::Read,
            index,
            None,
            self.guest_rip(),
            self.current_work(),
            &disp,
        );
        match disp {
            MsrDisposition::AllowFixed(v) => {
                self.backend.complete_read(v)?;
                Ok(Step::Continued)
            }
            MsrDisposition::DenyGp => {
                self.backend.complete_fault()?;
                Ok(Step::Continued)
            }
            MsrDisposition::EmulateVtime => self.rdmsr_vtime(index),
            // allow-stateful is in-kernel and should never surface; a read-side
            // deny-ignore-write does not exist in the contract.
            MsrDisposition::AllowStateful | MsrDisposition::DenyIgnoreWrite => {
                Err(VmmError::ContractViolation(format!(
                    "RDMSR {index:#x} surfaced with a non-userspace disposition {disp:?}"
                )))
            }
        }
    }

    pub(crate) fn dispatch_wrmsr(&mut self, index: u32, value: u64) -> Result<Step, VmmError> {
        let disp = contract::wrmsr_disposition(index, value);
        loud_msr(
            MsrDir::Write,
            index,
            Some(value),
            self.guest_rip(),
            self.current_work(),
            &disp,
        );
        match disp {
            MsrDisposition::DenyIgnoreWrite => {
                // Drop the write (already logged), then resume.
                self.backend.complete_ok()?;
                Ok(Step::Continued)
            }
            // A write to a read-only allow-fixed row, or any deny-gp row, faults.
            MsrDisposition::DenyGp | MsrDisposition::AllowFixed(_) => {
                self.backend.complete_fault()?;
                Ok(Step::Continued)
            }
            MsrDisposition::EmulateVtime => self.wrmsr_vtime(index, value),
            MsrDisposition::AllowStateful => Err(VmmError::ContractViolation(format!(
                "WRMSR {index:#x} surfaced but is allow-stateful (should be in-kernel)"
            ))),
        }
    }

    pub(crate) fn dispatch_cpuid(&mut self, leaf: u32, subleaf: u32) -> Result<Step, VmmError> {
        // Stock KVM answers CPUID in-kernel and never reaches here; a backend that
        // surfaces it gets the frozen model overlaid with the live dynamic cells.
        let state = self.backend.save()?;
        let base = lookup_cpuid(leaf, subleaf);
        let resolved = contract::resolve_cpuid(base, state.sregs.cr4, state.xcr0);
        self.backend.complete_arch(X86Completion::Cpuid {
            eax: resolved.eax,
            ebx: resolved.ebx,
            ecx: resolved.ecx,
            edx: resolved.edx,
        })?;
        Ok(Step::Continued)
    }

    /// Complete a pending `RDTSC`/`RDTSCP` with the **V-time** TSC,
    /// [`VtimeWiring::guest_clock`] (`VClock::guest_ticks(work)` + `IA32_TSC_ADJUST`) — never
    /// a host TSC, and identical to what `RDMSR(IA32_TSC)` returns. `work` is read
    /// from the host counter at this exit; the backend writes the value to EDX:EAX
    /// (and, for RDTSCP, the guest's `IA32_TSC_AUX` to ECX, which the backend
    /// supplies from guest state). Fails closed if V-time is unwired (stock KVM /
    /// M1/M2 never surface these exits, so reaching here without wiring is a contract
    /// bug).
    pub(crate) fn complete_tsc(&mut self) -> Result<Step, VmmError> {
        let tsc = {
            let Some(vt) = self.vtime.as_mut() else {
                return Err(VmmError::ContractViolation(
                    "RDTSC/RDTSCP surfaced but V-time is not wired (stock backend?) — refusing to \
                     supply a host TSC"
                        .to_string(),
                ));
            };
            let work = vt.work.work()?;
            // This is a V-time intercept (a synchronized point): record its
            // *deterministic* work so the `VTIM` hash anchors here, not to a
            // skid-laden live read at hash time (task-27 item 2).
            vt.last_intercept_work = work;
            vt.guest_clock(work)
        };
        self.backend.complete_read(tsc)?;
        // A V-time intercept: `last_intercept_work` is now the exact current work, so
        // a snapshot here would be exact (see `save_vtime`).
        self.vtime_synchronized = true;
        Ok(Step::Continued)
    }

    /// Service an `emulate-vtime` `RDMSR` (`IA32_TSC` 0x10 → the guest-visible
    /// V-time TSC, the **same** value the RDTSC instruction returns; `IA32_TSC_ADJUST`
    /// 0x3b → the stored adjust). Fails closed if V-time is unwired (stock KVM /
    /// M1/M2 never surface these), or if an unexpected index is routed here. Both are
    /// V-time MSR intercepts, so each records its deterministic work as the hash
    /// anchor (like [`complete_tsc`](Self::complete_tsc)).
    pub(crate) fn rdmsr_vtime(&mut self, index: u32) -> Result<Step, VmmError> {
        let value = {
            let Some(vt) = self.vtime.as_mut() else {
                return Err(VmmError::ContractViolation(format!(
                    "emulate-vtime RDMSR {index:#x} surfaced but V-time is not wired (stock \
                     backend?) — refusing to supply a host value"
                )));
            };
            match index {
                IA32_TSC => {
                    let work = vt.work.work()?;
                    vt.last_intercept_work = work;
                    vt.guest_clock(work)
                }
                IA32_TSC_ADJUST => {
                    // A TSC_ADJUST access is a V-time MSR intercept too: sample its
                    // deterministic work so the hashed effective V-time stays current
                    // (the returned value — the adjust — does not depend on work).
                    let work = vt.work.work()?;
                    vt.last_intercept_work = work;
                    vt.guest_clock_offset
                }
                other => {
                    return Err(VmmError::ContractViolation(format!(
                        "emulate-vtime RDMSR {other:#x} is not a V-time MSR (only IA32_TSC 0x10 and \
                         IA32_TSC_ADJUST 0x3b are emulate-vtime)"
                    )));
                }
            }
        };
        self.backend.complete_read(value)?;
        // A V-time MSR intercept: `last_intercept_work` is the exact current work.
        self.vtime_synchronized = true;
        Ok(Step::Continued)
    }

    /// Service an `emulate-vtime` `WRMSR`. `WRMSR(IA32_TSC, X)` sets the guest-visible
    /// TSC to `X` by choosing the adjust `X − VClock::guest_ticks(work)` (architecturally a
    /// TSC write also moves `IA32_TSC_ADJUST` by the same delta — this is exactly
    /// that); `WRMSR(IA32_TSC_ADJUST, Y)` sets the adjust to `Y`, shifting the visible
    /// TSC by `Y − old`. Both are honored (`complete_ok`); the write is deterministic
    /// (guest-driven at a deterministic work point) and folds into the hashed
    /// `tsc_adjust`. Fails closed if V-time is unwired or the index is unexpected.
    pub(crate) fn wrmsr_vtime(&mut self, index: u32, value: u64) -> Result<Step, VmmError> {
        {
            let Some(vt) = self.vtime.as_mut() else {
                return Err(VmmError::ContractViolation(format!(
                    "emulate-vtime WRMSR {index:#x} surfaced but V-time is not wired (stock \
                     backend?) — refusing to emulate"
                )));
            };
            match index {
                IA32_TSC => {
                    let work = vt.work.work()?;
                    vt.last_intercept_work = work;
                    // guest_clock(work) == value ⇔ adjust = value − VClock::guest_ticks(work).
                    vt.guest_clock_offset = value.wrapping_sub(vt.clock.guest_ticks(work));
                }
                IA32_TSC_ADJUST => {
                    // V-time MSR intercept — sample work to keep the hashed effective
                    // V-time current (see the RDMSR side).
                    let work = vt.work.work()?;
                    vt.last_intercept_work = work;
                    vt.guest_clock_offset = value;
                }
                other => {
                    return Err(VmmError::ContractViolation(format!(
                        "emulate-vtime WRMSR {other:#x} is not a V-time MSR (only IA32_TSC 0x10 and \
                         IA32_TSC_ADJUST 0x3b are emulate-vtime)"
                    )));
                }
            }
        }
        self.backend.complete_ok()?;
        // A V-time MSR intercept: `last_intercept_work` is the exact current work.
        self.vtime_synchronized = true;
        Ok(Step::Continued)
    }

    /// Complete a pending `RDRAND`/`RDSEED` with `width` bytes from the **seeded**
    /// entropy stream (the same one the `Entropy` hypercall uses) — never the host
    /// RNG. The backend masks to `width` and sets CF (deterministic success).
    /// Fails closed if V-time/RNG is unwired.
    ///
    /// An RNG exit is a **V-time intercept** (one of the four determinism-cap traps),
    /// so it records its deterministic work as the hash anchor — exactly like
    /// [`complete_tsc`](Self::complete_tsc) and the TSC-MSR paths. Without this, if an
    /// RNG exit were the last intercept before a checkpoint, the `VTIM` hash would use
    /// a stale (prior-intercept) work value, so two states that burned different
    /// branch counts before the same seeded draw would collide — a false determinism
    /// MATCH that then diverges on the next TSC read.
    pub(crate) fn complete_rng(&mut self, width: u8) -> Result<Step, VmmError> {
        let value = {
            let Some(vt) = self.vtime.as_mut() else {
                return Err(VmmError::ContractViolation(
                    "RDRAND/RDSEED surfaced but the seeded entropy stream is not wired (stock \
                     backend?) — refusing to supply host RNG"
                        .to_string(),
                ));
            };
            // Record the synchronized work at this RNG intercept (the draw itself
            // retires no guest branches, so the order vs `draw_rng` is irrelevant).
            let work = vt.work.work()?;
            vt.last_intercept_work = work;
            vt.draw_rng(width)?
        };
        self.backend.complete_read(value)?;
        // A V-time intercept: the V-time is exact (`last_intercept_work` is current).
        // Independently, the seeded draw advanced the stream but `complete_read` only
        // STAGES the reg-write/RIP-advance for the next `KVM_RUN`, so this is an unsafe
        // *entropy* snapshot boundary until the next `step` re-enters and commits it
        // (see `save_vtime`, which fails on the RNG flag even though V-time is exact).
        self.vtime_synchronized = true;
        self.rng_completion_staged = true;
        Ok(Step::Continued)
    }

    /// Best-effort guest RIP at the faulting instruction, for the loud §1 MSR
    /// log. Logging must never abort the run, so a `save()` failure degrades to
    /// `0` rather than propagating — the architectural effect (the completion) is
    /// still serviced afterward, where its own error path applies.
    pub(crate) fn guest_rip(&self) -> u64 {
        self.backend.save().map(|s| s.regs.rip).unwrap_or_default()
    }

    /// Wire the userspace xAPIC **and** the minimal legacy PC platform I/O for the
    /// Linux boot path: after this, a guest load/store in the `0xFEE0_0000` MMIO
    /// page is serviced by `lapic`, and the curated legacy ISA/PCI ports return
    /// "no device" instead of failing closed. M1/M2/corpus leave both unwired (they
    /// never touch the page or those ports), keeping their `state_hash` unchanged.
    pub fn wire_lapic(&mut self, lapic: lapic::Lapic) -> &mut Self {
        self.devices.lapic = Some(lapic);
        self.devices.legacy = Some(LegacyPlatform::new());
        self
    }

    /// `true` once the userspace xAPIC is wired (the Linux boot path).
    pub fn lapic_wired(&self) -> bool {
        self.devices.lapic.is_some()
    }

    /// `true` iff a **genuine guest interrupt is pending delivery but not yet accepted** —
    /// a real vector raised into the LAPIC IRR and re-arbitrated as deliverable (e.g. the
    /// periodic V-time LAPIC timer), or the legacy COM1 ExtINT line asserting — held in the
    /// inject seam awaiting the next safe VM-entry.
    ///
    /// This is the **architecturally in-flight event** that the determinism overlay makes
    /// observable at a *synchronized* (snapshottable) boundary. Unlike a `kvm_vcpu_events`
    /// `interrupt_injected` bit — which exists only at a non-synchronized interrupt-window
    /// exit, where [`Vmm::save_vm_state`] fails closed — a vector pending in the IRR sits in
    /// the captured LAPIC state (device blob) and is **re-derived exactly** on restore (the
    /// IRR→ISR acceptance transition models a hypervisor-side event, so vmm-core leaves the
    /// vector in IRR until acceptance — see [`lapic::Lapic::peek_interrupt`] and
    /// `snapshot_restore_re_derives_the_in_flight_lapic_irq`). It is **distinct from an
    /// inert `kvm_vcpu_events` modifier residual** (a stale post-delivery `interrupt.nr`):
    /// this is a committed, *undelivered* interrupt. The live gate seals on this (or on
    /// [`Vmm::has_active_event_injection`]) to prove restore of a true in-flight event.
    ///
    /// Re-arbitrates (`advance_to(now)`, idempotent with the run loop's per-step service)
    /// and peeks **without** moving IRR→ISR, so it does not perturb the snapshot. Returns
    /// `false` when no LAPIC is wired (M1/M2/corpus) and no serial line is asserting.
    pub(crate) fn has_pending_guest_interrupt_x86(&mut self) -> Result<bool, VmmError> {
        if self.devices.lapic.is_none() {
            return Ok(self.pending_serial_vector().is_some());
        }
        let now = self.now_vns()?;
        // Scope the `&mut lapic` borrow so it ends before `self.pending_serial_vector()`.
        let lapic_pending = {
            let lapic = self.devices.lapic.as_mut().expect("is_some checked above");
            lapic.advance_to(now);
            lapic.peek_interrupt().is_some()
        };
        Ok(lapic_pending || self.pending_serial_vector().is_some())
    }

    /// Build the canonical [`vm_state::VmState`] from `vcpu` + the **current** live
    /// machine (the memory-less half of a snapshot): the supplied vCPU registers, the
    /// V-time block + entropy stream, and a vmm-core-owned device blob carrying the
    /// xAPIC, the legacy 8259/PCI latches, the 8250 UART, the ordered report stream,
    /// and `IA32_TSC_ADJUST`. The `contract_hash` is stamped so a restore can reject a
    /// blob taken under a different contract. The caller supplies `vcpu` (so the
    /// fallible `Backend::save` is resolved where the error can propagate —
    /// [`Vmm::save_vm_state`] — rather than swallowed). Infallible; the V-time block is
    /// anchored to the deterministic `last_intercept_work`, exactly like
    /// [`encode_vtime`], so it is byte-deterministic at any exit.
    pub(crate) fn build_vm_state(&self, vcpu: &VcpuState) -> vm_state::VmState {
        let mut s = vm_state::VmState::default();
        records::fill_vcpu_state(&mut s, vcpu);
        let tsc_adjust = match &self.vtime {
            Some(vt) => {
                s.vtime = vm_state::VtimeState {
                    ratio_num: vt.cfg.ratio_num,
                    // `VtimeWiring::new` enforces `ratio_den == 1`; carry it so the
                    // blob is encodable (a fractional ratio is rejected at encode).
                    ratio_den: 1,
                    guest_hz: vt.cfg.guest_hz,
                    guest_base: vt.cfg.guest_base,
                    snapshot_vns: vt.clock.snapshot_vns(vt.last_intercept_work),
                };
                // The entropy PRNG position rides the `hypercall` section
                // (INTEGRATION.md §4: `Dispatcher::save_state()`, "notably the
                // entropy PRNG position") — vmm-core's hypercall RNG and RDRAND draw
                // from this one stream.
                s.hypercall = vt.entropy.save_state();
                vt.guest_clock_offset
            }
            None => {
                // Unwired (M1/M2): a sentinel encodable V-time block, no entropy.
                s.vtime.ratio_den = 1;
                0
            }
        };
        let dev = DeviceState {
            tsc_adjust,
            // The ordered conformance report stream is guest-observable output (it
            // feeds `observable_digest` / the O2 oracle), captured here so a restore
            // resumes it — else a branch taken after `REPORT_PORT` writes would lose
            // them and its `observable_digest` would diverge from the reference. It is
            // NOT in the default `state_hash` (O1): that path never emits a `VMST`
            // chunk (snapshot-hashing is opt-in), so O1/O2 stay separate.
            report_stream: self.report_stream.clone(),
            uart: UartState {
                capture: self.devices.uart.capture().to_vec(),
                regs: *self.devices.uart.shadow_regs(),
                dlab: self.devices.uart.dlab(),
                dlm: self.devices.uart.dlm(),
            },
            lapic: self.devices.lapic.as_ref().map(|l| l.snapshot()),
            legacy: self.devices.legacy.as_ref().map(|l| {
                let imr = l.pic_imr();
                LegacyState {
                    config_address: l.config_address(),
                    master_imr: imr[0],
                    slave_imr: imr[1],
                }
            }),
            // The full `kvm_vcpu_events` (task 41), **canonicalized** so an in-flight
            // interrupt/exception injection round-trips while KVM's inert modifier
            // residuals (a stale `interrupt.nr`/`exception.nr`, the GET-only validity
            // bits) collapse to the clean record — replaying those raw into
            // `KVM_SET_VCPU_EVENTS` corrupts the resumed guest. All-zero at a quiescent
            // point, so M1/M2/corpus blobs are unchanged.
            events: records::canonical_events(&vcpu.events),
            // The task-110 pvclock channel (v4): offer + Δ + the one-shot
            // registration, so the direct restore path carries the stamping
            // obligation with the state it governs (same-state ⇒ same-future).
            pvclock: self
                .pvclock_snapshot()
                .map(|s| (s.delta_work, s.gpa, s.registrable)),
        };
        s.devices = records::encode_device_blob(&dev);
        s.contract_hash = contract::contract_hash();
        s
    }

    /// The x86 half of a `vm_state` restore, **validating without mutating**: the
    /// contract hash, the device blob, the `kvm_vcpu_events` restorability, and the
    /// xAPIC / legacy-platform wiring coherence. Yields the decoded vCPU record set
    /// (with the restore-canonicalized events already applied), the guest
    /// clock-offset register (`IA32_TSC_ADJUST`) the engine re-applies with its
    /// V-time commit, and the prepared devices for
    /// [`commit_restore_x86`](Vmm::commit_restore_x86).
    pub(crate) fn validate_restore_x86(
        &self,
        s: &vm_state::VmState,
    ) -> Result<(VcpuState, u64, X86RestorePrep), VmmError> {
        // Contract: a blob taken under a different CPUID/MSR contract would silently
        // diverge on restore (INTEGRATION.md §4 `contract_hash`).
        if s.contract_hash != contract::contract_hash() {
            return Err(VmmError::Snapshot(SnapshotError::ContractMismatch));
        }
        // Decode the vmm-core device blob (total, never panics).
        let dev = records::decode_device_blob(&s.devices.0)?;
        // Reject an UNRESTORABLE `kvm_vcpu_events` blob up front — a foreign /
        // malformed v3 blob that sets a cap-disabled validity bit
        // (`VALID_TRIPLE_FAULT`/`VALID_PAYLOAD`) would make `KVM_SET_VCPU_EVENTS`
        // return `-EINVAL` only AFTER earlier `KVM_SET_*` ioctls inside
        // `Backend::restore` already mutated the target vCPU. Validate here, while
        // committing nothing, to preserve restore's reject-before-mutation (atomic)
        // contract — symmetric with the `save_vm_state` guard (PR #12 round 8).
        if let Some(reason) = records::cap_unrestorable_events(&dev.events) {
            return Err(VmmError::ContractViolation(format!(
                "restore_vm_state: {reason}"
            )));
        }
        // The blob's LAPIC must be coherent AND match this VM's wiring.
        let new_lapic = match (&dev.lapic, self.devices.lapic.is_some()) {
            (Some(ls), true) => Some(lapic::Lapic::restore(ls).map_err(|_| {
                SnapshotError::DeviceRestore("incoherent LapicState in device blob")
            })?),
            (Some(_), false) | (None, true) => {
                return Err(VmmError::ContractViolation(
                    "restore_vm_state: snapshot/VM xAPIC wiring mismatch (one has a LAPIC, the \
                     other does not) — restore into a VM composed like the snapshot source."
                        .to_string(),
                ));
            }
            (None, false) => None,
        };
        // The legacy platform must match this VM's wiring too — a blob whose legacy
        // subrecord is absent (or present) where the VM's is not is a malformed
        // snapshot, **rejected** rather than silently skipped (which would leave the
        // 8259 IMRs / PCI latch stale). (LAPIC + legacy are wired together by
        // `wire_lapic`, so a well-formed blob always agrees; this fails closed on one
        // that does not.)
        if dev.legacy.is_some() != self.devices.legacy.is_some() {
            return Err(VmmError::ContractViolation(
                "restore_vm_state: snapshot/VM legacy-platform wiring mismatch (one has the \
                 8259/PCI latches, the other does not) — restore into a VM composed like the \
                 snapshot source."
                    .to_string(),
            ));
        }
        // The task-110 pvclock channel record must validate symmetrically
        // against this VM's composition (offer/Δ/GPA/deterministic backend) —
        // still committing nothing (reject-before-mutation).
        self.pvclock_validate_restore(dev.pvclock.as_ref())?;
        // Build the vCPU state (pure). The typed records yield the reduced `vm_state`
        // event subset; overwrite `events` with the device blob's **full**
        // `kvm_vcpu_events` (task 41) so an in-flight interrupt/exception injection is
        // re-established exactly (`KVM_SET_VCPU_EVENTS`), not silently zeroed — the
        // device-blob record is a strict superset of the typed one and is
        // authoritative. (The inject-seam `set_pending_irq` slot is NOT serialized: it
        // is re-derived from the restored LAPIC IRR / UART THRE on the restored VM's
        // first `service_pending_irqs`, so there is no separate plan to carry.)
        let mut vcpu = records::vcpu_state_from(s);
        // Canonicalize on restore too — mirror the save side (`build_vm_state`, which
        // stores `canonical_events` in the device blob). This VM's own save path
        // already strips KVM's inert modifier residuals, but an **external or older
        // v3 blob** (hand-built, or from a different/buggy encoder) may carry RAW
        // residuals; forwarding them verbatim to `KVM_SET_VCPU_EVENTS` would
        // reintroduce the exact corruption canonicalization exists to prevent. Use
        // `events_for_restore` (not `canonical_events`): it additionally forces the
        // cap-free NMI_PENDING/SHADOW/SMM validity bits ON, so KVM **clears** any
        // stale NMI-pending / interrupt-shadow / SMM left on a NON-fresh target vCPU
        // (a clear bit means "leave unchanged" to `KVM_SET_VCPU_EVENTS`) — restore is
        // then independent of the prior occupant (the branch / restore-in-place case;
        // PR #12 round 6). The cap-gated TRIPLE_FAULT/PAYLOAD were already rejected
        // above. Idempotent for a self-produced blob; the `state_hash` still uses
        // `canonical_events`.
        vcpu.events = records::events_for_restore(&dev.events);
        let clock_offset = dev.tsc_adjust;
        Ok((
            vcpu,
            clock_offset,
            X86RestorePrep {
                lapic: new_lapic,
                dev,
            },
        ))
    }

    /// The x86 half of the restore **commit** (all infallible): install the prepared
    /// xAPIC, the legacy-platform latches, the UART residual state, the restored
    /// guest-observable report stream, and the task-110 pvclock channel state
    /// (the sealed registration resumes stamping into the restored RAM's page;
    /// a sealed-unregistered record clears any stale-timeline registration).
    pub(crate) fn commit_restore_x86(&mut self, prep: X86RestorePrep) {
        let X86RestorePrep { lapic, dev } = prep;
        if let Some(l) = lapic {
            self.devices.lapic = Some(l);
        }
        if let (Some(legacy), Some(ls)) = (self.devices.legacy.as_mut(), dev.legacy) {
            legacy.restore(ls.config_address, ls.master_imr, ls.slave_imr);
        }
        self.devices
            .uart
            .restore(dev.uart.capture, dev.uart.regs, dev.uart.dlab, dev.uart.dlm);
        self.pvclock_commit_restore(dev.pvclock.as_ref());
        self.report_stream = dev.report_stream;
    }
}

/// A modeled byte port (the 8250 UART block and isa-debug-exit) is
/// **byte-addressed**; a wider access (`size != 1`) is unmodeled by the M1/M2
/// payloads and must **fail closed** (CPU-MSR-CONTRACT default-deny), never a
/// silent `value as u8` truncation — an `outl $x, $0xF4` must not become a fake
/// debug-exit `PASS`, and a wide UART write must not drop its high bytes.
fn require_byte_io(dir: &str, port: u16, size: u8) -> Result<(), VmmError> {
    if size != 1 {
        return Err(VmmError::ContractViolation(format!(
            "{dir} to modeled byte port {port:#06x} with size {size} != 1 — the 8250 UART and \
             isa-debug-exit are byte-addressed; a wider access is unmodeled (fail closed, not a \
             truncation)"
        )));
    }
    Ok(())
}

/// The report channel ([`REPORT_PORT`]) is **dword-addressed** (`OUT …, EAX`):
/// a non-32-bit access is unmodeled and must **fail closed** (default-deny),
/// never a silent truncation/extension of a reported value — a reported value
/// rides exactly one 4-byte write, and `report(u64)` is two of them.
fn require_dword_io(dir: &str, port: u16, size: u8) -> Result<(), VmmError> {
    if size != 4 {
        return Err(VmmError::ContractViolation(format!(
            "{dir} to report port {port:#06x} with size {size} != 4 — the report channel is \
             dword-addressed (a reported value is one 32-bit OUT); a different width is unmodeled \
             (fail closed)"
        )));
    }
    Ok(())
}

/// Loud host-side log of an MSR access, emitted **before** any architectural
/// effect and never perturbing guest-visible state (CPU-MSR-CONTRACT §1
/// loud-event policy). §1 mandates the full context: access direction, the KVM
/// exit reason, the MSR index, the WRMSR data (`n/a` on a read), the guest RIP at
/// the faulting instruction, the current work counter / V-time, and the
/// disposition applied. `work` is the retired-branch counter at this exit
/// (task-21 P3): `Some(n)` on the determinism-complete path, `None` →
/// `work=unwired` when V-time is not wired (stock KVM / M1/M2) — logged honestly
/// rather than a fake `0` that would read as a real count.
fn loud_msr(
    dir: MsrDir,
    index: u32,
    data: Option<u64>,
    rip: u64,
    work: Option<u64>,
    disp: &MsrDisposition,
) {
    let data = match data {
        Some(v) => format!("{v:#x}"),
        None => "n/a".to_string(),
    };
    let work = match work {
        Some(n) => n.to_string(),
        None => "unwired".to_string(),
    };
    eprintln!(
        "[vmm-core] msr-exit dir={} exit-reason={} index={index:#x} data={data} rip={rip:#x} \
         work={work} disposition={disp:?}",
        dir.dir(),
        dir.exit_reason(),
    );
}

/// Look up the frozen CPUID entry for `(leaf, subleaf)`: an exact `(leaf,
/// subleaf)` match, else a `leaf`-only (insignificant-subleaf) match, else a
/// zeroed entry (the `cpuid-default zeroed` rule).
pub(crate) fn lookup_cpuid(leaf: u32, subleaf: u32) -> vmm_backend::CpuidEntry {
    let model = contract::cpuid_model();
    let mut leaf_only = None;
    for e in &model.entries {
        if e.leaf == leaf {
            if e.subleaf == subleaf {
                return *e;
            }
            if !e.subleaf_significant {
                leaf_only = Some(*e);
            }
        }
    }
    leaf_only.unwrap_or(vmm_backend::CpuidEntry {
        leaf,
        subleaf,
        ..Default::default()
    })
}

/// Deterministic, fixed-layout encoding of an xAPIC [`lapic::LapicState`] for the
/// `LAPC` hash chunk: every field little-endian in declaration order (all plain
/// `u32`/`u64`/`[u32; N]`/`bool` POD — no map iteration, no float). A change in any
/// register, the timer bookkeeping, or the armed/pending flags changes the hash.
pub(crate) fn encode_lapic_state(s: &lapic::LapicState) -> Vec<u8> {
    let mut v = Vec::new();
    for x in [s.version, s.id] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.extend_from_slice(&s.timer_hz.to_le_bytes());
    for x in [
        s.tpr,
        s.svr,
        s.ldr,
        s.dfr,
        s.esr,
        s.icr_low,
        s.icr_high,
        s.divide_config,
    ] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    for word in s.isr.iter().chain(&s.tmr).chain(&s.irr).chain(&s.lvt) {
        v.extend_from_slice(&word.to_le_bytes());
    }
    v.extend_from_slice(&s.initial_count.to_le_bytes());
    v.extend_from_slice(&s.count_at_arm.to_le_bytes());
    v.extend_from_slice(&s.timer_arm_vns.to_le_bytes());
    v.push(u8::from(s.timer_running));
    v.push(u8::from(s.timer_pending));
    v
}

/// Deterministic, fixed-layout encoding of a `VcpuState` (no map iteration into
/// bytes beyond the already-sorted `BTreeMap`; no float; no host clock).
pub(crate) fn encode_vcpu_state(s: &VcpuState) -> Vec<u8> {
    let mut v = Vec::new();
    let r = &s.regs;
    for x in [
        r.rax, r.rbx, r.rcx, r.rdx, r.rsi, r.rdi, r.rsp, r.rbp, r.r8, r.r9, r.r10, r.r11, r.r12,
        r.r13, r.r14, r.r15, r.rip, r.rflags,
    ] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    for seg in [
        &s.sregs.cs,
        &s.sregs.ds,
        &s.sregs.es,
        &s.sregs.fs,
        &s.sregs.gs,
        &s.sregs.ss,
        &s.sregs.tr,
        &s.sregs.ldt,
    ] {
        encode_segment(&mut v, seg);
    }
    for dt in [&s.sregs.gdt, &s.sregs.idt] {
        v.extend_from_slice(&dt.base.to_le_bytes());
        v.extend_from_slice(&dt.limit.to_le_bytes());
    }
    for cr in [
        s.sregs.cr0,
        s.sregs.cr2,
        s.sregs.cr3,
        s.sregs.cr4,
        s.sregs.cr8,
        s.sregs.efer,
        s.sregs.apic_base,
        s.sregs.flags,
    ] {
        v.extend_from_slice(&cr.to_le_bytes());
    }
    for p in s.sregs.pdptrs {
        v.extend_from_slice(&p.to_le_bytes());
    }
    v.extend_from_slice(&s.xcr0.to_le_bytes());
    for d in s.debugregs.db {
        v.extend_from_slice(&d.to_le_bytes());
    }
    v.extend_from_slice(&s.debugregs.dr6.to_le_bytes());
    v.extend_from_slice(&s.debugregs.dr7.to_le_bytes());
    v.extend_from_slice(&s.debugregs.flags.to_le_bytes());
    encode_events(&mut v, &s.events);
    v.push(match s.mp_state {
        vmm_backend::MpState::Runnable => 0,
        vmm_backend::MpState::Halted => 1,
    });
    // MSRs: BTreeMap iterates in ascending key order (deterministic).
    v.extend_from_slice(&(s.msrs.len() as u64).to_le_bytes());
    for (idx, val) in &s.msrs {
        v.extend_from_slice(&idx.to_le_bytes());
        v.extend_from_slice(&val.to_le_bytes());
    }
    v.extend_from_slice(&(s.xsave.len() as u64).to_le_bytes());
    v.extend_from_slice(&s.xsave);
    v
}

fn encode_segment(v: &mut Vec<u8>, seg: &vmm_backend::Segment) {
    v.extend_from_slice(&seg.base.to_le_bytes());
    v.extend_from_slice(&seg.limit.to_le_bytes());
    v.extend_from_slice(&seg.selector.to_le_bytes());
    // An **unusable** segment's `type` (and the rest of its access-rights byte) is
    // architecturally **don't-care**: the CPU never consults the descriptor cache of a
    // segment whose unusable bit is set (SDM Vol. 3 §24.4.1 — the VMX "unusable"
    // attribute means the segment is treated as absent; the hidden type/attr bits are
    // ignored on every use). KVM **normalizes** it (a `KVM_GET` of an unusable segment
    // reports `type = 0`, but after `KVM_SET_SREGS` a `KVM_GET` reports `type = 1`), so
    // a snapshot/restore round-trip otherwise perturbs this don't-care field and breaks
    // restore-transparency on `state_hash`. Canonicalize it to `0` so the hash reflects
    // only architecturally-meaningful state. Golden-safe: every live-`KVM_GET` value
    // already reports `type = 0` for unusable segments, so no existing (relative) golden
    // moves; the only effect is making a restored unusable segment hash like a live one.
    let type_ = if seg.unusable != 0 { 0 } else { seg.type_ };
    v.extend_from_slice(&[
        type_,
        seg.present,
        seg.dpl,
        seg.db,
        seg.s,
        seg.l,
        seg.g,
        seg.avl,
        seg.unusable,
    ]);
}

/// Encode the pending-event state into the `state_hash` in **canonical** form
/// ([`records::canonical_events`]): an inert `kvm_vcpu_events` modifier residual KVM
/// leaves set when its active bit is clear (a stale `interrupt.nr`/`exception.nr`, the
/// GET-only validity `flags` bits) has **no architectural effect** — the VM-entry
/// interruption-information / exception fields are consumed only when their valid bit is
/// set (SDM Vol. 3 §24.8.3, §26.5). Hashing the canonical form makes a restored VM
/// (whose events were canonicalized at restore for soundness — see
/// [`records::canonical_events`]) hash **identically** to a never-restored VM at the
/// same point, so restore-transparency holds on the full `state_hash`. Determinism is
/// unaffected (canonical is a pure function; two same-seed runs share identical raw
/// events ⇒ identical canonical), and it is golden-safe (the M1/M2/corpus paths carry
/// all-zero events ⇒ canonical == raw; the Linux paths' goldens are relative
/// deterministic-twice checks, so no pinned value moves).
fn encode_events(v: &mut Vec<u8>, raw: &vmm_backend::VcpuEvents) {
    let e = &records::canonical_events(raw);
    v.extend_from_slice(&[
        e.exception_injected,
        e.exception_nr,
        e.exception_has_error_code,
        e.exception_pending,
    ]);
    v.extend_from_slice(&e.exception_error_code.to_le_bytes());
    v.push(e.exception_has_payload);
    v.extend_from_slice(&e.exception_payload.to_le_bytes());
    v.extend_from_slice(&[
        e.interrupt_injected,
        e.interrupt_nr,
        e.interrupt_soft,
        e.interrupt_shadow,
        e.nmi_injected,
        e.nmi_pending,
        e.nmi_masked,
    ]);
    v.extend_from_slice(&e.sipi_vector.to_le_bytes());
    v.extend_from_slice(&e.flags.to_le_bytes());
    v.extend_from_slice(&[
        e.smi_smm,
        e.smi_pending,
        e.smi_inside_nmi,
        e.smi_latched_init,
        e.triple_fault_pending,
    ]);
}

/// The frozen V-time clock config (CPU-MSR-CONTRACT: the guest TSC is **2.0 GHz**,
/// leaf `0x15`). The work→nanosecond ratio is **1 ns per retired conditional
/// branch** — an integer ratio (`ratio_den == 1`), which INTEGRATION.md §4
/// requires for any snapshot-bearing config (a fractional ratio's sub-ns
/// remainder cannot survive `snapshot_vns`). So `tsc(work) = 2 · work` ticks,
/// strictly increasing whenever the guest retires a branch between two reads.
pub fn contract_vclock_config() -> VClockConfig {
    VClockConfig {
        ratio_num: 1,
        ratio_den: 1,
        guest_hz: 2_000_000_000,
        guest_base: 0,
        vns_base: 0,
    }
}

/// MSR access direction for the loud §1 log line — carries both the human
/// direction (`RDMSR`/`WRMSR`) and the KVM userspace exit reason it surfaces as.
#[derive(Clone, Copy)]
pub(crate) enum MsrDir {
    Read,
    Write,
}

impl MsrDir {
    pub(crate) fn dir(self) -> &'static str {
        match self {
            MsrDir::Read => "RDMSR",
            MsrDir::Write => "WRMSR",
        }
    }
    /// The KVM exit reason this direction surfaces as (CPU-MSR-CONTRACT §1).
    pub(crate) fn exit_reason(self) -> &'static str {
        match self {
            MsrDir::Read => "KVM_EXIT_X86_RDMSR",
            MsrDir::Write => "KVM_EXIT_X86_WRMSR",
        }
    }
}

/// The x86 half of a validated-but-uncommitted `vm_state` restore
/// ([`Vendor::validate_restore`](crate::vendor::Vendor::validate_restore) →
/// [`Vendor::commit_restore`](crate::vendor::Vendor::commit_restore)): the
/// coherence-checked xAPIC and the decoded device blob.
pub struct X86RestorePrep {
    lapic: Option<lapic::Lapic>,
    dev: DeviceState,
}

/// The x86 register-file breakdown for the **diagnostic**
/// [`Vmm::state_components`] (never part of `state_hash`): GPRs, segments,
/// descriptor tables, control regs, PDPTRs, XCR0, debug regs, pending events, MP
/// state, MSRs, and the three XSAVE sub-areas — the prime suspects for
/// host-leaked / init-optimization bytes. Labels are stable and in a fixed order.
pub(crate) fn vcpu_components(s: &VcpuState, out: &mut Vec<(&'static str, [u8; 32])>) {
    fn dig(bytes: &[u8]) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(bytes);
        h.finalize().into()
    }

    let mut gpr = Vec::new();
    let r = &s.regs;
    for x in [
        r.rax, r.rbx, r.rcx, r.rdx, r.rsi, r.rdi, r.rsp, r.rbp, r.r8, r.r9, r.r10, r.r11, r.r12,
        r.r13, r.r14, r.r15, r.rip, r.rflags,
    ] {
        gpr.extend_from_slice(&x.to_le_bytes());
    }
    out.push(("regs", dig(&gpr)));

    let mut seg = Vec::new();
    for sg in [
        &s.sregs.cs,
        &s.sregs.ds,
        &s.sregs.es,
        &s.sregs.fs,
        &s.sregs.gs,
        &s.sregs.ss,
        &s.sregs.tr,
        &s.sregs.ldt,
    ] {
        encode_segment(&mut seg, sg);
    }
    out.push(("segments", dig(&seg)));

    let mut dt = Vec::new();
    for t in [&s.sregs.gdt, &s.sregs.idt] {
        dt.extend_from_slice(&t.base.to_le_bytes());
        dt.extend_from_slice(&t.limit.to_le_bytes());
    }
    out.push(("desc-tables", dig(&dt)));

    let mut cr = Vec::new();
    for x in [
        s.sregs.cr0,
        s.sregs.cr2,
        s.sregs.cr3,
        s.sregs.cr4,
        s.sregs.cr8,
        s.sregs.efer,
        s.sregs.apic_base,
        s.sregs.flags,
    ] {
        cr.extend_from_slice(&x.to_le_bytes());
    }
    out.push(("control-regs", dig(&cr)));

    let mut pd = Vec::new();
    for x in s.sregs.pdptrs {
        pd.extend_from_slice(&x.to_le_bytes());
    }
    out.push(("pdptrs", dig(&pd)));
    out.push(("xcr0", dig(&s.xcr0.to_le_bytes())));

    let mut dr = Vec::new();
    for d in s.debugregs.db {
        dr.extend_from_slice(&d.to_le_bytes());
    }
    dr.extend_from_slice(&s.debugregs.dr6.to_le_bytes());
    dr.extend_from_slice(&s.debugregs.dr7.to_le_bytes());
    dr.extend_from_slice(&s.debugregs.flags.to_le_bytes());
    out.push(("debugregs", dig(&dr)));

    let mut ev = Vec::new();
    encode_events(&mut ev, &s.events);
    out.push(("events", dig(&ev)));

    let mp = match s.mp_state {
        vmm_backend::MpState::Runnable => 0u8,
        vmm_backend::MpState::Halted => 1,
    };
    out.push(("mp_state", dig(&[mp])));

    let mut msr = Vec::new();
    for (idx, val) in &s.msrs {
        msr.extend_from_slice(&idx.to_le_bytes());
        msr.extend_from_slice(&val.to_le_bytes());
    }
    out.push(("msrs", dig(&msr)));

    let xs = &s.xsave;
    let part = |lo: usize, hi: usize| {
        let (lo, hi) = (lo.min(xs.len()), hi.min(xs.len()));
        dig(&xs[lo..hi])
    };
    out.push(("xsave-legacy", part(0, 512)));
    out.push(("xsave-header", part(512, 576)));
    out.push(("xsave-extended", part(576, xs.len())));
}
