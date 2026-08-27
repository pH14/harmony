// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live retained-state round-trip oracle for the Apple Silicon HVF backend.

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri), not(kani)))]
fn main() -> std::process::ExitCode {
    use vmm_backend::{Arm64Policy, Arm64VcpuState, Backend, HvfBackend};

    type Perturbation = (&'static str, fn(&mut Arm64VcpuState));

    fn check_class(
        backend: &mut HvfBackend,
        baseline: &Arm64VcpuState,
        name: &str,
        perturb: fn(&mut Arm64VcpuState),
    ) -> vmm_backend::Result<()> {
        let mut expected = *baseline;
        perturb(&mut expected);
        backend.restore(&expected)?;
        let observed = backend.save()?;
        if observed != expected {
            return Err(vmm_backend::BackendError::Internal(
                "HVF retained-state perturbation did not round-trip exactly",
            ));
        }
        backend.restore(baseline)?;
        if backend.save()? != *baseline {
            return Err(vmm_backend::BackendError::Internal(
                "HVF baseline did not restore after retained-state perturbation",
            ));
        }
        println!("HVF_STATE_CLASS_OK class={name}");
        Ok(())
    }

    let mut backend = match HvfBackend::new() {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("HVF state-oracle composition failed: {error:?}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if let Err(error) = backend.set_policy(&Arm64Policy::default()) {
        eprintln!("HVF state-oracle policy failed: {error:?}");
        return std::process::ExitCode::FAILURE;
    }
    let baseline = match backend.save() {
        Ok(state) => state,
        Err(error) => {
            eprintln!("HVF baseline save failed: {error:?}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let cases: [Perturbation; 6] = [
        ("general", |state| state.core.x[0] ^= 0x1122_3344),
        ("simd-fp", |state| {
            state.simd_fp.q[0] = [0x5a; 16];
            state.simd_fp.fpcr ^= 1 << 22;
        }),
        ("sysregs", |state| state.sysregs.tpidr_el0 ^= 0x5566_7788),
        ("debug", |state| {
            state.debug.breakpoint_value[0] ^= 0x1000;
            state.debug.trap_debug_exceptions = !state.debug.trap_debug_exceptions;
            state.debug.trap_debug_reg_accesses = !state.debug.trap_debug_reg_accesses;
        }),
        ("vtimer", |state| {
            state.vtimer.cntv_cval_el0 ^= 0x1234;
            state.vtimer.offset ^= 0x4321;
            state.vtimer.masked = !state.vtimer.masked;
        }),
        ("pending-interrupts", |state| {
            state.interrupts.irq = !state.interrupts.irq;
            state.interrupts.fiq = !state.interrupts.fiq;
        }),
    ];
    for (name, perturb) in cases {
        if let Err(error) = check_class(&mut backend, &baseline, name, perturb) {
            eprintln!("HVF_STATE_CLASS_FAIL class={name} error={error:?}");
            return std::process::ExitCode::FAILURE;
        }
    }
    println!("HVF_STATE_ROUNDTRIP_OK classes=6 baseline_restores=6");
    std::process::ExitCode::SUCCESS
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64", not(miri), not(kani))))]
fn main() -> std::process::ExitCode {
    eprintln!("hvf_state_oracle requires an Apple Silicon macOS host outside Miri/Kani");
    std::process::ExitCode::from(2)
}
