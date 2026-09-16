// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::vendor::x86::contract;
use crate::vmm::{GuestRam, Step, TerminalReason, Vmm};
use sha2::{Digest, Sha256};
use vmm_backend::{Backend, Gpa, KvmBackend, X86, X86Policy};

const IA32_EFER: u32 = 0xC000_0080;
const CONTROLLED_GUEST_IDENTITY: [u8; 32] = [1; 32];
const PORTABLE_TRACE_FIELDS_START: usize = 68;
const PORTABLE_STATE_START: usize = 84;
const PORTABLE_MEMORY_START: usize = 116;
const PORTABLE_DIGEST_LEN: usize = 32;

fn controlled_contract_hash() -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"harmony.controlled-guest-identity.v1\0");
    hash.update(crate::vendor::x86::contract::contract_hash());
    hash.update(CONTROLLED_GUEST_IDENTITY);
    hash.finalize().into()
}

fn production_policy() -> X86Policy {
    X86Policy {
        cpuid: contract::cpuid_model(),
        msr_filter: contract::msr_filter_allow(),
    }
}

fn compose<B, F>(mut guest_ram: GuestRam, mut backend: B, configure: F) -> Vmm<B>
where
    B: Backend<A = X86>,
    F: FnOnce(&mut B),
{
    backend
        .set_policy(&production_policy())
        .expect("install production x86 policy");
    // SAFETY: `guest_ram` is moved into the returned `Vmm` immediately after
    // this call. Parameter order drops the backend before RAM if configure
    // unwinds. `GuestRam` is page-aligned and pinned (mmap off Miri, and the
    // mock does not dereference the pointer under Miri); the VMM owns the
    // backing for the entire backend lifetime and no alias is created while a
    // backend run is in flight.
    unsafe {
        backend
            .map_memory(Gpa(0), guest_ram.as_mut_bytes())
            .expect("map guest RAM");
    }
    configure(&mut backend);
    Vmm::new(backend, guest_ram)
}

fn put_u64(bytes: &mut [u8], gpa: usize, value: u64) {
    bytes[gpa..gpa + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_bytes(bytes: &mut [u8], gpa: usize, value: &[u8]) {
    bytes[gpa..gpa + value.len()].copy_from_slice(value);
}

fn wire_snapshot_path(vmm: &mut Vmm<KvmBackend>) {
    vmm.wire_vtime(
        crate::vmm::VtimeWiring::new_virtual_time(
            crate::vendor::x86::contract_vclock_config(),
            0x0050_AE00,
        )
        .expect("wire virtual time"),
    );
    vmm.wire_snapshot_hashing();
}

fn require_kvm() {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm missing — run this ignored live check on a Linux x86-64 KVM host"
    );
}

fn compare_public_identity_portable_execution_state(
    left: &[u8],
    right: &[u8],
    expected_memory_len: usize,
) -> crate::portable_snapshot::PortableExecutionComparison {
    let strict = crate::portable_snapshot::compare_portable_execution_state(
        left,
        right,
        expected_memory_len,
    )
    .expect("validate complete portable artifact checksums");
    if strict.equal {
        return strict;
    }
    let left_snapshot =
        crate::portable_snapshot::PortableSnapshot::read_from(left, expected_memory_len)
            .expect("decode validated left portable artifact");
    let right_snapshot =
        crate::portable_snapshot::PortableSnapshot::read_from(right, expected_memory_len)
            .expect("decode validated right portable artifact");
    let mut left_state =
        vm_state::VmState::decode(&left_snapshot.vm_state).expect("decode left VM state");
    let mut right_state =
        vm_state::VmState::decode(&right_snapshot.vm_state).expect("decode right VM state");
    assert_eq!(
        left_state.encode().expect("re-encode left VM state"),
        left_snapshot.vm_state,
        "left VM state must use the current complete encoding"
    );
    assert_eq!(
        right_state.encode().expect("re-encode right VM state"),
        right_snapshot.vm_state,
        "right VM state must use the current complete encoding"
    );
    assert_eq!(left_state.contract_hash, controlled_contract_hash());
    assert_eq!(right_state.contract_hash, controlled_contract_hash());
    left_state.xsave_restore_bv =
        vmm_backend::logical_xsave_restore_bv(&left_state.xsave.0, left_state.xsave_restore_bv)
            .expect("validate left XSAVE restore provenance");
    right_state.xsave_restore_bv =
        vmm_backend::logical_xsave_restore_bv(&right_state.xsave.0, right_state.xsave_restore_bv)
            .expect("validate right XSAVE restore provenance");
    let left_vm_start = PORTABLE_MEMORY_START + left_snapshot.memory.len();
    let right_vm_start = PORTABLE_MEMORY_START + right_snapshot.memory.len();
    let left_vm_end = left_vm_start + left_snapshot.vm_state.len();
    let right_vm_end = right_vm_start + right_snapshot.vm_state.len();
    let equal = left_snapshot.vm_state.len() == right_snapshot.vm_state.len()
        && left_state == right_state
        && left[..PORTABLE_TRACE_FIELDS_START] == right[..PORTABLE_TRACE_FIELDS_START]
        && left[PORTABLE_STATE_START..left_vm_start] == right[PORTABLE_STATE_START..right_vm_start]
        && left[left_vm_end..left.len() - PORTABLE_DIGEST_LEN]
            == right[right_vm_end..right.len() - PORTABLE_DIGEST_LEN];
    crate::portable_snapshot::PortableExecutionComparison { equal, ..strict }
}

fn logically_equal_vm_state(left: &vm_state::VmState, right: &vm_state::VmState) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.xsave_restore_bv =
        vmm_backend::logical_xsave_restore_bv(&left.xsave.0, left.xsave_restore_bv)
            .expect("validate left VM state XSAVE restore provenance");
    right.xsave_restore_bv =
        vmm_backend::logical_xsave_restore_bv(&right.xsave.0, right.xsave_restore_bv)
            .expect("validate right VM state XSAVE restore provenance");
    left == right
}

fn public_identity_vmm(active: bool, seed: u64, xcr0: u64) -> Vmm<KvmBackend> {
    let mut ram = GuestRam::new(0x10000).unwrap();
    let bytes = ram.as_mut_bytes();
    put_u64(bytes, 0x3000, 0x4003);
    put_u64(bytes, 0x4000, 0x5003);
    put_u64(bytes, 0x5000, 0x83);
    bytes[0x2000..0x2004].copy_from_slice(&0x3f80_u32.to_le_bytes());
    bytes[0x2004..0x2008].copy_from_slice(&0x5f80_u32.to_le_bytes());
    let mut code = Vec::new();
    if active {
        code.extend_from_slice(&[0xd9, 0xe8, 0x66, 0x0f, 0x76, 0xc0]);
        code.extend_from_slice(&[0x0f, 0xae, 0x14, 0x25, 0x00, 0x20, 0, 0]);
    }
    code.extend_from_slice(&[0xba, 0xf8, 3, 0, 0, 0xb0, 0x41, 0xee]);
    code.extend_from_slice(&[0x0f, 0xae, 0x1c, 0x25, 0x10, 0x20, 0, 0]);
    code.extend_from_slice(&[0xf3, 0x0f, 0x7f, 0x04, 0x25, 0x20, 0x20, 0, 0]);
    if active {
        code.extend_from_slice(&[0xdd, 0x1c, 0x25, 0x30, 0x20, 0, 0]);
    }
    code.extend_from_slice(&[0xd9, 0xeb]);
    code.extend_from_slice(&[0xb8, 0x55, 0x55, 0x55, 0x55]);
    code.extend_from_slice(&[0x66, 0x0f, 0x6e, 0xc0, 0x66, 0x0f, 0x70, 0xc0, 0]);
    code.extend_from_slice(&[0x0f, 0xae, 0x14, 0x25, 0x04, 0x20, 0, 0]);
    if xcr0 == 7 {
        code.extend_from_slice(&[0xc5, 0xf5, 0x76, 0xc9]);
    }
    code.extend_from_slice(&[0xb0, 0x42, 0xee, 0xf4]);
    put_bytes(bytes, 0x1000, &code);
    let mut vmm = compose(ram, KvmBackend::new().unwrap(), |backend| {
        let mut state = backend.save().unwrap();
        let entry = crate::vendor::x86::entry::long_mode_entry(0x1000, 0, 0x3000, 0x7000);
        state.regs = entry.regs;
        state.sregs = entry.sregs;
        state.sregs.cr4 |= (1 << 9) | (1 << 18);
        state.xcr0 = xcr0;
        assert_eq!(
            &state.xsave[512..520],
            &0_u64.to_le_bytes(),
            "entry template must be canonical init"
        );
        state.xsave_restore_bv = Some(seed);
        vmm_backend::restore_xsave_image(&state.xsave, state.xsave_restore_bv)
            .expect("seed must represent the same canonical init state");
        state.msrs.insert(IA32_EFER, state.sregs.efer);
        backend.restore(&state).unwrap();
    });
    wire_snapshot_path(&mut vmm);
    vmm.controlled_guest_identity = Some(CONTROLLED_GUEST_IDENTITY);
    vmm.prepare_snapshot().unwrap();
    assert_eq!(
        vmm.save_vm_state().unwrap().contract_hash,
        controlled_contract_hash()
    );
    vmm
}

fn public_capture(
    server: &mut crate::control::ControlServer<KvmBackend>,
) -> (control_proto::SnapId, Vec<u8>, [u8; 32]) {
    let control_proto::Reply::Snapshot { id, .. } = server
        .handle(&control_proto::Request::Snapshot)
        .unwrap()
        .unwrap()
    else {
        panic!("snapshot did not return a snapshot handle");
    };
    let mut bytes = Vec::new();
    let receipt = server.export_portable_snapshot(id, &mut bytes).unwrap();
    (id, bytes, receipt.state_hash)
}

fn public_continue(
    server: &mut crate::control::ControlServer<KvmBackend>,
    active: bool,
) -> Vec<u8> {
    let mut stopped = false;
    for _ in 0..8 {
        if server.vmm_mut().unwrap().step().unwrap() == Step::Terminal(TerminalReason::Idle) {
            stopped = true;
            break;
        }
    }
    assert!(stopped, "bounded guest must reach HLT");
    assert_eq!(server.vmm().unwrap().serial_output(), b"AB");
    let memory = server.vmm().unwrap().guest_memory();
    assert_eq!(
        &memory[0x2010..0x2014],
        &if active { 0x3f80_u32 } else { 0x1f80_u32 }.to_le_bytes()
    );
    assert_eq!(
        &memory[0x2020..0x2030],
        &[if active { 0xff } else { 0 }; 16]
    );
    if active {
        assert_eq!(&memory[0x2030..0x2038], &1_f64.to_le_bytes());
    }
    public_capture(server).1
}

fn assert_public_identity_negative_controls() {
    use crate::control::{ControlServer, server_caps};
    let capture_initial = |seed| {
        let mut server = ControlServer::new(
            public_identity_vmm(true, seed, 7),
            Box::new(move || Ok(public_identity_vmm(true, seed, 7))),
        );
        server
            .handle(&control_proto::Request::Hello(server_caps()))
            .unwrap()
            .unwrap();
        public_capture(&mut server).1
    };
    let init_three = capture_initial(3);
    let init_two = capture_initial(2);
    assert!(
        compare_public_identity_portable_execution_state(&init_three, &init_two, 0x10000).equal,
        "logical init XSAVE metadata projection must accept only raw presence differences"
    );
    let mut server = ControlServer::new(
        public_identity_vmm(true, 3, 7),
        Box::new(|| Ok(public_identity_vmm(true, 3, 7))),
    );
    server
        .handle(&control_proto::Request::Hello(server_caps()))
        .unwrap()
        .unwrap();
    assert_eq!(server.vmm_mut().unwrap().step().unwrap(), Step::Continued);
    let memory = server.vmm().unwrap().guest_memory().to_vec();
    let mut baseline = server.vmm().unwrap().save_vm_state().unwrap();
    baseline.xcrs.xcr0 = 7;
    baseline.xsave.0[576..592].fill(0x5a);
    baseline.xsave.0[512..520].copy_from_slice(&7_u64.to_le_bytes());
    baseline.xsave_restore_bv = Some(7);
    let restore_target = |state: &vm_state::VmState| {
        let mut target = ControlServer::new(
            public_identity_vmm(true, 3, 7),
            Box::new(|| Ok(public_identity_vmm(true, 3, 7))),
        );
        target
            .handle(&control_proto::Request::Hello(server_caps()))
            .unwrap()
            .unwrap();
        target
            .vmm_mut()
            .unwrap()
            .restore_snapshot(&memory, state)
            .unwrap();
        target
    };
    let mut baseline_target = restore_target(&baseline);
    let (_, baseline_bytes, baseline_hash) = public_capture(&mut baseline_target);
    for (label, offset, replacement) in [
        ("x87", 40, 0x00_u8),
        ("XMM", 160, 0xfe),
        ("MXCSR", 25, 0x5f),
        ("YMM", 576, 0xa5),
    ] {
        let mut changed = baseline.clone();
        assert_ne!(changed.xsave.0[offset], replacement);
        changed.xsave.0[offset] = replacement;
        let mut server = restore_target(&changed);
        let observed = server.vmm().unwrap().save_vm_state().unwrap();
        assert_eq!(
            observed.xsave.0[offset], replacement,
            "{label} mutation must survive restore"
        );
        assert_eq!(
            server.vmm().unwrap().guest_memory(),
            memory,
            "negative control RAM must be identical"
        );
        let (_, bytes, hash) = public_capture(&mut server);
        assert_ne!(
            hash, baseline_hash,
            "published identity must distinguish {label}"
        );
        assert!(
            bytes != baseline_bytes,
            "published bytes must distinguish {label}"
        );
        assert!(
            !compare_public_identity_portable_execution_state(&baseline_bytes, &bytes, 0x10000)
                .equal,
            "portable comparison must retain {label} changes outside raw init metadata"
        );
    }
}

#[test]
#[ignore = "requires Linux x86-64 KVM"]
fn public_snapshot_replay_recapture_preserves_xsave_identity() {
    use crate::control::{ControlServer, RestoreMode};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    require_kvm();
    assert_public_identity_negative_controls();
    let mut failures = Vec::new();
    for seed in [0, 2, 3] {
        for xcr0 in [3, 7] {
            for active in [false, true] {
                for after_out in [false, true] {
                    for mode in [RestoreMode::Memcpy, RestoreMode::InPlace] {
                        eprintln!(
                            "public identity case: seed={seed}, xcr0={xcr0}, active={active}, after_out={after_out}, mode={mode:?}"
                        );
                        let creations = Arc::new(AtomicUsize::new(0));
                        let factory_count = creations.clone();
                        let mut server = ControlServer::new(
                            public_identity_vmm(active, seed, xcr0),
                            Box::new(move || {
                                factory_count.fetch_add(1, Ordering::SeqCst);
                                Ok(public_identity_vmm(active, seed, xcr0))
                            }),
                        );
                        server
                            .handle(&control_proto::Request::Hello(crate::control::server_caps()))
                            .unwrap()
                            .unwrap();
                        server.set_restore_mode(mode);
                        if after_out {
                            assert_eq!(server.vmm_mut().unwrap().step().unwrap(), Step::Continued);
                            assert_eq!(server.vmm().unwrap().serial_output(), b"A");
                        }
                        let (a, a_bytes, a_hash) = public_capture(&mut server);
                        let a_raw = server
                            .vmm()
                            .unwrap()
                            .save_vm_state()
                            .unwrap()
                            .xsave_restore_bv;
                        let (a_again, a_again_bytes, a_again_hash) = public_capture(&mut server);
                        assert_ne!(a, a_again);
                        assert_eq!(a_hash, a_again_hash, "repeated A capture hash");
                        assert!(a_bytes == a_again_bytes, "repeated A capture bytes");
                        let source_xsave = server.vmm().unwrap().save_vm_state().unwrap().xsave;
                        let expected = public_continue(&mut server, active);
                        let dirty_xsave = server.vmm().unwrap().save_vm_state().unwrap().xsave;
                        assert_ne!(
                            source_xsave, dirty_xsave,
                            "guest continuation must dirty FP state before replay"
                        );
                        assert_eq!(&dirty_xsave.0[160..176], &[0x55; 16]);
                        assert_eq!(&dirty_xsave.0[24..28], &0x5f80_u32.to_le_bytes());
                        if xcr0 == 7 {
                            assert_eq!(&dirty_xsave.0[592..608], &[0xff; 16]);
                        }
                        let restores_before = creations.load(Ordering::SeqCst);
                        assert_eq!(
                            server
                                .handle(&control_proto::Request::Replay(a))
                                .unwrap()
                                .unwrap(),
                            control_proto::Reply::Unit
                        );
                        assert_eq!(
                            server.in_place_fallbacks(),
                            0,
                            "reuse must not silently fall back"
                        );
                        assert_eq!(
                            creations.load(Ordering::SeqCst) - restores_before,
                            usize::from(mode == RestoreMode::Memcpy)
                        );
                        let (b, b_bytes, b_hash) = public_capture(&mut server);
                        let b_state = server.vmm().unwrap().save_vm_state().unwrap();
                        let b_memory = server.vmm().unwrap().guest_memory().to_vec();
                        let b_raw = b_state.xsave_restore_bv;
                        let case = format!(
                            "seed={seed}, xcr0={xcr0}, active={active}, after_out={after_out}, mode={mode:?}"
                        );
                        eprintln!("post-capture raw metadata: A={a_raw:?}, B={b_raw:?}");
                        let (b_again, b_again_bytes, b_again_hash) = public_capture(&mut server);
                        assert_ne!(b, b_again);
                        assert_eq!(b_hash, b_again_hash, "repeated B capture hash");
                        assert!(b_bytes == b_again_bytes, "repeated B capture bytes");
                        eprintln!(
                            "A/B envelope differences: active={active}, after_out={after_out}, mode={mode:?}: {:?}",
                            a_bytes
                                .iter()
                                .zip(&b_bytes)
                                .enumerate()
                                .filter(|(_, (a, b))| a != b)
                                .take(16)
                                .map(|(offset, (a, b))| (offset, *a, *b))
                                .collect::<Vec<_>>()
                        );
                        for cycle in 0..3 {
                            server.vmm_mut().unwrap().prepare_snapshot().unwrap();
                            let (_, prepared_bytes, prepared_hash) = public_capture(&mut server);
                            let prepared_state = server.vmm().unwrap().save_vm_state().unwrap();
                            let mut without_presence_change = prepared_state.clone();
                            without_presence_change.xsave_restore_bv = b_state.xsave_restore_bv;
                            let ram_equal = server.vmm().unwrap().guest_memory() == b_memory;
                            let portable_equal = compare_public_identity_portable_execution_state(
                                &b_bytes,
                                &prepared_bytes,
                                0x10000,
                            )
                            .equal;
                            eprintln!(
                                "host-only preparation {cycle}: {case}, raw_before={:?}, raw_after={:?}, full_state_equal={}, state_equal_except_raw_bitmap={}, state_equal_except_logical_raw_bitmap={}, ram_equal={ram_equal}, hash_equal={}, portable_logical_equal={portable_equal}",
                                b_state.xsave_restore_bv,
                                prepared_state.xsave_restore_bv,
                                b_state == prepared_state,
                                b_state == without_presence_change,
                                logically_equal_vm_state(&b_state, &prepared_state),
                                b_hash == prepared_hash,
                            );
                            if b_hash != prepared_hash
                                || !portable_equal
                                || !logically_equal_vm_state(&b_state, &prepared_state)
                                || !ram_equal
                            {
                                failures.push(format!("host-only preparation {cycle}: {case}"));
                            }
                        }
                        assert_ne!(a, b);
                        if a_hash != b_hash {
                            failures.push(format!("immediate recapture hash: {case}"));
                        }
                        for (label, left, right) in [
                            ("immediate A/B", a_bytes, b_bytes),
                            (
                                "continuation",
                                expected,
                                public_continue(&mut server, active),
                            ),
                        ] {
                            let comparison = compare_public_identity_portable_execution_state(
                                &left, &right, 0x10000,
                            );
                            eprintln!(
                                "{label}: active={active}, after_out={after_out}, mode={mode:?}, comparison={comparison:?}, first_envelope_difference={:?}",
                                left.iter().zip(&right).position(|(a, b)| a != b)
                            );
                            if !comparison.equal {
                                failures
                                    .push(format!("complete execution state at {label}: {case}"));
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "controlled public snapshot failures:\n{}",
        failures.join("\n")
    );
}
