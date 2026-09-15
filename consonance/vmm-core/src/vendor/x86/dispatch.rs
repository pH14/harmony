// SPDX-License-Identifier: AGPL-3.0-or-later

use hypercall_proto::Service;
use vmm_backend::{Backend, CommonExit, Exit, Gpa, VcpuState, X86, X86Completion, X86Exit};
use vtime::VClockConfig;

use crate::snapshot::SnapshotError;
use crate::vendor::x86::contract::{self, MsrDisposition};
use crate::vendor::x86::devices::{ISA_DEBUG_EXIT_PORT, LegacyPlatform, REPORT_PORT, Uart8250};
use crate::vendor::x86::records::{self, DeviceState, LegacyState, UartState};
use crate::virtual_time::{DeviceClass, NormalizedEventClass};
use crate::vmm::{Step, TerminalReason, Vmm, VmmError};

pub struct X86Devices {
    pub(crate) uart: Uart8250,
    pub(crate) lapic: Option<lapic::Lapic>,
    pub(crate) legacy: Option<LegacyPlatform>,
}

impl X86Devices {
    pub(crate) fn new() -> Self {
        Self {
            uart: Uart8250::new(),
            lapic: None,
            legacy: None,
        }
    }
}

pub(crate) const APIC_MMIO_BASE: u64 = 0xFEE0_0000;

pub(crate) const APIC_MMIO_END: u64 = 0xFEE0_1000;

pub(crate) const COM1_IRQ: u8 = 4;

pub(crate) const COM1_IRQ_VECTOR: u8 = 0x34;

pub(crate) const IA32_TSC: u32 = 0x10;

pub(crate) const IA32_TSC_ADJUST: u32 = 0x3b;

pub(crate) const DOORBELL_PORT: u16 = 0x0CA1;

pub(crate) const VIRTUAL_TIME_TICK_PORT: u16 = 0x0CA3;

pub(crate) const RFLAGS_IF: u64 = 1 << 9;

pub(crate) fn port_event_class(port: u16) -> NormalizedEventClass {
    if port == ISA_DEBUG_EXIT_PORT {
        NormalizedEventClass::Terminal
    } else if port == DOORBELL_PORT {
        NormalizedEventClass::Doorbell
    } else if Uart8250::owns(port) {
        NormalizedEventClass::DeviceMmio(DeviceClass::Serial)
    } else if matches!(port, 0x0020 | 0x0021 | 0x00A0 | 0x00A1 | 0x04D0 | 0x04D1) {
        NormalizedEventClass::DeviceMmio(DeviceClass::InterruptController)
    } else {
        NormalizedEventClass::DeviceMmio(DeviceClass::Paravirtual)
    }
}

fn msr_event_class(index: u32) -> NormalizedEventClass {
    if matches!(index, IA32_TSC | IA32_TSC_ADJUST) {
        NormalizedEventClass::TimeRead
    } else {
        NormalizedEventClass::ArchitecturalControl
    }
}

pub(crate) fn normalize_virtual_time_exit_x86(exit: &Exit<X86>) -> (NormalizedEventClass, Vec<u8>) {
    match exit {
        Exit::Common(CommonExit::Mmio { gpa, size, write }) => {
            let mut payload = gpa.0.to_le_bytes().to_vec();
            payload.push(*size);
            match write {
                Some(value) => {
                    payload.push(1);
                    payload.extend_from_slice(&value.to_le_bytes());
                }
                None => payload.push(0),
            }
            let class = if (APIC_MMIO_BASE..APIC_MMIO_END).contains(&gpa.0) {
                NormalizedEventClass::DeviceMmio(DeviceClass::InterruptController)
            } else {
                NormalizedEventClass::DeviceMmio(DeviceClass::Paravirtual)
            };
            (class, payload)
        }
        Exit::Common(CommonExit::Idle) => (NormalizedEventClass::Idle, Vec::new()),
        Exit::Common(CommonExit::Shutdown) => (NormalizedEventClass::Terminal, Vec::new()),
        Exit::Common(CommonExit::Hypercall(frame)) => {
            let mut payload = Vec::new();
            for arg in frame.args {
                payload.extend_from_slice(&arg.to_le_bytes());
            }
            (NormalizedEventClass::Doorbell, payload)
        }
        Exit::Arch(arch) => {
            let mut payload = vec![match arch {
                X86Exit::Io { .. } => 0u8,
                X86Exit::Rdmsr { .. } => 1,
                X86Exit::Wrmsr { .. } => 2,
                X86Exit::Cpuid { .. } => 3,
            }];
            let class = match arch {
                X86Exit::Io { port, size, write } => {
                    payload.extend_from_slice(&port.to_le_bytes());
                    payload.push(*size);
                    match write {
                        Some(value) => {
                            payload.push(1);
                            payload.extend_from_slice(&value.to_le_bytes());
                        }
                        None => payload.push(0),
                    }
                    port_event_class(*port)
                }
                X86Exit::Rdmsr { index } => {
                    payload.extend_from_slice(&index.to_le_bytes());
                    msr_event_class(*index)
                }
                X86Exit::Wrmsr { index, value } => {
                    payload.extend_from_slice(&index.to_le_bytes());
                    payload.extend_from_slice(&value.to_le_bytes());
                    msr_event_class(*index)
                }
                X86Exit::Cpuid { leaf, subleaf } => {
                    payload.extend_from_slice(&leaf.to_le_bytes());
                    payload.extend_from_slice(&subleaf.to_le_bytes());
                    NormalizedEventClass::ArchitecturalControl
                }
            };
            (class, payload)
        }
    }
}

impl<B: Backend<A = X86>> Vmm<B> {
    fn advance_virtual_time_for_port(&mut self, port: u16) -> Result<(), VmmError> {
        if !self.virtual_time_vtime_enabled() {
            return Ok(());
        }
        let vns = if port == VIRTUAL_TIME_TICK_PORT {
            contract::virtual_time_timing().execution_tick_vns
        } else {
            match port_event_class(port) {
                NormalizedEventClass::DeviceMmio(class) => {
                    contract::virtual_time_timing().mmio_vns(class)
                }
                _ => return Ok(()),
            }
        };
        self.advance_virtual_time_vtime(vns)
    }

    fn advance_virtual_time_for_msr(&mut self, disp: &MsrDisposition) -> Result<(), VmmError> {
        if !self.virtual_time_vtime_enabled() {
            return Ok(());
        }
        let timing = contract::virtual_time_timing();
        let vns = if matches!(disp, MsrDisposition::EmulateVtime) {
            timing.trapped_time_read_vns
        } else {
            timing.architectural_control_vns
        };
        self.advance_virtual_time_vtime(vns)
    }

    pub(crate) fn dispatch_out(
        &mut self,
        port: u16,
        size: u8,
        value: u32,
    ) -> Result<Step, VmmError> {
        if port == VIRTUAL_TIME_TICK_PORT {
            require_dword_io("OUT", port, size)?;
            if value != 1 {
                return Err(VmmError::ContractViolation(format!(
                    "execution-tick protocol fault: OUT {port:#06x} value {value:#x} (must be 1)"
                )));
            }
            self.advance_virtual_time_for_port(port)?;
            return Ok(Step::Continued);
        }
        self.advance_virtual_time_for_port(port)?;
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
            require_dword_io("OUT", port, size)?;
            self.report_stream.push(value);
            return Ok(Step::Continued);
        }
        if port == DOORBELL_PORT {
            require_dword_io("OUT", DOORBELL_PORT, size)?;
            return self.service_doorbell(value);
        }
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
        if port == VIRTUAL_TIME_TICK_PORT {
            return Err(VmmError::ContractViolation(format!(
                "execution-tick protocol fault: IN {port:#06x} (size {size}); the tick port is \
                 write-only"
            )));
        }
        self.advance_virtual_time_for_port(port)?;
        if Uart8250::owns(port) {
            require_byte_io("IN", port, size)?;
            if let Some(byte) = self.devices.uart.read_in(port) {
                self.backend.complete_read(u64::from(byte))?;
                return Ok(Step::Continued);
            }
        }
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
        if self.virtual_time_vtime_enabled() {
            self.advance_virtual_time_vtime(
                contract::virtual_time_timing().interrupt_controller_mmio_vns,
            )?;
        }
        let now_vns = self.now_vns()?;
        let offset = (gpa.0 - APIC_MMIO_BASE) as u32;
        match write {
            None => {
                let lapic = self
                    .devices
                    .lapic
                    .as_mut()
                    .expect("in_apic_page implies wired");
                let value = lapic.mmio_read(offset, now_vns).map_err(|e| {
                    VmmError::ContractViolation(format!("xAPIC read {offset:#x}: {e}"))
                })?;
                self.backend.complete_read(u64::from(value))?;
                Ok(Step::Continued)
            }
            Some(v) => {
                let (deadline_before, deadline_after, timer_id) = {
                    let lapic = self
                        .devices
                        .lapic
                        .as_mut()
                        .expect("in_apic_page implies wired");
                    let before = lapic.next_timer_deadline();
                    lapic.mmio_write(offset, v as u32, now_vns).map_err(|e| {
                        VmmError::ContractViolation(format!("xAPIC write {offset:#x}: {e}"))
                    })?;
                    (
                        before,
                        lapic.next_timer_deadline(),
                        u32::from(lapic.timer_vector()),
                    )
                };
                if deadline_after != deadline_before {
                    match deadline_after {
                        Some(deadline_vns) => {
                            self.trace_clockevent_schedule_vns(deadline_vns, timer_id)?;
                        }
                        None => self.trace_clockevent_cancel()?,
                    }
                }
                Ok(Step::Continued)
            }
        }
    }

    pub(crate) fn service_lapic_timer_due(&mut self) -> Result<(), VmmError> {
        if !self.virtual_time_vtime_enabled() || self.devices.lapic.is_none() {
            return Ok(());
        }
        let now_vns = self.now_vns()?;
        let (fired, next_deadline, timer_id) = {
            let lapic = self.devices.lapic.as_mut().expect("is_some checked above");
            let fired = lapic.advance_to(now_vns);
            (
                fired,
                lapic.next_timer_deadline(),
                u32::from(lapic.timer_vector()),
            )
        };
        if fired {
            self.trace_clockevent_delivery()?;
            if let Some(deadline_vns) = next_deadline {
                self.trace_clockevent_schedule_vns(deadline_vns, timer_id)?;
            }
        }
        Ok(())
    }

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
                 arbitration path to assert the vector through (host interrupts are enforced \
                 through the Linux-boot xAPIC)"
            )));
        };
        lapic.raise(vector).map_err(|e| {
            VmmError::ContractViolation(format!(
                "InjectInterrupt vector {vector:#x} rejected: {e:?}"
            ))
        })
    }

    pub(crate) fn service_pending_irqs(&mut self) -> Result<(), VmmError> {
        if self.devices.lapic.is_none() {
            return Ok(());
        }
        let now_vns = self.now_vns()?;
        let lapic_vector = {
            let lapic = self.devices.lapic.as_mut().expect("is_some checked above");
            lapic.advance_to(now_vns);
            lapic.peek_interrupt()
        };
        let vector = lapic_vector.or_else(|| self.pending_serial_vector());
        self.backend.set_pending_irq(vector)?;
        Ok(())
    }

    pub(crate) fn pending_serial_vector(&self) -> Option<u8> {
        let legacy = self.devices.legacy.as_ref()?;
        (self.devices.uart.serial_irq_asserted() && !legacy.irq_masked(COM1_IRQ))
            .then_some(COM1_IRQ_VECTOR)
    }

    pub(crate) fn complete_irq_delivery(&mut self) {
        while self.backend.take_accepted_interrupt().is_some() {
            if let Some(lapic) = self.devices.lapic.as_mut() {
                lapic.take_interrupt();
            }
        }
    }

    pub(crate) fn dispatch_rdmsr(&mut self, index: u32) -> Result<Step, VmmError> {
        let disp = contract::rdmsr_disposition(index);
        self.advance_virtual_time_for_msr(&disp)?;
        loud_msr(
            MsrDir::Read,
            index,
            None,
            self.guest_rip(),
            self.current_vns(),
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
            MsrDisposition::AllowStateful | MsrDisposition::DenyIgnoreWrite => {
                Err(VmmError::ContractViolation(format!(
                    "RDMSR {index:#x} surfaced with a non-userspace disposition {disp:?}"
                )))
            }
        }
    }

    pub(crate) fn dispatch_wrmsr(&mut self, index: u32, value: u64) -> Result<Step, VmmError> {
        let disp = contract::wrmsr_disposition(index, value);
        self.advance_virtual_time_for_msr(&disp)?;
        loud_msr(
            MsrDir::Write,
            index,
            Some(value),
            self.guest_rip(),
            self.current_vns(),
            &disp,
        );
        match disp {
            MsrDisposition::DenyIgnoreWrite => {
                self.backend.complete_ok()?;
                Ok(Step::Continued)
            }
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
        if self.virtual_time_vtime_enabled() {
            self.advance_virtual_time_vtime(
                contract::virtual_time_timing().architectural_control_vns,
            )?;
        }
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

    pub(crate) fn rdmsr_vtime(&mut self, index: u32) -> Result<Step, VmmError> {
        let value = {
            let Some(vt) = self.vtime.as_mut() else {
                return Err(VmmError::ContractViolation(format!(
                    "emulate-vtime RDMSR {index:#x} surfaced but V-time is not wired (stock \
                     backend?) — refusing to supply a host value"
                )));
            };
            match index {
                IA32_TSC => vt.guest_clock(),
                IA32_TSC_ADJUST => vt.guest_clock_offset,
                other => {
                    return Err(VmmError::ContractViolation(format!(
                        "emulate-vtime RDMSR {other:#x} is not a V-time MSR (only IA32_TSC 0x10 and \
                         IA32_TSC_ADJUST 0x3b are emulate-vtime)"
                    )));
                }
            }
        };
        self.backend.complete_read(value)?;
        Ok(Step::Continued)
    }

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
                    vt.guest_clock_offset = value.wrapping_sub(vt.clock.guest_ticks());
                }
                IA32_TSC_ADJUST => {
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
        Ok(Step::Continued)
    }

    pub(crate) fn guest_rip(&self) -> u64 {
        self.backend.save().map(|s| s.regs.rip).unwrap_or_default()
    }

    pub fn wire_lapic(&mut self, lapic: lapic::Lapic) -> &mut Self {
        self.devices.lapic = Some(lapic);
        self.devices.legacy = Some(LegacyPlatform::new());
        self
    }

    pub fn lapic_wired(&self) -> bool {
        self.devices.lapic.is_some()
    }

    pub(crate) fn has_pending_guest_interrupt_x86(&mut self) -> Result<bool, VmmError> {
        if self.devices.lapic.is_none() {
            return Ok(self.pending_serial_vector().is_some());
        }
        let now = self.now_vns()?;
        let lapic_pending = {
            let lapic = self.devices.lapic.as_mut().expect("is_some checked above");
            lapic.advance_to(now);
            lapic.peek_interrupt().is_some()
        };
        Ok(lapic_pending || self.pending_serial_vector().is_some())
    }

    pub(crate) fn build_vm_state(&self, vcpu: &VcpuState) -> vm_state::VmState {
        let mut s = vm_state::VmState::default();
        records::fill_vcpu_state(&mut s, vcpu);
        let tsc_adjust = match &self.vtime {
            Some(vt) => {
                s.vtime = vm_state::VtimeState {
                    guest_hz: vt.cfg.guest_hz,
                    guest_base: vt.cfg.guest_base,
                    snapshot_vns: vt.clock.vns(),
                };
                s.hypercall = vt.entropy.save_state();
                vt.guest_clock_offset
            }
            None => 0,
        };
        let dev = DeviceState {
            tsc_adjust,
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
            events: records::canonical_events(&vcpu.events),
            pvclock: self.pvclock_snapshot(),
        };
        s.devices = records::encode_device_blob(&dev);
        s.contract_hash = contract::contract_hash();
        s
    }

    pub(crate) fn validate_restore_x86(
        &self,
        s: &vm_state::VmState,
    ) -> Result<(VcpuState, u64, X86RestorePrep), VmmError> {
        if s.contract_hash != contract::contract_hash() {
            return Err(VmmError::Snapshot(SnapshotError::ContractMismatch));
        }
        vmm_backend::restore_xsave_image(&s.xsave.0, s.xsave_restore_bv).map_err(|error| {
            VmmError::ContractViolation(format!(
                "restore_vm_state: invalid XSAVE restore provenance: {error}"
            ))
        })?;
        let dev = records::decode_device_blob(&s.devices.0)?;
        if let Some(reason) = records::unrestorable_events(&dev.events) {
            return Err(VmmError::ContractViolation(format!(
                "restore_vm_state: {reason}"
            )));
        }
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
        if dev.legacy.is_some() != self.devices.legacy.is_some() {
            return Err(VmmError::ContractViolation(
                "restore_vm_state: snapshot/VM legacy-platform wiring mismatch (one has the \
                 8259/PCI latches, the other does not) — restore into a VM composed like the \
                 snapshot source."
                    .to_string(),
            ));
        }
        self.pvclock_validate_restore(dev.pvclock.as_ref())?;
        let mut vcpu = records::vcpu_state_from(s);
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

fn loud_msr(
    dir: MsrDir,
    index: u32,
    data: Option<u64>,
    rip: u64,
    vns: Option<u64>,
    disp: &MsrDisposition,
) {
    let data = match data {
        Some(v) => format!("{v:#x}"),
        None => "n/a".to_string(),
    };
    let vns = match vns {
        Some(n) => n.to_string(),
        None => "unwired".to_string(),
    };
    eprintln!(
        "[vmm-core] msr-exit dir={} exit-reason={} index={index:#x} data={data} rip={rip:#x} \
         vns={vns} disposition={disp:?}",
        dir.dir(),
        dir.exit_reason(),
    );
}

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
    v.extend_from_slice(&(s.msrs.len() as u64).to_le_bytes());
    for (idx, val) in &s.msrs {
        v.extend_from_slice(&idx.to_le_bytes());
        v.extend_from_slice(&val.to_le_bytes());
    }
    v.extend_from_slice(&(s.xsave.len() as u64).to_le_bytes());
    v.extend_from_slice(&s.xsave);
    if let Some(restore_bv) = s.xsave_restore_bv {
        v.extend_from_slice(b"XSRB");
        v.extend_from_slice(&restore_bv.to_le_bytes());
    }
    v
}

fn encode_segment(v: &mut Vec<u8>, seg: &vmm_backend::Segment) {
    v.extend_from_slice(&seg.base.to_le_bytes());
    v.extend_from_slice(&seg.limit.to_le_bytes());
    v.extend_from_slice(&seg.selector.to_le_bytes());
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

pub fn contract_vclock_config() -> VClockConfig {
    VClockConfig {
        guest_hz: 2_000_000_000,
        guest_base: 0,
        vns_base: 0,
    }
}

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
    pub(crate) fn exit_reason(self) -> &'static str {
        match self {
            MsrDir::Read => "KVM_EXIT_X86_RDMSR",
            MsrDir::Write => "KVM_EXIT_X86_WRMSR",
        }
    }
}

pub struct X86RestorePrep {
    lapic: Option<lapic::Lapic>,
    dev: DeviceState,
}

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
    let header_start = 512.min(xs.len());
    let header_end = 576.min(xs.len());
    out.push(("xsave-header", dig(&xs[header_start..header_end])));
    if let Some(value) = s.xsave_restore_bv {
        out.push(("xsave-restore-bv", dig(&value.to_le_bytes())));
    }
    out.push(("xsave-extended", part(576, xs.len())));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_xsave_image() -> Vec<u8> {
        let mut image = vec![0; 576];
        image[0..2].copy_from_slice(&0x037Fu16.to_le_bytes());
        image[24..28].copy_from_slice(&0x1F80u32.to_le_bytes());
        image[28..32].copy_from_slice(&0x0000FFFFu32.to_le_bytes());
        image[512..520].copy_from_slice(&3u64.to_le_bytes());
        vmm_backend::arch::x86::canonicalize_xsave(&mut image);
        image
    }

    fn xsave_state(restore_bv: Option<u64>) -> VcpuState {
        VcpuState {
            xsave: canonical_xsave_image(),
            xsave_restore_bv: restore_bv,
            ..Default::default()
        }
    }

    #[test]
    fn xsave_restore_provenance_is_an_optional_identity_suffix() {
        let legacy = encode_vcpu_state(&xsave_state(None));
        let with_zero = encode_vcpu_state(&xsave_state(Some(0)));
        let with_three = encode_vcpu_state(&xsave_state(Some(3)));
        let with_two = encode_vcpu_state(&xsave_state(Some(2)));

        assert!(with_zero.starts_with(&legacy));
        assert!(with_three.starts_with(&legacy));
        assert_eq!(with_zero.len(), legacy.len() + 12);
        assert_eq!(with_three.len(), legacy.len() + 12);
        assert_eq!(with_two.len(), legacy.len() + 12);
        assert_ne!(with_zero, with_two);
        assert_ne!(with_zero, with_three);
        assert_ne!(with_three, with_two);
    }
}
