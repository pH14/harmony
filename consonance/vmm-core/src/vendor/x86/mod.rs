// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **x86-64 vendor** (`docs/ARCH-BOUNDARY.md` §B): everything in the
//! deterministic VMM that names the x86 ISA — the CPU/MSR contract and its
//! installed policy ([`contract`]), the exit dispatch and dispositions
//! ([`dispatch`]), the boot loaders and entry state ([`multiboot`],
//! [`linux_loader`], [`entry`]), the interrupt fabric and platform device models
//! ([`devices`] + the `lapic` crate), the host-homogeneity probe
//! ([`hostassert`]), the retired-branch work counter (`work_perf`), and the
//! `vm_state` record set ([`records`]).
//!
//! The engine ([`crate::vmm`]) reaches all of it through [`Vendor`] alone. x86
//! is the sole vendor today; an ARM vendor is a sibling module here (the §D
//! pre-build wave), not an edit to the engine.

// The x86 **boot composition root** — the one place the concrete
// `(Backend impl, Arch vendor)` pair is named (R-Backend; the §B composition-root
// discipline). A *vendor* module, not an engine one: it installs the x86
// CPU-contract policy, runs the Multiboot v1 / Linux bzImage loaders, and builds
// the x86 entry state.
pub mod bringup;
pub mod contract;
pub mod devices;
pub mod dispatch;
pub mod entry;
pub mod hostassert;
pub mod linux_loader;
pub mod multiboot;
pub mod records;

// The box-only `perf_event` work counter (the V-time work source): the x86
// retired-conditional-branch event (`0x1c4`) behind the arch-neutral `WorkSource`
// seam. Gated on the arch as well as the OS — the raw event number is Intel's, so
// this is x86-64-only, not merely Linux-only.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod work_perf;

use control_proto::RegsView;
use vm_state::VmState;
use vmm_backend::{Backend, Gpa, X86, X86Exit};

pub use dispatch::{X86Devices, contract_vclock_config};

use crate::vendor::x86::linux_loader::{LAPIC_MMIO_PAGE, LAPIC_MMIO_PAGE_LEN};
use crate::vendor::{InterruptReject, Vendor};
use crate::vmm::{Step, Vmm, VmmError};

impl Vendor for X86 {
    type Devices = X86Devices;
    type RestorePrep = dispatch::X86RestorePrep;
    type Snapshot = VmState;

    fn new_devices() -> Self::Devices {
        X86Devices::new()
    }

    fn mmio_holes() -> &'static [(u64, u64)] {
        // The xAPIC page: `KvmBackend::map_memory` splits the RAM memslots around
        // exactly this range (`split_around_hole`, vmm-backend), so it is device
        // MMIO, never RAM — the one hole the x86 machine model punches today.
        &[(LAPIC_MMIO_PAGE, LAPIC_MMIO_PAGE_LEN)]
    }

    fn dispatch_arch<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        exit: X86Exit,
    ) -> Result<Step, VmmError> {
        // Exhaustive over `X86Exit` — no wildcard arm (default-deny stays
        // structural; `docs/ARCH-BOUNDARY.md` §A).
        match exit {
            X86Exit::Io {
                port,
                size,
                write: Some(v),
            } => vmm.dispatch_out(port, size, v),
            X86Exit::Io {
                port,
                size,
                write: None,
            } => vmm.dispatch_in(port, size),
            X86Exit::Rdmsr { index } => vmm.dispatch_rdmsr(index),
            X86Exit::Wrmsr { index, value } => vmm.dispatch_wrmsr(index, value),
            X86Exit::Cpuid { leaf, subleaf } => vmm.dispatch_cpuid(leaf, subleaf),
            // Determinism-complete path: RDTSC/RDTSCP → the V-time guest clock;
            // RDRAND/RDSEED → the seeded stream. Computed above the trait; the
            // backend only surfaced + will complete the exit. Unwired (stock KVM /
            // M1/M2) is a loud contract violation, never a host-derived value.
            X86Exit::Rdtsc | X86Exit::Rdtscp => vmm.complete_tsc(),
            X86Exit::Rdrand { width } | X86Exit::Rdseed { width } => vmm.complete_rng(width),
        }
    }

    fn dispatch_mmio<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        gpa: Gpa,
        size: u8,
        write: Option<u64>,
    ) -> Result<Step, VmmError> {
        vmm.dispatch_mmio(gpa, size, write)
    }

    fn service_pending_irqs<B: Backend<A = Self>>(vmm: &mut Vmm<B>) -> Result<(), VmmError> {
        vmm.service_pending_irqs()
    }

    fn complete_irq_delivery<B: Backend<A = Self>>(vmm: &mut Vmm<B>) {
        vmm.complete_irq_delivery();
    }

    fn guest_interruptible<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Result<bool, VmmError> {
        // `RFLAGS.IF` — the guest's own "I can take an interrupt" signal.
        Ok(vmm.backend().save()?.regs.rflags & dispatch::RFLAGS_IF != 0)
    }

    fn pending_deliverable_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
    ) -> Result<bool, VmmError> {
        // `peek_interrupt` does the vector-validity + TPR/PPR arbitration.
        Ok(vmm
            .devices()
            .lapic
            .as_ref()
            .is_some_and(|l| l.peek_interrupt().is_some()))
    }

    fn next_timer_deadline_vns<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Option<u64> {
        vmm.devices().lapic.as_ref()?.next_timer_deadline()
    }

    fn deliverable_timer_deadline_vns<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Option<u64> {
        // An *armed* timer can still be **undeliverable** — a reserved vector
        // (`< 16`), or masked by TPR/PPR — in which case it fires into the IRR but
        // never injects, so a one-shot leaves no future wake. Such a timer is no
        // wake at all.
        let lapic = vmm.devices().lapic.as_ref()?;
        lapic
            .next_timer_deadline()
            .filter(|_| lapic.armed_timer_deliverable())
    }

    fn check_wire_interrupt<B: Backend<A = Self>>(
        vmm: &Vmm<B>,
        vector: u32,
    ) -> Result<(), InterruptReject> {
        // No userspace xAPIC ⇒ no IRQ arbitration path to assert a vector through.
        if vmm.devices().lapic.is_none() {
            return Err(InterruptReject::NoFabric);
        }
        // The xAPIC's identity space is 8 bits wide.
        let Ok(vector) = u8::try_from(vector) else {
            return Err(InterruptReject::OutOfRange);
        };
        // Vectors 0..16 are architecturally reserved on x86 and the LAPIC will not
        // raise them. (An ARM vendor would NOT reject its 0..16 — those are SGIs,
        // and they deliver.)
        if vector < 16 {
            return Err(InterruptReject::Reserved { vector });
        }
        Ok(())
    }

    fn inject_wire_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        vector: u32,
    ) -> Result<(), VmmError> {
        vmm.inject_host_interrupt(vector)
    }

    fn has_pending_guest_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
    ) -> Result<bool, VmmError> {
        vmm.has_pending_guest_interrupt_x86()
    }

    fn serial_capture(devices: &Self::Devices) -> &[u8] {
        devices.uart.capture()
    }

    fn inject_serial_input(devices: &mut Self::Devices, bytes: &[u8]) {
        devices.uart.inject_input(bytes);
    }

    fn encode_vcpu_chunk(vcpu: &vmm_backend::VcpuState) -> Vec<u8> {
        dispatch::encode_vcpu_state(vcpu)
    }

    fn encode_device_state(devices: &Self::Devices) -> Vec<u8> {
        // The UART register shadows (offsets 0..=7) + the latched `LCR.DLAB`
        // window — the device's residual state, so two runs that drive the UART
        // into a different register/DLAB configuration hash differently even with
        // byte-identical serial output. (The engine appends the terminal-reason
        // bytes after this.)
        let mut v = Vec::new();
        v.extend_from_slice(devices.uart.shadow_regs());
        v.push(u8::from(devices.uart.dlab()));
        v
    }

    fn hash_device_chunks(devices: &Self::Devices, out: &mut Vec<u8>) {
        // The xAPIC chunk is present **only** on the Linux boot path (`lapic`
        // wired); M1/M2/corpus emit none, so their hash is byte-for-byte
        // unchanged. It captures the register file + timer bookkeeping that
        // governs future interrupt delivery.
        if let Some(lapic) = &devices.lapic {
            crate::vmm::put_chunk(
                out,
                b"LAPC",
                &dispatch::encode_lapic_state(&lapic.snapshot()),
            );
        }
        // Legacy-platform state (the PCI CONFIG_ADDRESS latch + the 8259 master/
        // slave IMR) — Linux path only. The IMR governs which IRQ lines deliver,
        // so two same-seed runs that leave it different hash differently.
        if let Some(legacy) = &devices.legacy {
            let mut legy = legacy.config_address().to_le_bytes().to_vec();
            legy.extend_from_slice(&legacy.pic_imr());
            crate::vmm::put_chunk(out, b"LEGY", &legy);
        }
    }

    fn regs_view(vcpu: &vmm_backend::VcpuState) -> RegsView {
        // The GPRs and segment selectors go in the view's canonical order
        // (`rax rbx rcx rdx rsi rdi rbp rsp r8..r15` — note **rbp before rsp** —
        // and `cs ss ds es fs gs`). The engine fills the `Moment`/`vtime` half.
        let r = &vcpu.regs;
        RegsView {
            version: RegsView::VERSION,
            gpr: [
                r.rax, r.rbx, r.rcx, r.rdx, r.rsi, r.rdi, r.rbp, r.rsp, r.r8, r.r9, r.r10, r.r11,
                r.r12, r.r13, r.r14, r.r15,
            ],
            rip: r.rip,
            rflags: r.rflags,
            seg: [
                vcpu.sregs.cs.selector,
                vcpu.sregs.ss.selector,
                vcpu.sregs.ds.selector,
                vcpu.sregs.es.selector,
                vcpu.sregs.fs.selector,
                vcpu.sregs.gs.selector,
            ],
            cr0: vcpu.sregs.cr0,
            cr3: vcpu.sregs.cr3,
            cr4: vcpu.sregs.cr4,
            moment: control_proto::Moment(0),
            vtime: 0,
        }
    }

    fn vcpu_components(vcpu: &vmm_backend::VcpuState, out: &mut Vec<(&'static str, [u8; 32])>) {
        dispatch::vcpu_components(vcpu, out);
    }

    fn vcpu_has_inflight_injection(vcpu: &vmm_backend::VcpuState) -> bool {
        records::has_inflight_injection(&vcpu.events)
    }

    fn vcpu_has_active_injection(vcpu: &vmm_backend::VcpuState) -> bool {
        records::has_active_event_injection(&vcpu.events)
    }

    fn check_sealable_vcpu(vcpu: &vmm_backend::VcpuState) -> Result<(), VmmError> {
        match records::unrepresentable_state(vcpu) {
            Some(reason) => Err(VmmError::ContractViolation(format!(
                "save_vm_state: {reason}"
            ))),
            None => Ok(()),
        }
    }

    fn build_vm_state<B: Backend<A = Self>>(
        vmm: &Vmm<B>,
        vcpu: &vmm_backend::VcpuState,
    ) -> VmState {
        vmm.build_vm_state(vcpu)
    }

    fn validate_restore<B: Backend<A = Self>>(
        vmm: &Vmm<B>,
        s: &VmState,
    ) -> Result<(vmm_backend::VcpuState, u64, Self::RestorePrep), VmmError> {
        vmm.validate_restore_x86(s)
    }

    fn commit_restore<B: Backend<A = Self>>(vmm: &mut Vmm<B>, prep: Self::RestorePrep) {
        vmm.commit_restore_x86(prep);
    }
}
