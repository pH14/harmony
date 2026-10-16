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
        bringup::boot(
            backend,
            &image_loader::wrap_image(&code, 0, 0xA),
            "console=ttyAMA0",
            1 << 20,
        )
        .expect("boot ARM executable restore witness")
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
            restore_memory(&mut reused, image);
            assert_eq!(reused.guest_memory().as_ptr(), address);
            step(&mut reused, expected);
        }

        let mut direct = boot(A);
        for _ in 0..ROUNDS {
            step(&mut direct, A);
        }
        for round in 0..ROUNDS {
            let (expected, image) = if round % 2 == 0 {
                (B, &ram_b)
            } else {
                (A, &ram_a)
            };
            restore_memory(&mut direct, image);
            step(&mut direct, expected);
        }
        println!(
            "ARM_EXECUTABLE_RESTORE cold=pass capture_only=pass fresh=pass reused={ROUNDS} memory_only={ROUNDS} stale=0"
        );
    }
}
