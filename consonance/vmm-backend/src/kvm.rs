// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use kvm_bindings::{
    KVM_CPUID_FLAG_SIGNIFCANT_INDEX, KVM_EXIT_FAIL_ENTRY, KVM_EXIT_HLT, KVM_EXIT_INTERNAL_ERROR,
    KVM_EXIT_IO, KVM_EXIT_IO_IN, KVM_EXIT_IRQ_WINDOW_OPEN, KVM_EXIT_MMIO, KVM_EXIT_SHUTDOWN,
    KVM_EXIT_X86_RDMSR, KVM_EXIT_X86_WRMSR, KVM_MP_STATE_HALTED, KVM_MP_STATE_RUNNABLE,
    kvm_cpuid_entry2, kvm_debugregs, kvm_dtable, kvm_msr_entry, kvm_regs, kvm_run, kvm_segment,
    kvm_sregs2, kvm_vcpu_events, kvm_xcrs, kvm_xsave,
};

use crate::arch::x86::{CpuidModel, MsrFilter, X86, X86Caps, X86Exit};
use crate::arch::x86::{
    DebugRegs, DescriptorTable, Segment, VcpuEvents, VcpuRegs, VcpuSregs, VcpuState,
};
use crate::error::{BackendError, Result};
use crate::exit::{Capabilities, CommonExit, Exit};
use crate::run_buf::RunBuf;
use crate::types::{Gpa, MpState};

pub(crate) fn kvm_err(e: kvm_ioctls::Error) -> BackendError {
    BackendError::Io(std::io::Error::from_raw_os_error(e.errno()))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Pending {
    None,
    IoIn { data_offset: u64, size: u8 },
    MmioLoad { len: u32 },
    Rdmsr,
    Wrmsr,
}

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

    fn io(&self) -> (u8, u8, u16, u32, u64) {
        // SAFETY: read only for a `KVM_EXIT_IO`, where `io` is the active union
        // member; the struct is `Copy` and read out wholesale.
        let io = unsafe { (*self.run).__bindgen_anon_1.io };
        (io.direction, io.size, io.port, io.count, io.data_offset)
    }

    fn mmio(&self) -> (u64, u32, u8, [u8; 8]) {
        // SAFETY: active union member for a `KVM_EXIT_MMIO`; `Copy` read.
        let m = unsafe { (*self.run).__bindgen_anon_1.mmio };
        (m.phys_addr, m.len, m.is_write, m.data)
    }

    fn msr(&self) -> (u32, u64) {
        // SAFETY: active union member for an MSR exit; `Copy` read.
        let m = unsafe { (*self.run).__bindgen_anon_1.msr };
        (m.index, m.data)
    }

    fn read_pio(&self, data_offset: u64, size: u8) -> Result<u32> {
        let n = (size as usize).min(4);
        // SAFETY: `run`/`len` describe a live buffer; `RunBuf` bound-checks the
        // offset and rejects anything past `len`.
        let buf = unsafe { RunBuf::new(self.run.cast::<u8>(), self.len) };
        let mut bytes = [0u8; 4];
        buf.read_bytes(data_offset as usize, &mut bytes[..n])?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn write_pio(&self, data_offset: u64, size: u8, value: u64) -> Result<()> {
        let n = (size as usize).min(4);
        let bytes = (value as u32).to_le_bytes();
        // SAFETY: as `read_pio`; `RunBuf` bound-checks the write.
        let mut buf = unsafe { RunBuf::new(self.run.cast::<u8>(), self.len) };
        buf.write_bytes(data_offset as usize, &bytes[..n])
    }

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

    fn set_msr(&self, data: u64, error: u8) {
        // SAFETY: active union member for a pending MSR exit; in-place field writes.
        unsafe {
            let m = &mut (*self.run).__bindgen_anon_1.msr;
            m.data = data;
            m.error = error;
        }
    }

    fn set_msr_error(&self, error: u8) {
        // SAFETY: active union member for a pending MSR exit; in-place field write.
        unsafe {
            (*self.run).__bindgen_anon_1.msr.error = error;
        }
    }

    fn ready_for_interrupt_injection(&self) -> u8 {
        // SAFETY: `run` is a valid `kvm_run` (constructor contract); this is a
        // plain, always-initialized top-level field.
        unsafe { (*self.run).ready_for_interrupt_injection }
    }

    #[cfg(test)]
    pub(crate) fn immediate_exit(&self) -> u8 {
        // SAFETY: `run` is a valid `kvm_run` (constructor contract); this is a
        // plain, always-initialized top-level field.
        unsafe { (*self.run).immediate_exit }
    }

    pub(crate) fn set_immediate_exit(&self, on: bool) {
        // SAFETY: as above; in-place write of a plain top-level field.
        unsafe {
            (*self.run).immediate_exit = u8::from(on);
        }
    }

    fn set_request_interrupt_window(&self, on: bool) {
        // SAFETY: as above; in-place write of a plain top-level field.
        unsafe {
            (*self.run).request_interrupt_window = u8::from(on);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum IrqEntry {
    Queue(u8),
    Run,
}

pub(crate) fn plan_irq_entry(
    page: RunPage,
    pending_irq: Option<u8>,
    readiness_current: bool,
) -> IrqEntry {
    match pending_irq {
        Some(vector) if readiness_current && page.ready_for_interrupt_injection() != 0 => {
            page.set_request_interrupt_window(false);
            IrqEntry::Queue(vector)
        }
        Some(_) => {
            page.set_request_interrupt_window(true);
            IrqEntry::Run
        }
        None => {
            page.set_request_interrupt_window(false);
            IrqEntry::Run
        }
    }
}

pub(crate) fn decode_exit(page: RunPage) -> Result<Option<(Exit<X86>, Pending)>> {
    match page.exit_reason() {
        KVM_EXIT_IO => decode_io(page).map(Some),
        KVM_EXIT_MMIO => Ok(Some(decode_mmio(page))),
        KVM_EXIT_X86_RDMSR => {
            let (index, _) = page.msr();
            Ok(Some((Exit::Arch(X86Exit::Rdmsr { index }), Pending::Rdmsr)))
        }
        KVM_EXIT_X86_WRMSR => {
            let (index, value) = page.msr();
            Ok(Some((
                Exit::Arch(X86Exit::Wrmsr { index, value }),
                Pending::Wrmsr,
            )))
        }
        KVM_EXIT_HLT => Ok(Some((Exit::Common(CommonExit::Idle), Pending::None))),
        KVM_EXIT_SHUTDOWN => Ok(Some((Exit::Common(CommonExit::Shutdown), Pending::None))),
        KVM_EXIT_INTERNAL_ERROR => Err(BackendError::Internal("KVM_EXIT_INTERNAL_ERROR")),
        KVM_EXIT_FAIL_ENTRY => Err(BackendError::Internal("KVM_EXIT_FAIL_ENTRY")),
        KVM_EXIT_IRQ_WINDOW_OPEN => Ok(None),
        _ => Err(BackendError::Internal("unhandled KVM exit reason")),
    }
}

pub(crate) fn decoded_exit_stages_completion(exit: &Exit<X86>, pending: Pending) -> bool {
    exit.stages_completion() && pending == Pending::None
}

fn decode_io(page: RunPage) -> Result<(Exit<X86>, Pending)> {
    let (direction, size, port, count, data_offset) = page.io();
    if count != 1 {
        return Err(BackendError::Unsupported {
            what: "string/REP port I/O (io.count != 1)",
        });
    }
    if u32::from(direction) != KVM_EXIT_IO_IN {
        let value = page.read_pio(data_offset, size)?;
        Ok((
            Exit::Arch(X86Exit::Io {
                port,
                size,
                write: Some(value),
            }),
            Pending::None,
        ))
    } else {
        Ok((
            Exit::Arch(X86Exit::Io {
                port,
                size,
                write: None,
            }),
            Pending::IoIn { data_offset, size },
        ))
    }
}

fn decode_mmio(page: RunPage) -> (Exit<X86>, Pending) {
    let (phys_addr, len, is_write, data) = page.mmio();
    let gpa = Gpa(phys_addr);
    let size = len.min(8) as u8;
    if is_write != 0 {
        let mut bytes = [0u8; 8];
        let n = (len as usize).min(8);
        bytes[..n].copy_from_slice(&data[..n]);
        (
            Exit::Common(CommonExit::Mmio {
                gpa,
                size,
                write: Some(u64::from_le_bytes(bytes)),
            }),
            Pending::None,
        )
    } else {
        (
            Exit::Common(CommonExit::Mmio {
                gpa,
                size,
                write: None,
            }),
            Pending::MmioLoad { len },
        )
    }
}

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

pub(crate) fn apply_complete_fault(page: RunPage, pending: Pending) -> Result<()> {
    match pending {
        Pending::Rdmsr | Pending::Wrmsr => {
            page.set_msr_error(1);
            Ok(())
        }
        _ => Err(BackendError::BadCompletion),
    }
}

pub(crate) fn apply_complete_ok(page: RunPage, pending: Pending) -> Result<()> {
    match pending {
        Pending::Wrmsr => {
            page.set_msr_error(0);
            Ok(())
        }
        _ => Err(BackendError::BadCompletion),
    }
}

pub(crate) fn retire_staged_completion<F>(
    page: RunPage,
    pending: &mut Pending,
    staged: &mut bool,
    mut enter: F,
) -> Result<()>
where
    F: FnMut() -> std::result::Result<(), std::io::Error>,
{
    if !*staged {
        return Ok(());
    }

    page.set_immediate_exit(true);
    let result = enter();
    page.set_immediate_exit(false);

    match result {
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
            *staged = false;
            *pending = Pending::None;
            Ok(())
        }
        Err(error) => Err(BackendError::Io(error)),
        Ok(()) => Err(BackendError::Internal(
            "completion-only KVM_RUN returned without EINTR",
        )),
    }
}

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

pub(crate) fn ensure_full_msr_count(serviced: usize, requested: usize) -> Result<()> {
    if serviced != requested {
        return Err(BackendError::Internal(
            "KVM MSR ioctl serviced a short count (an MSR index was rejected)",
        ));
    }
    Ok(())
}

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

pub(crate) fn restore_sregs2_with_flush<F>(state: &VcpuSregs, mut set: F) -> Result<()>
where
    F: FnMut(&kvm_sregs2) -> Result<()>,
{
    let target = to_kvm_sregs2(state);
    let mut transient = target;
    transient.cr0 ^= 1 << 16;
    set(&transient)?;
    set(&target)
}

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
    let actual: Vec<u32> = state.msrs.keys().copied().collect();
    if actual != expected {
        return Err(BackendError::InvalidState);
    }
    if state.xsave.len() != xsave_len {
        return Err(BackendError::InvalidState);
    }
    Ok(())
}

pub(crate) fn kvm_capabilities() -> Capabilities<X86Caps> {
    Capabilities {
        name: "kvm-stock",
        arch: X86Caps,
    }
}

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

pub(crate) fn mp_from_kvm(mp_state: u32) -> MpState {
    if mp_state == KVM_MP_STATE_HALTED {
        MpState::Halted
    } else {
        MpState::Runnable
    }
}

pub(crate) fn mp_to_kvm(mp: MpState) -> u32 {
    match mp {
        MpState::Halted => KVM_MP_STATE_HALTED,
        MpState::Runnable => KVM_MP_STATE_RUNNABLE,
    }
}

pub(crate) fn xcr0_of(x: &kvm_xcrs) -> u64 {
    x.xcrs
        .iter()
        .take(x.nr_xcrs as usize)
        .find(|e| e.xcr == 0)
        .map_or(0, |e| e.value)
}

pub(crate) fn xcrs_of(xcr0: u64) -> kvm_xcrs {
    let mut x = kvm_xcrs {
        nr_xcrs: 1,
        ..Default::default()
    };
    x.xcrs[0].xcr = 0;
    x.xcrs[0].value = xcr0;
    x
}

pub(crate) fn xsave_to_bytes(x: &kvm_xsave) -> Vec<u8> {
    let mut out = Vec::with_capacity(x.region.len() * 4);
    for word in &x.region {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

pub(crate) fn xsave_from_bytes(bytes: &[u8]) -> Result<kvm_xsave> {
    let mut x = kvm_xsave::default();
    if bytes.len() != x.region.len() * 4 {
        return Err(BackendError::InvalidState);
    }
    let (chunks, remainder) = bytes.as_chunks::<4>();
    debug_assert!(remainder.is_empty());
    for (word, chunk) in x.region.iter_mut().zip(chunks) {
        *word = u32::from_le_bytes(*chunk);
    }
    Ok(x)
}

#[cfg(test)]
mod tests;
