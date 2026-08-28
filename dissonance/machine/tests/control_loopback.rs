// SPDX-License-Identifier: AGPL-3.0-or-later

//! Real socket adapter ↔ real `ControlServer<MockBackend>` closure.

#![cfg(unix)]

use std::os::unix::net::UnixStream;

use machine::{
    Machine,
    control::{RestoreCounters, SocketMachine},
    nes::{ButtonChord, reproducer},
};
use vmm_backend::{Backend, MockBackend, X86Policy};
use vmm_core::{
    control::ControlServer,
    vendor::x86::contract_vclock_config,
    vmm::{GuestRam, Vmm, VtimeWiring},
};

const RAM: usize = 0x1_0000;

fn configured_vmm() -> Vmm<MockBackend> {
    let mut backend = MockBackend::new();
    backend
        .set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .expect("configure mock backend");
    let mut vmm = Vmm::new(backend, GuestRam::new(RAM).expect("guest RAM"));
    vmm.wire_vtime(
        VtimeWiring::new_virtual_time(contract_vclock_config(), 0).expect("V-time wiring"),
    );
    vmm.wire_snapshot_hashing();
    vmm
}

#[test]
fn socket_machine_drives_the_real_control_server() {
    let (client_end, server_end) = UnixStream::pair().expect("socketpair");
    let client = std::thread::spawn(move || {
        let mut machine = SocketMachine::from_stream(client_end).expect("negotiate");
        let genesis = machine.snapshot().expect("genesis snapshot");
        machine.mark_genesis(genesis).expect("mark genesis");
        machine
            .branch(genesis, &reproducer(&[ButtonChord::new(0x81, 4)]))
            .expect("branch payload tape");
        let branch_hash = machine.state_hash().expect("branch hash");
        assert_eq!(machine.read(0, 8).expect("read RAM"), vec![0; 8]);

        let continuation = machine.snapshot().expect("continuation snapshot");
        machine.replay(continuation).expect("replay continuation");
        assert_eq!(
            machine.state_hash().expect("replay hash"),
            branch_hash,
            "a no-op continuation replay restores the identical whole state"
        );
        machine
            .drop_snapshot(continuation)
            .expect("drop continuation");
        assert_eq!(
            machine.restore_counters(),
            RestoreCounters {
                genesis: 1,
                continuation: 1,
            }
        );
    });

    let mut server = ControlServer::new(configured_vmm(), Box::new(|| Ok(configured_vmm())));
    server.serve(server_end).expect("serve client session");
    client.join().expect("client thread");
}
