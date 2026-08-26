// SPDX-License-Identifier: AGPL-3.0-or-later
//! Empirical probe for the Apple Silicon Hypervisor.framework surface used by
//! the prescriptive V-time bring-up backend.

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
mod arm64 {
    use core::ffi::c_void;
    use std::alloc::{Layout, alloc_zeroed, dealloc};
    use std::ptr::{self, NonNull};
    use std::thread;
    use std::time::Duration;

    const PAGE_SIZE: usize = 16 * 1024;
    const HV_MEMORY_READ: u64 = 1;
    const HV_MEMORY_WRITE: u64 = 2;
    const HV_MEMORY_EXEC: u64 = 4;

    const HV_REG_X0: u32 = 0;
    const HV_REG_X1: u32 = 1;
    const HV_REG_X3: u32 = 3;
    const HV_REG_PC: u32 = 31;
    const HV_REG_CPSR: u32 = 34;

    const HV_SYS_REG_DBGBVR0_EL1: u16 = 0x8004;
    const HV_SYS_REG_DBGBCR0_EL1: u16 = 0x8005;
    const HV_SYS_REG_DBGWVR0_EL1: u16 = 0x8006;
    const HV_SYS_REG_DBGWCR0_EL1: u16 = 0x8007;
    const HV_SYS_REG_MDSCR_EL1: u16 = 0x8012;
    const HV_SYS_REG_SCTLR_EL1: u16 = 0xc080;
    const HV_SYS_REG_CPACR_EL1: u16 = 0xc082;
    const HV_SYS_REG_TTBR0_EL1: u16 = 0xc100;
    const HV_SYS_REG_TTBR1_EL1: u16 = 0xc101;
    const HV_SYS_REG_TCR_EL1: u16 = 0xc102;
    const HV_SYS_REG_SPSR_EL1: u16 = 0xc200;
    const HV_SYS_REG_ELR_EL1: u16 = 0xc201;
    const HV_SYS_REG_SP_EL0: u16 = 0xc208;
    const HV_SYS_REG_ESR_EL1: u16 = 0xc290;
    const HV_SYS_REG_FAR_EL1: u16 = 0xc300;
    const HV_SYS_REG_MAIR_EL1: u16 = 0xc510;
    const HV_SYS_REG_VBAR_EL1: u16 = 0xc600;
    const HV_SYS_REG_TPIDR_EL1: u16 = 0xc684;
    const HV_SYS_REG_CNTKCTL_EL1: u16 = 0xc708;
    const HV_SYS_REG_TPIDR_EL0: u16 = 0xde82;
    const HV_SYS_REG_CNTV_CTL_EL0: u16 = 0xdf19;
    const HV_SYS_REG_CNTV_CVAL_EL0: u16 = 0xdf1a;
    const HV_SYS_REG_SP_EL1: u16 = 0xe208;

    const HV_INTERRUPT_TYPE_IRQ: u32 = 0;
    const HV_EXIT_REASON_EXCEPTION: u32 = 1;

    const PSTATE_EL1H_MASKED: u64 = 0x3c5;
    const PSTATE_EL1H_IRQ_ENABLED: u64 = PSTATE_EL1H_MASKED & !(1 << 7);

    const INSN_HVC_0: u32 = 0xd400_0002;
    const INSN_WFI: u32 = 0xd503_207f;
    const INSN_MRS_X0_CNTVCT_EL0: u32 = 0xd53b_e040;
    const INSN_MRS_X1_PMCCNTR_EL0: u32 = 0xd53b_9d01;
    const INSN_MRS_X2_MIDR_EL1: u32 = 0xd538_0002;
    const INSN_MSR_CNTV_CVAL_EL0_X3: u32 = 0xd51b_e343;
    const INSN_LDR_X4_X5: u32 = 0xf940_00a4;

    // Stable Rust deliberately rejects the platform SIMD type in an FFI
    // signature (`simd_ffi` is nightly-only). This AAPCS64 thunk accepts the
    // bytes by pointer in X2, loads the required by-value vector argument into
    // Q0, and tail-calls Hypervisor.framework's public setter.
    core::arch::global_asm!(
        ".globl _harmony_hv_vcpu_set_simd_fp_reg",
        "_harmony_hv_vcpu_set_simd_fp_reg:",
        "ldr q0, [x2]",
        "b _hv_vcpu_set_simd_fp_reg",
    );

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct HvExitException {
        syndrome: u64,
        virtual_address: u64,
        physical_address: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct HvVcpuExit {
        reason: u32,
        _padding: u32,
        exception: HvExitException,
    }

    #[link(name = "Hypervisor", kind = "framework")]
    unsafe extern "C" {
        fn hv_vm_create(config: *mut c_void) -> i32;
        fn hv_vm_destroy() -> i32;
        fn hv_vm_map(addr: *mut c_void, ipa: u64, size: usize, flags: u64) -> i32;
        fn hv_vm_unmap(ipa: u64, size: usize) -> i32;

        fn hv_vcpu_create(vcpu: *mut u64, exit: *mut *const HvVcpuExit, config: *mut c_void)
        -> i32;
        fn hv_vcpu_destroy(vcpu: u64) -> i32;
        fn hv_vcpu_run(vcpu: u64) -> i32;
        fn hv_vcpus_exit(vcpus: *mut u64, count: u32) -> i32;
        fn hv_vcpu_get_reg(vcpu: u64, reg: u32, value: *mut u64) -> i32;
        fn hv_vcpu_set_reg(vcpu: u64, reg: u32, value: u64) -> i32;
        fn hv_vcpu_get_simd_fp_reg(vcpu: u64, reg: u32, value: *mut [u8; 16]) -> i32;
        fn harmony_hv_vcpu_set_simd_fp_reg(vcpu: u64, reg: u32, value: *const u8) -> i32;
        fn hv_vcpu_get_sys_reg(vcpu: u64, reg: u16, value: *mut u64) -> i32;
        fn hv_vcpu_set_sys_reg(vcpu: u64, reg: u16, value: u64) -> i32;
        fn hv_vcpu_get_pending_interrupt(vcpu: u64, kind: u32, pending: *mut bool) -> i32;
        fn hv_vcpu_set_pending_interrupt(vcpu: u64, kind: u32, pending: bool) -> i32;
        fn hv_vcpu_get_trap_debug_exceptions(vcpu: u64, value: *mut bool) -> i32;
        fn hv_vcpu_set_trap_debug_exceptions(vcpu: u64, value: bool) -> i32;
        fn hv_vcpu_get_trap_debug_reg_accesses(vcpu: u64, value: *mut bool) -> i32;
        fn hv_vcpu_set_trap_debug_reg_accesses(vcpu: u64, value: bool) -> i32;
        fn hv_vcpu_get_vtimer_mask(vcpu: u64, value: *mut bool) -> i32;
        fn hv_vcpu_set_vtimer_mask(vcpu: u64, value: bool) -> i32;
        fn hv_vcpu_get_vtimer_offset(vcpu: u64, value: *mut u64) -> i32;
        fn hv_vcpu_set_vtimer_offset(vcpu: u64, value: u64) -> i32;
        fn sys_icache_invalidate(start: *mut c_void, length: usize);
    }

    fn check(operation: &'static str, status: i32) -> Result<(), String> {
        if status == 0 {
            Ok(())
        } else {
            Err(format!("{operation}: HV error {:#010x}", status as u32))
        }
    }

    struct GuestPage {
        ptr: NonNull<u8>,
        layout: Layout,
        mapped: bool,
    }

    impl GuestPage {
        fn new() -> Result<Self, String> {
            let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE)
                .map_err(|_| "invalid probe page layout".to_owned())?;
            // SAFETY: `layout` is non-zero and valid; the allocation is retained
            // by this owner until after it has been unmapped from the VM.
            let raw = unsafe { alloc_zeroed(layout) };
            let ptr = NonNull::new(raw).ok_or_else(|| "probe page allocation failed".to_owned())?;
            Ok(Self {
                ptr,
                layout,
                mapped: false,
            })
        }

        fn map(&mut self) -> Result<(), String> {
            // SAFETY: this allocation is 16 KiB aligned, PAGE_SIZE long, live
            // until unmap, and not accessed by Rust while the vCPU is running.
            check("hv_vm_map", unsafe {
                hv_vm_map(
                    self.ptr.as_ptr().cast(),
                    0,
                    PAGE_SIZE,
                    HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC,
                )
            })?;
            self.mapped = true;
            Ok(())
        }

        fn clear(&mut self) {
            // SAFETY: the allocation is live and exactly PAGE_SIZE bytes; the
            // caller only mutates it between vCPU runs.
            unsafe { ptr::write_bytes(self.ptr.as_ptr(), 0, PAGE_SIZE) };
        }

        fn put(&mut self, offset: usize, instruction: u32) -> Result<(), String> {
            let end = offset
                .checked_add(size_of::<u32>())
                .ok_or_else(|| "instruction offset overflow".to_owned())?;
            if end > PAGE_SIZE || !offset.is_multiple_of(size_of::<u32>()) {
                return Err("instruction lies outside the aligned guest page".to_owned());
            }
            // SAFETY: bounds and alignment were checked and no vCPU is running.
            unsafe {
                self.ptr
                    .as_ptr()
                    .add(offset)
                    .cast::<u32>()
                    .write(instruction)
            };
            Ok(())
        }

        fn sync_instruction_cache(&mut self) {
            // SAFETY: the mapped allocation is live for PAGE_SIZE bytes. This
            // is required after rewriting code at a reused IPA on Apple
            // Silicon; without it, later probe cases may execute stale lines.
            unsafe { sys_icache_invalidate(self.ptr.as_ptr().cast(), PAGE_SIZE) };
        }
    }

    impl Drop for GuestPage {
        fn drop(&mut self) {
            if self.mapped {
                // SAFETY: this owner mapped exactly this IPA range and the vCPU
                // has already been destroyed by declaration/drop order.
                let _ = unsafe { hv_vm_unmap(0, PAGE_SIZE) };
            }
            // SAFETY: `ptr` was allocated with exactly `layout` and is live.
            unsafe { dealloc(self.ptr.as_ptr(), self.layout) };
        }
    }

    struct Vm;

    impl Vm {
        fn new() -> Result<Self, String> {
            // SAFETY: null selects the documented default VM configuration.
            check("hv_vm_create", unsafe { hv_vm_create(ptr::null_mut()) })?;
            Ok(Self)
        }
    }

    impl Drop for Vm {
        fn drop(&mut self) {
            // SAFETY: the sole vCPU and mapping are dropped before this guard.
            let _ = unsafe { hv_vm_destroy() };
        }
    }

    struct Vcpu {
        id: u64,
        exit: NonNull<HvVcpuExit>,
    }

    impl Vcpu {
        fn new() -> Result<Self, String> {
            let mut id = 0;
            let mut exit = ptr::null();
            // SAFETY: both output pointers are valid and null selects the
            // documented default vCPU configuration.
            check("hv_vcpu_create", unsafe {
                hv_vcpu_create(&mut id, &mut exit, ptr::null_mut())
            })?;
            let exit = NonNull::new(exit.cast_mut())
                .ok_or_else(|| "hv_vcpu_create returned a null exit pointer".to_owned())?;
            Ok(Self { id, exit })
        }

        fn set_reg(&self, reg: u32, value: u64) -> Result<(), String> {
            // SAFETY: `id` names the current thread's live vCPU.
            check("hv_vcpu_set_reg", unsafe {
                hv_vcpu_set_reg(self.id, reg, value)
            })
        }

        fn reg(&self, reg: u32) -> Result<u64, String> {
            let mut value = 0;
            // SAFETY: the output points to a live u64 and this is the owning thread.
            check("hv_vcpu_get_reg", unsafe {
                hv_vcpu_get_reg(self.id, reg, &mut value)
            })?;
            Ok(value)
        }

        fn set_sys(&self, reg: u16, value: u64) -> Result<(), String> {
            // SAFETY: `id` names the current thread's live vCPU.
            check("hv_vcpu_set_sys_reg", unsafe {
                hv_vcpu_set_sys_reg(self.id, reg, value)
            })
        }

        fn sys(&self, reg: u16) -> Result<u64, String> {
            let mut value = 0;
            // SAFETY: the output points to a live u64 and this is the owning thread.
            check("hv_vcpu_get_sys_reg", unsafe {
                hv_vcpu_get_sys_reg(self.id, reg, &mut value)
            })?;
            Ok(value)
        }

        fn run(&self) -> Result<HvVcpuExit, String> {
            // SAFETY: `id` is live and called on its owning thread.
            check("hv_vcpu_run", unsafe { hv_vcpu_run(self.id) })?;
            // SAFETY: Hypervisor.framework owns this stable exit page for the
            // life of the vCPU and has completed writing it before run returns.
            Ok(unsafe { *self.exit.as_ptr() })
        }

        fn reset(&self, irq_enabled: bool) -> Result<(), String> {
            self.set_reg(HV_REG_PC, 0)?;
            self.set_reg(
                HV_REG_CPSR,
                if irq_enabled {
                    PSTATE_EL1H_IRQ_ENABLED
                } else {
                    PSTATE_EL1H_MASKED
                },
            )?;
            self.set_sys(HV_SYS_REG_VBAR_EL1, 0)?;
            // SAFETY: `id` is live and called on its owning thread.
            check("clear pending IRQ", unsafe {
                hv_vcpu_set_pending_interrupt(self.id, HV_INTERRUPT_TYPE_IRQ, false)
            })
        }
    }

    impl Drop for Vcpu {
        fn drop(&mut self) {
            // SAFETY: `id` names this thread's live vCPU.
            let _ = unsafe { hv_vcpu_destroy(self.id) };
        }
    }

    fn ec(exit: HvVcpuExit) -> u64 {
        exit.exception.syndrome >> 26
    }

    fn describe_exit(exit: HvVcpuExit) -> String {
        format!(
            "reason={} ec={:#x} syndrome={:#x} va={:#x} ipa={:#x}",
            exit.reason,
            ec(exit),
            exit.exception.syndrome,
            exit.exception.virtual_address,
            exit.exception.physical_address
        )
    }

    fn probe_register_state(vcpu: &Vcpu) -> Result<(), String> {
        let mut scalar_count = 0;
        for reg in 0..=HV_REG_CPSR {
            let original = vcpu.reg(reg)?;
            vcpu.set_reg(reg, original)?;
            scalar_count += 1;
        }
        let original_x0 = vcpu.reg(HV_REG_X0)?;
        vcpu.set_reg(HV_REG_X0, 0x0123_4567_89ab_cdef)?;
        let gpr_exact = vcpu.reg(HV_REG_X0)? == 0x0123_4567_89ab_cdef;
        vcpu.set_reg(HV_REG_X0, original_x0)?;
        let original_fpcr = vcpu.reg(32)?;
        let original_fpsr = vcpu.reg(33)?;
        vcpu.set_reg(32, 0x0040_0000)?;
        vcpu.set_reg(33, 0x0800_0000)?;
        let fp_status_exact = vcpu.reg(32)? == 0x0040_0000 && vcpu.reg(33)? == 0x0800_0000;
        vcpu.set_reg(32, original_fpcr)?;
        vcpu.set_reg(33, original_fpsr)?;

        let pattern = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let mut original_q0 = [0; 16];
        // SAFETY: the output points to 16 writable bytes and Q0 is valid.
        check("hv_vcpu_get_simd_fp_reg(Q0 original)", unsafe {
            hv_vcpu_get_simd_fp_reg(vcpu.id, 0, &mut original_q0)
        })?;
        // SAFETY: Q0 is valid, `pattern` has 16 readable bytes, and the thunk
        // loads those bytes into the ABI-mandated Q0 argument register.
        check("hv_vcpu_set_simd_fp_reg(Q0)", unsafe {
            harmony_hv_vcpu_set_simd_fp_reg(vcpu.id, 0, pattern.as_ptr())
        })?;
        let mut bytes = [0; 16];
        // SAFETY: the output points to an initialized SIMD value and Q0 is valid.
        check("hv_vcpu_get_simd_fp_reg(Q0)", unsafe {
            hv_vcpu_get_simd_fp_reg(vcpu.id, 0, &mut bytes)
        })?;
        // SAFETY: Q0 is valid and `original_q0` has 16 readable bytes.
        check("hv_vcpu_set_simd_fp_reg(Q0 restore)", unsafe {
            harmony_hv_vcpu_set_simd_fp_reg(vcpu.id, 0, original_q0.as_ptr())
        })?;

        let sysregs = [
            HV_SYS_REG_SCTLR_EL1,
            HV_SYS_REG_CPACR_EL1,
            HV_SYS_REG_TTBR0_EL1,
            HV_SYS_REG_TTBR1_EL1,
            HV_SYS_REG_TCR_EL1,
            HV_SYS_REG_SPSR_EL1,
            HV_SYS_REG_ELR_EL1,
            HV_SYS_REG_SP_EL0,
            HV_SYS_REG_ESR_EL1,
            HV_SYS_REG_FAR_EL1,
            HV_SYS_REG_MAIR_EL1,
            HV_SYS_REG_VBAR_EL1,
            HV_SYS_REG_TPIDR_EL1,
            HV_SYS_REG_CNTKCTL_EL1,
            HV_SYS_REG_TPIDR_EL0,
            HV_SYS_REG_CNTV_CTL_EL0,
            HV_SYS_REG_CNTV_CVAL_EL0,
            HV_SYS_REG_SP_EL1,
        ];
        let mut sysreg_count = 0;
        for reg in sysregs {
            let original = vcpu.sys(reg)?;
            vcpu.set_sys(reg, original)?;
            sysreg_count += 1;
        }
        let original_tpidr = vcpu.sys(HV_SYS_REG_TPIDR_EL0)?;
        vcpu.set_sys(HV_SYS_REG_TPIDR_EL0, 0xfeed_face_cafe_beef)?;
        let sysreg_exact = vcpu.sys(HV_SYS_REG_TPIDR_EL0)? == 0xfeed_face_cafe_beef;
        vcpu.set_sys(HV_SYS_REG_TPIDR_EL0, original_tpidr)?;

        let mut debug_count = 0;
        for index in 0..16u16 {
            for base in [
                HV_SYS_REG_DBGBVR0_EL1,
                HV_SYS_REG_DBGBCR0_EL1,
                HV_SYS_REG_DBGWVR0_EL1,
                HV_SYS_REG_DBGWCR0_EL1,
            ] {
                let reg = base + index * 8;
                let original = vcpu.sys(reg)?;
                vcpu.set_sys(reg, original)?;
                debug_count += 1;
            }
        }
        let mdscr = vcpu.sys(HV_SYS_REG_MDSCR_EL1)?;
        vcpu.set_sys(HV_SYS_REG_MDSCR_EL1, mdscr)?;
        debug_count += 1;
        let original_dbgbvr0 = vcpu.sys(HV_SYS_REG_DBGBVR0_EL1)?;
        vcpu.set_sys(HV_SYS_REG_DBGBVR0_EL1, 0x1234_5000)?;
        let debug_reg_exact = vcpu.sys(HV_SYS_REG_DBGBVR0_EL1)? == 0x1234_5000;
        vcpu.set_sys(HV_SYS_REG_DBGBVR0_EL1, original_dbgbvr0)?;

        let mut pending = false;
        // SAFETY: the output bool is live and the vCPU is owned by this thread.
        check("set pending IRQ", unsafe {
            hv_vcpu_set_pending_interrupt(vcpu.id, HV_INTERRUPT_TYPE_IRQ, true)
        })?;
        // SAFETY: the output bool is live and the vCPU is owned by this thread.
        check("get pending IRQ", unsafe {
            hv_vcpu_get_pending_interrupt(vcpu.id, HV_INTERRUPT_TYPE_IRQ, &mut pending)
        })?;
        // SAFETY: the vCPU is live and owned by this thread.
        check("clear pending IRQ", unsafe {
            hv_vcpu_set_pending_interrupt(vcpu.id, HV_INTERRUPT_TYPE_IRQ, false)
        })?;
        let mut pending_after_clear = true;
        // SAFETY: the output bool is live and the vCPU is owned by this thread.
        check("get cleared pending IRQ", unsafe {
            hv_vcpu_get_pending_interrupt(vcpu.id, HV_INTERRUPT_TYPE_IRQ, &mut pending_after_clear)
        })?;

        let mut trap_debug_exceptions = false;
        let mut trap_debug_accesses = false;
        // SAFETY: output pointers are live and this is the owning thread.
        check("get trap debug exceptions", unsafe {
            hv_vcpu_get_trap_debug_exceptions(vcpu.id, &mut trap_debug_exceptions)
        })?;
        // SAFETY: output pointers are live and this is the owning thread.
        check("get trap debug accesses", unsafe {
            hv_vcpu_get_trap_debug_reg_accesses(vcpu.id, &mut trap_debug_accesses)
        })?;
        // SAFETY: the vCPU is live and this is the owning thread.
        check("toggle trap debug exceptions", unsafe {
            hv_vcpu_set_trap_debug_exceptions(vcpu.id, !trap_debug_exceptions)
        })?;
        let mut toggled_debug_exceptions = trap_debug_exceptions;
        // SAFETY: the output is live and this is the owning thread.
        check("read toggled trap debug exceptions", unsafe {
            hv_vcpu_get_trap_debug_exceptions(vcpu.id, &mut toggled_debug_exceptions)
        })?;
        // SAFETY: the vCPU is live and this is the owning thread.
        check("restore trap debug exceptions", unsafe {
            hv_vcpu_set_trap_debug_exceptions(vcpu.id, trap_debug_exceptions)
        })?;
        // SAFETY: the vCPU is live and this is the owning thread.
        check("toggle trap debug accesses", unsafe {
            hv_vcpu_set_trap_debug_reg_accesses(vcpu.id, !trap_debug_accesses)
        })?;
        let mut toggled_debug_accesses = trap_debug_accesses;
        // SAFETY: the output is live and this is the owning thread.
        check("read toggled trap debug accesses", unsafe {
            hv_vcpu_get_trap_debug_reg_accesses(vcpu.id, &mut toggled_debug_accesses)
        })?;
        // SAFETY: the vCPU is live and this is the owning thread.
        check("restore trap debug accesses", unsafe {
            hv_vcpu_set_trap_debug_reg_accesses(vcpu.id, trap_debug_accesses)
        })?;

        let mut timer_mask = false;
        let mut timer_offset = 0;
        // SAFETY: outputs are live and this is the owning thread.
        check("get VTimer mask", unsafe {
            hv_vcpu_get_vtimer_mask(vcpu.id, &mut timer_mask)
        })?;
        // SAFETY: the vCPU is live and this is the owning thread.
        check("set VTimer mask", unsafe {
            hv_vcpu_set_vtimer_mask(vcpu.id, timer_mask)
        })?;
        // SAFETY: outputs are live and this is the owning thread.
        check("get VTimer offset", unsafe {
            hv_vcpu_get_vtimer_offset(vcpu.id, &mut timer_offset)
        })?;
        // SAFETY: the vCPU is live and this is the owning thread.
        check("set VTimer offset", unsafe {
            hv_vcpu_set_vtimer_offset(vcpu.id, timer_offset)
        })?;
        let original_timer_ctl = vcpu.sys(HV_SYS_REG_CNTV_CTL_EL0)?;
        let original_timer_cval = vcpu.sys(HV_SYS_REG_CNTV_CVAL_EL0)?;
        vcpu.set_sys(HV_SYS_REG_CNTV_CTL_EL0, 0)?;
        let timer_pattern = original_timer_cval ^ 0x5a5a_a5a5_0123_4567;
        vcpu.set_sys(HV_SYS_REG_CNTV_CVAL_EL0, timer_pattern)?;
        let timer_regs_exact = vcpu.sys(HV_SYS_REG_CNTV_CVAL_EL0)? == timer_pattern;
        vcpu.set_sys(HV_SYS_REG_CNTV_CVAL_EL0, original_timer_cval)?;
        vcpu.set_sys(HV_SYS_REG_CNTV_CTL_EL0, original_timer_ctl)?;

        println!(
            "state.scalar: {scalar_count}/35 get+set, X0 perturbation exact={gpr_exact}, FPCR/FPSR perturbation exact={fp_status_exact}"
        );
        println!("state.simd-fp: Q0 perturbation exact={}", bytes == pattern);
        println!(
            "state.sysregs: {sysreg_count}/{} selected EL1/timer get+set, TPIDR_EL0 perturbation exact={sysreg_exact}",
            sysregs.len()
        );
        println!(
            "state.debug: {debug_count}/65 debug sysregs get+set; DBGBVR0 perturbation exact={debug_reg_exact}; trap-control toggles exact={}",
            toggled_debug_exceptions != trap_debug_exceptions
                && toggled_debug_accesses != trap_debug_accesses
        );
        println!(
            "state.pending-irq: set/read true={pending}; clear/read false={}",
            !pending_after_clear
        );
        println!("state.pending-exception: no public get/set API in the macOS 26.4 SDK");
        println!("state.exclusive-monitor: no public get/set API in the macOS 26.4 SDK");
        println!(
            "state.vtimer: CNTV_CVAL perturbation exact={timer_regs_exact}; host-offset get+set present (value={timer_offset:#x})"
        );
        Ok(())
    }

    fn run_program(
        page: &mut GuestPage,
        vcpu: &Vcpu,
        instructions: &[u32],
    ) -> Result<HvVcpuExit, String> {
        page.clear();
        for (index, instruction) in instructions.iter().copied().enumerate() {
            let offset = index
                .checked_mul(size_of::<u32>())
                .ok_or_else(|| "program offset overflow".to_owned())?;
            page.put(offset, instruction)?;
        }
        page.sync_instruction_cache();
        vcpu.reset(false)?;
        vcpu.run()
    }

    fn probe_exit_surface(page: &mut GuestPage, vcpu: &Vcpu) -> Result<(), String> {
        vcpu.set_reg(HV_REG_X0, 0)?;
        let counter = run_program(page, vcpu, &[INSN_MRS_X0_CNTVCT_EL0, INSN_HVC_0])?;
        println!(
            "trap.cntvct-el0: {}; X0={:#x}; trapped={}",
            describe_exit(counter),
            vcpu.reg(HV_REG_X0)?,
            ec(counter) == 0x18
        );

        vcpu.set_reg(HV_REG_X1, 0)?;
        let pmu = run_program(page, vcpu, &[INSN_MRS_X1_PMCCNTR_EL0, INSN_HVC_0])?;
        println!(
            "trap.pmccntr-el0: {}; X1={:#x}; trapped={}",
            describe_exit(pmu),
            vcpu.reg(HV_REG_X1)?,
            ec(pmu) == 0x18
        );

        let midr = run_program(page, vcpu, &[INSN_MRS_X2_MIDR_EL1, INSN_HVC_0])?;
        println!(
            "trap.midr-el1: {}; trapped={}",
            describe_exit(midr),
            ec(midr) == 0x18
        );

        vcpu.set_reg(HV_REG_X3, 0x1234_5678)?;
        let timer = run_program(page, vcpu, &[INSN_MSR_CNTV_CVAL_EL0_X3, INSN_HVC_0])?;
        println!(
            "trap.cntv-cval-el0: {}; trapped={}",
            describe_exit(timer),
            ec(timer) == 0x18
        );

        page.clear();
        page.put(0, INSN_LDR_X4_X5)?;
        page.put(4, INSN_HVC_0)?;
        page.sync_instruction_cache();
        vcpu.reset(false)?;
        vcpu.set_reg(5, 0x20_000)?;
        let mmio = vcpu.run()?;
        println!("exit.unmapped-mmio: {}", describe_exit(mmio));

        page.clear();
        page.put(0, INSN_WFI)?;
        page.put(4, INSN_HVC_0)?;
        page.put(0x280, INSN_HVC_0)?;
        page.sync_instruction_cache();
        vcpu.reset(true)?;
        // SAFETY: the vCPU is live and this is the owning thread. The framework
        // documents that the pending level is applied on the following entry.
        check("arm pending IRQ", unsafe {
            hv_vcpu_set_pending_interrupt(vcpu.id, HV_INTERRUPT_TYPE_IRQ, true)
        })?;
        let injected = vcpu.run()?;
        println!(
            "interrupt.pre-entry: {}; PC={:#x}; ELR_EL1={:#x}",
            describe_exit(injected),
            vcpu.reg(HV_REG_PC)?,
            vcpu.sys(HV_SYS_REG_ELR_EL1)?
        );

        page.clear();
        page.put(0, INSN_WFI)?;
        page.put(4, INSN_HVC_0)?;
        page.sync_instruction_cache();
        vcpu.reset(false)?;
        let id = vcpu.id;
        let cancel = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let mut target = id;
            // SAFETY: cancellation may be requested from another thread; the
            // copied ID remains valid until the owning run returns and joins us.
            unsafe { hv_vcpus_exit(&mut target, 1) }
        });
        let wfi = vcpu.run()?;
        let cancel_status = cancel
            .join()
            .map_err(|_| "WFI cancellation helper panicked".to_owned())?;
        check("hv_vcpus_exit watchdog", cancel_status)?;
        println!(
            "exit.wfi: {}; PC={:#x}; recognizable-wfx-exception={}",
            describe_exit(wfi),
            vcpu.reg(HV_REG_PC)?,
            wfi.reason == HV_EXIT_REASON_EXCEPTION && ec(wfi) == 0x1
        );
        Ok(())
    }

    pub fn run() -> Result<(), String> {
        println!("HVF prescriptive V-time probe v1");
        println!("host: {}-{}", std::env::consts::OS, std::env::consts::ARCH);
        println!("sdk-assumption: macOS 26.4.1 SDK headers");

        // Declaration order is reverse drop order: vCPU, then mapping, then VM.
        let _vm = Vm::new()?;
        let mut page = GuestPage::new()?;
        page.map()?;
        let vcpu = Vcpu::new()?;

        probe_register_state(&vcpu)?;
        probe_exit_surface(&mut page, &vcpu)?;
        Ok(())
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
fn main() {
    if let Err(error) = arm64::run() {
        eprintln!("hvf probe failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64", not(miri))))]
fn main() {
    eprintln!("hvf_probe requires an Apple Silicon macOS host outside Miri");
    std::process::exit(2);
}
