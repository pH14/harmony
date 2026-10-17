// SPDX-License-Identifier: AGPL-3.0-or-later

fn observed_byte(output: &[u8], before: usize, expected: u8) -> Result<(), String> {
    if output.len() != before + 1 || output[before] != expected {
        return Err(format!(
            "expected one byte {expected:#04x} after {before}, got {output:?}"
        ));
    }
    Ok(())
}

fn changed_pages(before: &[u8], after: &[u8]) -> Vec<usize> {
    assert_eq!(before.len(), after.len());
    before
        .chunks(4096)
        .zip(after.chunks(4096))
        .enumerate()
        .filter_map(|(page, (a, b))| (a != b).then_some(page))
        .collect()
}

#[test]
fn observation_oracle_rejects_stale_missing_and_extra_output() {
    assert!(observed_byte(b"AB", 1, b'B').is_ok());
    for output in [b"AA".as_slice(), b"A", b"ABB"] {
        assert!(observed_byte(output, 1, b'B').is_err());
    }
    let a = vec![0; 8192];
    let mut b = a.clone();
    b[64] = 1;
    assert_eq!(changed_pages(&a, &b), [0]);
    b[4096] = 1;
    assert_eq!(changed_pages(&a, &b), [0, 1]);
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
mod live {
    use super::{changed_pages, observed_byte};
    use vmm_backend::{Arm64KvmBackend, LiveKvm};
    use vmm_core::vendor::arm64::{bringup, image_loader};
    use vmm_core::vmm::{Step, Vmm};

    use rustix::thread::{CpuSet, sched_getaffinity, sched_getcpu, sched_setaffinity};

    struct PinnedCpu {
        previous: CpuSet,
        cpu: usize,
    }

    impl PinnedCpu {
        fn current() -> Self {
            let previous = sched_getaffinity(None).expect("read qualification thread affinity");
            let cpu = sched_getcpu();
            assert!(cpu < CpuSet::MAX_CPU && previous.is_set(cpu));
            let mut selected = CpuSet::new();
            selected.set(cpu);
            sched_setaffinity(None, &selected).expect("pin qualification thread");
            let guard = Self { previous, cpu };
            assert_eq!(sched_getaffinity(None).unwrap(), selected);
            guard.verify();
            let root = format!("/sys/devices/system/cpu/cpu{cpu}/regs/identification");
            let read = |name| {
                std::fs::read_to_string(format!("{root}/{name}"))
                    .map(|value| value.trim().to_owned())
                    .unwrap_or_else(|_| "unavailable".into())
            };
            println!(
                "ARM_EXECUTABLE_CACHE cpu={cpu} midr_el1={} host_id_aa64mmfr2_el1={} stage2_fwb=unmeasured host_ctr_el0=unmeasured",
                read("midr_el1"),
                read("id_aa64mmfr2_el1")
            );
            guard
        }

        fn verify(&self) {
            assert_eq!(sched_getcpu(), self.cpu, "qualification thread migrated");
        }
    }

    impl Drop for PinnedCpu {
        fn drop(&mut self) {
            if let Err(error) = sched_setaffinity(None, &self.previous) {
                eprintln!("failed to restore qualification thread affinity: {error}");
                assert!(
                    std::thread::panicking(),
                    "could not restore thread affinity"
                );
            }
        }
    }

    const INSTRUCTION_CACHE: u64 = 1 << 12;

    fn assert_instruction_cache(machine: &mut Machine) {
        let state = machine
            .vcpu_record()
            .expect("read guest instruction-cache control");
        assert_ne!(state.sysregs.sctlr_el1 & INSTRUCTION_CACHE, 0);
        assert_eq!(state.sysregs.sctlr_el1 & 1, 0);
        println!(
            "ARM_EXECUTABLE_CACHE guest_sctlr_el1={:#x} instruction_cache=enabled stage1_mmu=disabled",
            state.sysregs.sctlr_el1
        );
    }

    type Machine = Vmm<Arm64KvmBackend<LiveKvm>>;
    const ROUNDS: usize = 16;
    const A: u8 = b'A';
    const B: u8 = b'B';

    fn boot(value: u8) -> Machine {
        let words = [
            0xd280_0000 | (u32::from(value) << 5),
            0xd2a1_2001,
            0xb900_0020,
            0x17ff_fffd,
        ];
        let code = words
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let backend = Arm64KvmBackend::new(
            LiveKvm::new().expect("qualification requires accessible ARM KVM"),
        );
        let mut machine = bringup::boot(
            backend,
            &image_loader::wrap_image(&code, 0, 0xA),
            "console=ttyAMA0",
            1 << 20,
        )
        .expect("boot ARM executable restore witness");
        let mut initial = machine
            .save_vm_state()
            .expect("capture entry cache control");
        initial.sysregs.sctlr_el1 |= INSTRUCTION_CACHE;
        machine
            .restore_vm_state(&initial)
            .expect("enable guest instruction cache before entry");
        assert_instruction_cache(&mut machine);
        machine
    }

    fn step(machine: &mut Machine, expected: u8) {
        let before = machine.serial_output().len();
        assert_eq!(machine.step().expect("guest observation"), Step::Continued);
        observed_byte(machine.serial_output(), before, expected).unwrap();
    }

    fn restore_memory(machine: &mut Machine, image: &[u8]) {
        let address = machine.guest_memory().as_ptr();
        assert_eq!(changed_pages(machine.guest_memory(), image), [0]);
        machine
            .restore_guest_memory(image)
            .expect("public RAM restore");
        assert_eq!(machine.guest_memory(), image);
        assert_eq!(machine.guest_memory().as_ptr(), address);
    }

    #[test]
    #[ignore = "live ARM KVM; registered in HOST_TESTS"]
    fn executable_ram_restores_match_guest_continuations() {
        let pinned = PinnedCpu::current();
        let mut cold_a = boot(A);
        step(&mut cold_a, A);
        let mut uninterrupted = boot(B);
        let mut captured = boot(B);
        for _ in 0..=ROUNDS {
            captured.save_vm_state().expect("capture neutrality");
            step(&mut captured, B);
            step(&mut uninterrupted, B);
            assert_eq!(captured.serial_output(), uninterrupted.serial_output());
            assert_eq!(captured.guest_memory(), uninterrupted.guest_memory());
        }

        let mut reused = boot(A);
        let initial = reused.save_vm_state().expect("initial snapshot");
        let ram_a = reused.guest_memory().to_vec();
        let mut ram_b = ram_a.clone();
        ram_b[64..68].copy_from_slice(&(0xd280_0000 | (u32::from(B) << 5)).to_le_bytes());
        assert_eq!(changed_pages(&ram_a, &ram_b), [0]);
        let mut fresh = boot(A);
        fresh
            .restore_vm_state(&initial)
            .expect("fresh state restore");
        restore_memory(&mut fresh, &ram_b);
        step(&mut fresh, B);
        assert_eq!(fresh.serial_output(), &uninterrupted.serial_output()[..1]);

        for _ in 0..ROUNDS {
            step(&mut reused, A);
        }
        assert_instruction_cache(&mut reused);
        let address = reused.guest_memory().as_ptr();
        for round in 0..ROUNDS {
            let (expected, image) = if round % 2 == 0 {
                (B, &ram_b)
            } else {
                (A, &ram_a)
            };
            reused
                .restore_vm_state(&initial)
                .expect("reused state restore");
            pinned.verify();
            restore_memory(&mut reused, image);
            assert_eq!(reused.guest_memory().as_ptr(), address);
            step(&mut reused, expected);
        }

        let mut direct = boot(A);
        for _ in 0..ROUNDS {
            step(&mut direct, A);
        }
        assert_instruction_cache(&mut direct);
        for round in 0..ROUNDS {
            let (expected, image) = if round % 2 == 0 {
                (B, &ram_b)
            } else {
                (A, &ram_a)
            };
            pinned.verify();
            restore_memory(&mut direct, image);
            step(&mut direct, expected);
        }
        pinned.verify();
        assert_instruction_cache(&mut reused);
        assert_instruction_cache(&mut direct);
        println!(
            "ARM_EXECUTABLE_RESTORE cold=pass capture_only=pass fresh=pass reused={ROUNDS} memory_only={ROUNDS} stale=0"
        );
    }
}
