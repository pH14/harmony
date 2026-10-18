// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vmm_backend::{Backend, BackendError, X86};
use vmm_core::vendor::x86::bringup::{
    boot_linux_stock_virtual_time, compose_stock_virtual_time_restore_target,
};
use vmm_core::vmm::{Step, Vmm, VmmError};

type Machine = Vmm<Box<dyn Backend<A = X86>>>;
const RAM: usize = 256 << 20;
const SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic hpet=disable noxsaveopt noxsaves";

fn root() -> PathBuf {
    std::env::var_os("X3_ARTIFACTS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../harmony-linux/build"),
        PathBuf::from,
    )
}

fn boot() -> Machine {
    boot_linux_stock_virtual_time(
        &std::fs::read(root().join("bzImage")).expect("retained Linux kernel"),
        &std::fs::read(root().join("initramfs-continuation.cpio.gz"))
            .expect("owned continuation initramfs"),
        RAM,
        CMDLINE,
        SEED,
    )
    .expect("boot composed Linux")
}

fn until(vmm: &mut Machine, marker: &[u8], pressure: bool) {
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();
    let mut noise = vec![0u8; if pressure { 32 << 20 } else { 0 }];
    for steps in 0..50_000_000u64 {
        if vmm
            .serial_output()
            .windows(marker.len())
            .any(|v| v == marker)
        {
            return;
        }
        if pressure && steps % 1024 == 0 {
            for byte in noise.iter_mut().step_by(4096) {
                *byte = byte.wrapping_add(1);
            }
            std::hint::black_box(&noise);
        }
        match vmm.step() {
            Ok(Step::Continued) => {}
            result => panic!(
                "incomplete continuation at step {steps}: {result:?}; UART={}",
                String::from_utf8_lossy(vmm.serial_output())
            ),
        }
        assert!(
            start.elapsed() < Duration::from_secs(300),
            "incomplete: external wall budget"
        );
    }
    panic!("incomplete: guest step budget");
}

fn observation(vmm: &mut Machine, pressure: bool) -> String {
    vmm.inject_serial_input(b"A\n");
    until(vmm, b"X3_DONE\r\n", pressure);
    let serial = String::from_utf8_lossy(vmm.serial_output());
    let records: Vec<_> = serial
        .lines()
        .filter(|line| line.starts_with("X3_RESULT "))
        .collect();
    assert_eq!(records.len(), 1, "one independent guest observation");
    let words: Vec<_> = records[0].split_whitespace().collect();
    assert_eq!(words.len(), 8);
    for word in &words[1..] {
        assert_eq!(word.len(), 16);
        u64::from_str_radix(word, 16).expect("guest hexadecimal readback");
    }
    records[0].to_owned()
}

struct Cut {
    memory: Vec<u8>,
    state: vm_state::VmState,
    host_class: Option<vmm_backend::HostCoreClass>,
}

impl Cut {
    fn capture(vmm: &mut Machine) -> Self {
        vmm.prepare_snapshot()
            .expect("retire the pending hardware completion before capture");
        let state = vmm
            .save_vm_state()
            .expect("completed-boundary Linux capture");
        Self {
            memory: vmm.guest_memory().to_vec(),
            state,
            host_class: vmm.vcpu_record().unwrap().host_core_class,
        }
    }

    fn fresh(&self) -> Result<Machine, VmmError> {
        let mapping = snapshot_store::Mapping::anonymous(self.memory.len());
        let mut vmm = compose_stock_virtual_time_restore_target(mapping, SEED)?;
        vmm.restore_snapshot(&self.memory, &self.state)?;
        Ok(vmm)
    }

    fn export(&self, directory: &Path, expected: &str) {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(directory.join("memory.bin"), &self.memory).unwrap();
        std::fs::write(directory.join("state.bin"), self.state.encode().unwrap()).unwrap();
        std::fs::write(directory.join("observation.txt"), expected).unwrap();
        std::fs::write(
            directory.join("host-class.txt"),
            format!("{:?}", self.host_class),
        )
        .unwrap();
    }
}

fn import(directory: &Path) {
    let memory = std::fs::read(directory.join("memory.bin")).unwrap();
    assert_eq!(memory.len(), RAM);
    let state =
        vm_state::VmState::decode(&std::fs::read(directory.join("state.bin")).unwrap()).unwrap();
    let expected = std::fs::read_to_string(directory.join("observation.txt")).unwrap();
    let mut vmm =
        compose_stock_virtual_time_restore_target(snapshot_store::Mapping::anonymous(RAM), SEED)
            .unwrap();
    let before_cpu = vmm.vcpu_record().unwrap();
    let before_ram = blake3::hash(vmm.guest_memory());
    let before_time = vmm.effective_vns();
    let source_class = std::fs::read_to_string(directory.join("host-class.txt")).unwrap();
    assert!(
        ["None", "Some(Performance)", "Some(Efficiency)"].contains(&source_class.as_str()),
        "known retained source core class"
    );
    println!("X3_SOURCE_CLASS={source_class}");
    println!("X3_TARGET_CLASS={:?}", before_cpu.host_core_class);
    match vmm.restore_snapshot(&memory, &state) {
        Ok(()) => {
            assert_eq!(
                source_class,
                format!("{:?}", before_cpu.host_core_class),
                "different core classes must refuse admission"
            );
            let observed = observation(&mut vmm, true);
            assert_eq!(observed, expected, "imported continuation guest readback");
            println!("X3_OBSERVATION={observed}");
            println!("X3_IMPORT=matched");
        }
        Err(VmmError::Snapshot(vmm_core::snapshot::SnapshotError::ContractMismatch))
            if source_class != format!("{:?}", before_cpu.host_core_class) =>
        {
            assert_eq!(vmm.vcpu_record().unwrap(), before_cpu);
            assert_eq!(blake3::hash(vmm.guest_memory()), before_ram);
            assert_eq!(vmm.effective_vns(), before_time);
            assert!(vmm.serial_output().is_empty());
            let mut own_source = boot();
            until(&mut own_source, b"X3_READY\r\n", false);
            let own_cut = Cut::capture(&mut own_source);
            let own_expected = observation(&mut own_source, false);
            vmm.restore_snapshot(&own_cut.memory, &own_cut.state)
                .expect("refused target must remain reusable for an admitted local snapshot");
            assert_eq!(observation(&mut vmm, false), own_expected);
            println!("X3_IMPORT=refused-host-core-class-unchanged");
        }
        Err(error) => panic!("unqualified import refusal: {error:?}"),
    }
}

#[test]
#[ignore = "live Linux KVM; requires retained kernel and continuation initramfs"]
fn composed_linux_snapshot_continuation() {
    if let Some(directory) = std::env::var_os("X3_IMPORT") {
        import(Path::new(&directory));
        return;
    }
    let mut uninterrupted = boot();
    until(&mut uninterrupted, b"X3_READY\r\n", false);
    let expected = observation(&mut uninterrupted, false);
    let fields: Vec<_> = expected.split_whitespace().collect();
    let hash = (0..4096u64).fold(0xcbf29ce484222325u64, |hash, i| {
        (hash ^ u64::from((i * 17 + 31) as u8)).wrapping_mul(0x100000001b3)
    });
    assert_eq!(fields[1], format!("{hash:016x}"));
    assert_eq!(
        &fields[2..5],
        &["3141592653589793", "2384626433832795", "400921fb54442d18"]
    );
    drop(uninterrupted);

    let mut source = boot();
    until(&mut source, b"X3_READY\r\n", false);
    let cut = Cut::capture(&mut source);
    assert_eq!(
        observation(&mut source, false),
        expected,
        "capture must be neutral"
    );
    drop(source);
    assert_eq!(
        observation(&mut cut.fresh().unwrap(), false),
        expected,
        "fresh restore"
    );
    assert_eq!(
        observation(&mut cut.fresh().unwrap(), true),
        expected,
        "host cache and pressure variation"
    );

    let mut reused = cut.fresh().unwrap();
    reused.inject_serial_input(b"B\n");
    until(&mut reused, b"X3_DETOUR\r\n", false);
    for steps in 0..1_000_000 {
        assert_eq!(
            reused.step().unwrap(),
            Step::Continued,
            "detour must remain runnable"
        );
        if reused
            .virtual_time_trace()
            .unwrap()
            .raw_log()
            .last()
            .unwrap()
            .reason
            == vmm_backend::ExitReason::Idle
        {
            break;
        }
        assert_ne!(
            steps, 999_999,
            "detour must reach a completion-free boundary"
        );
    }
    let before = Cut::capture(&mut reused);
    match reused.restore_snapshot(&cut.memory, &cut.state) {
        Ok(()) => {
            assert_eq!(
                observation(&mut reused, true),
                expected,
                "reused restore after opposite-state detour"
            );
            println!("X3_REUSED=matched");
        }
        Err(VmmError::ContractViolation(message))
            if message
                == format!(
                    "restore_vm_state: backend rejected snapshot shape: {}",
                    BackendError::AmdReusedVcpuRestoreUnsafe
                ) =>
        {
            assert_eq!(Cut::capture(&mut reused).state, before.state);
            assert_eq!(reused.guest_memory(), before.memory);
            assert_ne!(
                observation(&mut reused, false),
                expected,
                "refusal leaves detoured guest usable"
            );
            println!("X3_REUSED=refused-amd-unchanged-usable");
        }
        Err(error) => panic!("unqualified reused restore refusal: {error:?}"),
    }
    drop(reused);

    let mut wrong = Cut {
        memory: cut.memory.clone(),
        state: cut.state.clone(),
        host_class: cut.host_class,
    };
    let pattern: Vec<_> = (0..4096).map(|i| (i * 17 + 31) as u8).collect();
    let offsets: Vec<_> = wrong
        .memory
        .windows(pattern.len())
        .enumerate()
        .filter_map(|(i, v)| (v == pattern).then_some(i))
        .collect();
    assert!(!offsets.is_empty(), "live guest memory witness must exist");
    for offset in offsets {
        wrong.memory[offset] ^= 1;
    }
    assert_ne!(
        observation(&mut wrong.fresh().unwrap(), false),
        expected,
        "guest oracle must detect omitted RAM restoration"
    );
    println!("X3_RAM_NEGATIVE=detected");

    let mut wrong_cpu = Cut {
        memory: cut.memory.clone(),
        state: cut.state.clone(),
        host_class: cut.host_class,
    };
    assert_eq!(
        &wrong_cpu.state.xsave.0[400..408],
        &0x3141592653589793u64.to_le_bytes(),
        "cut must keep the guest vector live in hardware state"
    );
    wrong_cpu.state.xsave.0[400..416].fill(0);
    vmm_backend::arch::x86::canonicalize_xsave(&mut wrong_cpu.state.xsave.0);
    assert_ne!(
        observation(&mut wrong_cpu.fresh().unwrap(), false),
        expected,
        "guest oracle must detect omitted SIMD restoration"
    );
    println!("X3_CPU_NEGATIVE=detected");

    if let Some(directory) = std::env::var_os("X3_EXPORT") {
        cut.export(Path::new(&directory), &expected);
    }
    println!("X3_OBSERVATION={expected}");
    println!("X3_LOCAL=uninterrupted,capture-neutral,fresh,pressure,reused-or-refused");
}
