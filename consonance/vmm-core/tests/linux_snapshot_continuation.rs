// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vmm_backend::{Backend, BackendError, X86};
use vmm_core::vendor::x86::bringup::{
    boot_linux_stock_virtual_time, compose_stock_virtual_time_restore_target,
};
use vmm_core::vmm::{Step, TerminalReason, Vmm, VmmError};

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

fn boot_seed(seed: u64) -> Machine {
    wired(
        boot_linux_stock_virtual_time(
            &std::fs::read(root().join("bzImage")).expect("retained Linux kernel"),
            &std::fs::read(root().join("initramfs-continuation.cpio.gz"))
                .expect("owned continuation initramfs"),
            RAM,
            CMDLINE,
            seed,
        )
        .expect("boot composed Linux"),
    )
}

fn wired(mut vmm: Machine) -> Machine {
    vmm.wire_snapshot_hashing();
    vmm
}

fn target() -> Result<Machine, VmmError> {
    compose_stock_virtual_time_restore_target(snapshot_store::Mapping::anonymous(RAM), SEED)
        .map(wired)
}

fn boot() -> Machine {
    boot_seed(SEED)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Checkpoint {
    state: [u8; 32],
    history: control_proto::EventHistory,
}

fn checkpoint(vmm: &mut Machine) -> Checkpoint {
    vmm.prepare_snapshot()
        .expect("complete the checkpoint boundary");
    Checkpoint {
        state: vmm.state_hash().expect("whole modeled VMM state"),
        history: vmm.event_history().expect("normalized history available"),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Observation {
    readback: String,
    checkpoint: Checkpoint,
    events: u64,
    elapsed_vns: u64,
    terminal_state: [u8; 32],
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hash(text: &str) -> [u8; 32] {
    assert_eq!(text.len(), 64);
    std::array::from_fn(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap())
}

impl Checkpoint {
    fn encode(&self) -> String {
        format!(
            "{} {} {}",
            hex(&self.state),
            self.history.count,
            hex(&self.history.digest)
        )
    }

    fn decode(words: &[&str]) -> Self {
        assert_eq!(words.len(), 3);
        Self {
            state: hash(words[0]),
            history: control_proto::EventHistory {
                count: words[1].parse().unwrap(),
                digest: hash(words[2]),
            },
        }
    }

    fn emit(&self, name: &str) {
        println!("X3_{name}_STATE={}", hex(&self.state));
        println!(
            "X3_{name}_HISTORY={}:{}",
            self.history.count,
            hex(&self.history.digest)
        );
    }
}

impl Observation {
    fn assert_raw_matches(&self, expected: &Self) {
        assert_eq!(
            self.readback, expected.readback,
            "raw continuation guest readback"
        );
        assert_eq!(
            self.checkpoint.state, expected.checkpoint.state,
            "raw continuation whole state"
        );
        assert_eq!(
            self.terminal_state, expected.terminal_state,
            "raw terminal whole state"
        );
        assert_eq!(self.events, expected.events, "raw continuation work");
        assert_eq!(
            self.elapsed_vns, expected.elapsed_vns,
            "raw continuation V-time"
        );
    }

    fn encode(&self) -> String {
        format!(
            "{} {} {} {}",
            self.checkpoint.encode(),
            self.events,
            self.elapsed_vns,
            hex(&self.terminal_state)
        )
    }

    fn decode(readback: String, words: &[&str]) -> Self {
        assert_eq!(words.len(), 6);
        Self {
            readback,
            checkpoint: Checkpoint::decode(&words[..3]),
            events: words[3].parse().unwrap(),
            elapsed_vns: words[4].parse().unwrap(),
            terminal_state: hash(words[5]),
        }
    }

    fn emit(&self) {
        println!("X3_DONE_STATE={}", hex(&self.checkpoint.state));
        println!("X3_WORK={}:{}", self.events, self.elapsed_vns);
        println!("X3_TERMINAL_STATE={}", hex(&self.terminal_state));
        println!("X3_TERMINAL=guest-poweroff-idle");
    }
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

fn observation(vmm: &mut Machine, pressure: bool) -> Observation {
    let start = vmm.event_history().unwrap().count;
    let start_vns = vmm.effective_vns().unwrap();
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
    let readback = records[0].to_owned();
    let done = checkpoint(vmm);
    let events = done.history.count.checked_sub(start).unwrap();
    let elapsed_vns = vmm.effective_vns().unwrap().checked_sub(start_vns).unwrap();
    assert!(
        events > 0 && elapsed_vns > 0,
        "nonvacuous continuation work"
    );
    for steps in 0..1_000_000 {
        match vmm.step().expect("complete guest shutdown") {
            Step::Continued => {}
            Step::Terminal(TerminalReason::Idle) => {
                return Observation {
                    readback,
                    checkpoint: done,
                    events,
                    elapsed_vns,
                    terminal_state: checkpoint(vmm).state,
                };
            }
            other => panic!("unexpected continuation termination: {other:?}"),
        }
        assert_ne!(steps, 999_999, "guest must terminate within the work bound");
    }
    unreachable!()
}

struct Cut {
    memory: Vec<u8>,
    state: vm_state::VmState,
    host_class: Option<vmm_backend::HostCoreClass>,
    checkpoint: Checkpoint,
}

impl Cut {
    fn capture(vmm: &mut Machine) -> Self {
        vmm.prepare_snapshot()
            .expect("retire the pending hardware completion before capture");
        let checkpoint = checkpoint(vmm);
        let state = vmm
            .save_vm_state()
            .expect("completed-boundary Linux capture");
        Self {
            memory: vmm.guest_memory().to_vec(),
            state,
            host_class: vmm.vcpu_record().unwrap().host_core_class,
            checkpoint,
        }
    }

    fn fresh(&self) -> Result<Machine, VmmError> {
        let mapping = snapshot_store::Mapping::anonymous(self.memory.len());
        let mut vmm = wired(compose_stock_virtual_time_restore_target(mapping, SEED)?);
        vmm.restore_snapshot(&self.memory, &self.state)?;
        Ok(vmm)
    }

    fn export(&self, directory: &Path, expected: &Observation) {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(directory.join("memory.bin"), &self.memory).unwrap();
        std::fs::write(directory.join("state.bin"), self.state.encode().unwrap()).unwrap();
        std::fs::write(directory.join("observation.txt"), &expected.readback).unwrap();
        std::fs::write(
            directory.join("oracles.txt"),
            format!("{}\n{}\n", self.checkpoint.encode(), expected.encode()),
        )
        .unwrap();
        std::fs::write(
            directory.join("host-class.txt"),
            format!("{:?}", self.host_class),
        )
        .unwrap();
    }
}

type Server = vmm_core::control::ControlServer<Box<dyn Backend<A = X86>>>;

fn server(vmm: Machine) -> Server {
    let mut server = Server::new(wired(vmm), Box::new(target));
    assert!(matches!(
        server
            .handle(&control_proto::Request::Hello(
                vmm_core::control::server_caps()
            ))
            .unwrap(),
        Ok(control_proto::Reply::Hello(_))
    ));
    server
}

struct ControlCut {
    portable: Vec<u8>,
    ready: Checkpoint,
    expected: Observation,
}

impl ControlCut {
    fn restore(&self) -> Server {
        let mut target = server(
            compose_stock_virtual_time_restore_target(
                snapshot_store::Mapping::anonymous(RAM),
                SEED,
            )
            .unwrap(),
        );
        let receipt = target
            .import_portable_snapshot(self.portable.as_slice())
            .unwrap();
        assert_eq!(receipt.event_history, Some(self.ready.history));
        assert!(
            target
                .handle(&control_proto::Request::Replay(receipt.id))
                .unwrap()
                .is_ok()
        );
        assert_eq!(checkpoint(target.vmm_mut().unwrap()), self.ready);
        target
    }

    fn emit(&self) {
        self.ready.emit("CONTROL_READY");
        self.expected.checkpoint.emit("CONTROL_DONE");
        println!(
            "X3_CONTROL_TERMINAL_STATE={}",
            hex(&self.expected.terminal_state)
        );
        println!(
            "X3_CONTROL_WORK={}:{}",
            self.expected.events, self.expected.elapsed_vns
        );
    }

    fn export(&self, directory: &Path) {
        std::fs::write(directory.join("portable.bin"), &self.portable).unwrap();
        std::fs::write(
            directory.join("control-oracles.txt"),
            format!("{}\n{}\n", self.ready.encode(), self.expected.encode()),
        )
        .unwrap();
    }

    fn import(directory: &Path, readback: String) -> Self {
        let oracles = std::fs::read_to_string(directory.join("control-oracles.txt")).unwrap();
        let words: Vec<_> = oracles.split_whitespace().collect();
        assert_eq!(words.len(), 9);
        Self {
            portable: std::fs::read(directory.join("portable.bin")).unwrap(),
            ready: Checkpoint::decode(&words[..3]),
            expected: Observation::decode(readback, &words[3..]),
        }
    }
}

fn control_continuation() -> ControlCut {
    let mut source = server(boot());
    until(source.vmm_mut().unwrap(), b"X3_READY\r\n", false);
    let ready = checkpoint(source.vmm_mut().unwrap());
    let id = match source
        .handle(&control_proto::Request::Snapshot)
        .unwrap()
        .unwrap()
    {
        control_proto::Reply::Snapshot { id, .. } => id,
        other => panic!("unexpected snapshot reply: {other:?}"),
    };
    let mut portable = Vec::new();
    let receipt = source.export_portable_snapshot(id, &mut portable).unwrap();
    assert_eq!(receipt.event_history, Some(ready.history));
    assert_eq!(
        checkpoint(source.vmm_mut().unwrap()),
        ready,
        "control capture preserves whole state and history"
    );
    let expected = observation(source.vmm_mut().unwrap(), false);
    drop(source);
    let cut = ControlCut {
        portable,
        ready,
        expected,
    };
    let mut fresh = cut.restore();
    assert_eq!(
        observation(fresh.vmm_mut().unwrap(), true),
        cut.expected,
        "portable control snapshot preserves whole state and history"
    );
    cut
}

fn import(directory: &Path) {
    let memory = std::fs::read(directory.join("memory.bin")).unwrap();
    assert_eq!(memory.len(), RAM);
    let state =
        vm_state::VmState::decode(&std::fs::read(directory.join("state.bin")).unwrap()).unwrap();
    let expected = std::fs::read_to_string(directory.join("observation.txt")).unwrap();
    let oracles = std::fs::read_to_string(directory.join("oracles.txt")).unwrap();
    let words: Vec<_> = oracles.split_whitespace().collect();
    assert_eq!(words.len(), 9);
    let ready = Checkpoint::decode(&words[..3]);
    let expected = Observation::decode(expected, &words[3..]);
    let mut vmm = target().unwrap();
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
            let actual_ready = checkpoint(&mut vmm);
            assert_eq!(
                actual_ready.state, ready.state,
                "imported READY whole state"
            );
            println!("X3_READY_STATE={}", hex(&actual_ready.state));
            let observed = observation(&mut vmm, true);
            observed.assert_raw_matches(&expected);
            let control = ControlCut::import(directory, expected.readback.clone());
            let mut restored_control = control.restore();
            assert_eq!(
                observation(restored_control.vmm_mut().unwrap(), true),
                control.expected
            );
            control.emit();
            observed.emit();
            println!("X3_OBSERVATION={}", observed.readback);
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
            observation(&mut vmm, false).assert_raw_matches(&own_expected);
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
    let ready = checkpoint(&mut uninterrupted);
    let expected = observation(&mut uninterrupted, false);
    let fields: Vec<_> = expected.readback.split_whitespace().collect();
    let hash = (0..4096u64).fold(0xcbf29ce484222325u64, |hash, i| {
        (hash ^ u64::from((i * 17 + 31) as u8)).wrapping_mul(0x100000001b3)
    });
    assert_eq!(fields[1], format!("{hash:016x}"));
    assert_eq!(
        &fields[2..5],
        &["3141592653589793", "2384626433832795", "400921fb54442d18"]
    );
    drop(uninterrupted);

    let alternate_seed = SEED ^ 0x1_0000;
    let mut alternate = boot_seed(alternate_seed);
    until(&mut alternate, b"X3_READY\r\n", false);
    let alternate_ready = checkpoint(&mut alternate);
    let alternate_observed = observation(&mut alternate, false);
    drop(alternate);
    let mut alternate_repeat = boot_seed(alternate_seed);
    until(&mut alternate_repeat, b"X3_READY\r\n", false);
    assert_eq!(checkpoint(&mut alternate_repeat), alternate_ready);
    assert_eq!(
        observation(&mut alternate_repeat, false),
        alternate_observed
    );
    let alternate_fields: Vec<_> = alternate_observed.readback.split_whitespace().collect();
    assert_eq!(
        &fields[1..7],
        &alternate_fields[1..7],
        "seed-pure readbacks"
    );
    assert_ne!(
        fields[7], alternate_fields[7],
        "entropy-consuming readback must change"
    );
    assert_eq!(
        expected.events, alternate_observed.events,
        "seed-stable work"
    );
    assert_eq!(
        expected.elapsed_vns, alternate_observed.elapsed_vns,
        "seed-stable V-time"
    );
    println!(
        "X3_O3={SEED:016x}:{alternate_seed:016x}:{}:{}:{}:{}:{}:{}",
        fields[7],
        alternate_fields[7],
        expected.events,
        alternate_observed.events,
        expected.elapsed_vns,
        alternate_observed.elapsed_vns
    );
    println!("X3_O3_REPEAT=matched");
    println!("X3_O3_PURE=matched");
    println!("X3_O3_TERMINAL=guest-poweroff-idle");

    let mut source = boot();
    until(&mut source, b"X3_READY\r\n", false);
    assert_eq!(
        checkpoint(&mut source),
        ready,
        "same-seed READY whole state and history"
    );
    let cut = Cut::capture(&mut source);
    assert_eq!(
        observation(&mut source, false),
        expected,
        "capture must be neutral"
    );
    drop(source);
    let mut fresh = cut.fresh().unwrap();
    assert_eq!(checkpoint(&mut fresh).state, ready.state);
    observation(&mut fresh, false).assert_raw_matches(&expected);
    drop(fresh);
    observation(&mut cut.fresh().unwrap(), true).assert_raw_matches(&expected);

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
            assert_eq!(checkpoint(&mut reused).state, ready.state);
            observation(&mut reused, true).assert_raw_matches(&expected);
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
                observation(&mut reused, false).readback,
                expected.readback,
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
        checkpoint: cut.checkpoint.clone(),
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
        observation(&mut wrong.fresh().unwrap(), false).readback,
        expected.readback,
        "guest oracle must detect omitted RAM restoration"
    );
    println!("X3_RAM_NEGATIVE=detected");

    let mut wrong_cpu = Cut {
        memory: cut.memory.clone(),
        state: cut.state.clone(),
        host_class: cut.host_class,
        checkpoint: cut.checkpoint.clone(),
    };
    assert_eq!(
        &wrong_cpu.state.xsave.0[400..408],
        &0x3141592653589793u64.to_le_bytes(),
        "cut must keep the guest vector live in hardware state"
    );
    wrong_cpu.state.xsave.0[400..416].fill(0);
    vmm_backend::arch::x86::canonicalize_xsave(&mut wrong_cpu.state.xsave.0);
    assert_ne!(
        observation(&mut wrong_cpu.fresh().unwrap(), false).readback,
        expected.readback,
        "guest oracle must detect omitted SIMD restoration"
    );
    println!("X3_CPU_NEGATIVE=detected");
    drop(wrong);
    drop(wrong_cpu);

    let mut latent = Cut {
        memory: cut.memory.clone(),
        state: cut.state.clone(),
        host_class: cut.host_class,
        checkpoint: cut.checkpoint.clone(),
    };
    latent.memory[RAM - 1] ^= 1;
    let mut latent_vm = latent.fresh().unwrap();
    assert_ne!(checkpoint(&mut latent_vm).state, ready.state);
    let latent_observed = observation(&mut latent_vm, false);
    assert_eq!(
        latent_observed.readback, expected.readback,
        "latent-byte control preserves guest observations"
    );
    assert_eq!(latent_observed.events, expected.events);
    assert_eq!(latent_observed.elapsed_vns, expected.elapsed_vns);
    assert_ne!(
        latent_observed.checkpoint.state, expected.checkpoint.state,
        "O1 must detect latent RAM outside emitted observations"
    );
    println!("X3_O1_NEGATIVE=detected-output-equal");
    drop(latent_vm);
    drop(latent);

    let control = control_continuation();
    assert_eq!(
        control.expected.readback, expected.readback,
        "composed and raw guest observations"
    );
    control.emit();
    if let Some(directory) = std::env::var_os("X3_EXPORT") {
        cut.export(Path::new(&directory), &expected);
        control.export(Path::new(&directory));
    }
    println!("X3_O2=ram-simd-x87-goldens");
    println!("X3_READY_STATE={}", hex(&ready.state));
    expected.emit();
    println!("X3_OBSERVATION={}", expected.readback);
    println!("X3_LOCAL=uninterrupted,capture-neutral,fresh,pressure,reused-or-refused");
}
