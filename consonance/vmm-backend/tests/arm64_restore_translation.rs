// SPDX-License-Identifier: AGPL-3.0-or-later
const BYTES: usize = 0x10_0000;

struct Memory {
    ptr: *mut u8,
    layout: std::alloc::Layout,
}

impl Memory {
    fn new() -> Self {
        let layout = std::alloc::Layout::from_size_align(BYTES, 16384).unwrap();
        // SAFETY: the nonzero layout has valid power-of-two alignment.
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!ptr.is_null());
        Self { ptr, layout }
    }

    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: the allocation contains BYTES initialized bytes and the
        // backend is stopped whenever the exclusive host view is used.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, BYTES) }
    }

    fn word(&mut self, offset: usize, value: u64) {
        self.bytes()[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        // SAFETY: this pointer was allocated with this layout and Guest drops
        // the backend retaining its mapping before this memory.
        unsafe { std::alloc::dealloc(self.ptr, self.layout) };
    }
}

#[test]
fn aligned_memory_owns_initialized_bytes() {
    let mut memory = Memory::new();
    assert_eq!(memory.ptr as usize % 16384, 0);
    assert!(memory.bytes().iter().all(|&byte| byte == 0));
    memory.word(BYTES - 8, 0x1234);
    assert_eq!(&memory.bytes()[BYTES - 8..], &0x1234u64.to_le_bytes());
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
mod live {
    use super::*;
    use vmm_backend::{Arm64KvmBackend, Arm64Policy, Backend, CommonExit, Exit, Gpa, LiveKvm};
    const RAM: u64 = 0x4000_0000;
    const OUTPUT: u64 = 0x0900_0000;

    struct PinnedCpu(rustix::thread::CpuSet);

    impl PinnedCpu {
        fn current() -> Self {
            use rustix::thread::{CpuSet, sched_getaffinity, sched_getcpu, sched_setaffinity};
            let previous = sched_getaffinity(None).unwrap();
            let cpu = sched_getcpu();
            assert!(cpu < CpuSet::MAX_CPU && previous.is_set(cpu));
            let mut selected = CpuSet::new();
            selected.set(cpu);
            sched_setaffinity(None, &selected).unwrap();
            assert_eq!(sched_getaffinity(None).unwrap(), selected);
            eprintln!("ARM restore witness pinned to CPU {cpu}");
            Self(previous)
        }
    }

    impl Drop for PinnedCpu {
        fn drop(&mut self) {
            if let Err(error) = rustix::thread::sched_setaffinity(None, &self.0) {
                eprintln!("failed to restore test affinity: {error}");
                assert!(std::thread::panicking());
            }
        }
    }

    struct Guest {
        backend: Arm64KvmBackend<LiveKvm>,
        memory: Memory,
    }

    impl Guest {
        fn new() -> Self {
            Self::with_read_instruction(0xf940_0141)
        }

        fn with_read_instruction(read_instruction: u32) -> Self {
            let mut memory = Memory::new();
            for (offset, value) in [
                (0x1000, RAM + 0x6003),
                (0x1008, RAM + 0x2003),
                (0x2000, RAM | 0x701),
                (0x2008, RAM + 0x3003),
                (0x3000, RAM + 0x4703),
                (0x4000, 0xaaaa),
                (0x5000, 0xbbbb),
                (0x6000 + 72 * 8, OUTPUT | (1 << 54) | (1 << 53) | 0x405),
            ] {
                memory.word(offset, value);
            }
            let movz = |r: u32, value: u32| 0xd2a0_0000 | (value << 5) | r;
            let movk = |r: u32, value: u32| 0xf280_0000 | (value << 5) | r;
            let words = [
                movz(9, 0x0900),
                movz(10, 0x4020),
                movz(11, 0x4000),
                movk(11, 0x3000),
                movz(12, 0x4000),
                movk(12, 0x5703),
                movz(13, 0x4000),
                movk(13, 0x4703),
                0xf940_0120,
                0xb400_01a0,
                0xf100_041f,
                0x5400_0080,
                0xf100_081f,
                0x5400_0080,
                0x1400_0004,
                0xf900_016c,
                0x1400_0002,
                0xf900_016d,
                0xd503_3a9f,
                0xd508_831f,
                0xd503_3b9f,
                0xd503_3fdf,
                read_instruction,
                0xf900_0121,
                0x17ff_fff0,
            ];
            for (slot, word) in memory.bytes().chunks_exact_mut(4).zip(words) {
                slot.copy_from_slice(&word.to_le_bytes());
            }
            let mut backend = Arm64KvmBackend::new(LiveKvm::new().unwrap());
            backend.set_policy(&Arm64Policy::default()).unwrap();
            // SAFETY: this page-aligned allocation never moves, outlives the
            // backend, and host access only occurs while the vCPU is stopped.
            unsafe { backend.map_memory(Gpa(RAM), memory.bytes()) }.unwrap();
            backend.invalidate_instruction_cache(memory.ptr as usize, BYTES);
            let mut initial = backend.save().unwrap();
            initial.core.pc = RAM;
            initial.core.pstate = 0x3c5;
            initial.sysregs.sctlr_el1 |= 1 | (1 << 2) | (1 << 12);
            initial.sysregs.ttbr0_el1 = (1 << 48) | (RAM + 0x1000);
            initial.sysregs.tcr_el1 = 25
                | (1 << 8)
                | (1 << 10)
                | (3 << 12)
                | (25 << 16)
                | (1 << 23)
                | (2 << 30)
                | (2 << 32);
            initial.sysregs.mair_el1 = 0xff;
            backend.restore(&initial).unwrap();
            Self { backend, memory }
        }

        fn command(&mut self, command: u64) -> u64 {
            assert!(matches!(
                self.backend.run().unwrap(),
                Exit::Common(CommonExit::Mmio {
                    gpa: Gpa(OUTPUT),
                    write: None,
                    ..
                })
            ));
            self.backend.complete_read(command).unwrap();
            let value = match self.backend.run().unwrap() {
                Exit::Common(CommonExit::Mmio {
                    gpa: Gpa(OUTPUT),
                    write: Some(value),
                    ..
                }) => value,
                other => panic!("expected guest readback, got {other:?}"),
            };
            self.backend.save().unwrap();
            value
        }
    }

    #[test]
    #[ignore = "live ARM KVM; run pinned to one host CPU"]
    fn empty_dirty_detour_refuses_reuse_and_fresh_restore_discards_abandoned_translation() {
        let _pinned = PinnedCpu::current();
        let mut guest = Guest::new();
        assert_eq!(guest.command(0), 0xaaaa);
        let state = guest.backend.save().unwrap();
        let memory = guest.memory.bytes().to_vec();
        guest.backend.drain_dirty_pages().unwrap();
        assert_eq!(guest.command(1), 0xbbbb);
        guest.backend.save().unwrap();
        let dirty = guest.backend.drain_dirty_pages().unwrap();
        assert!(
            !dirty.is_empty(),
            "the detour must write its page-table entry"
        );
        for _ in 0..32 {
            assert_eq!(guest.command(0), 0xbbbb);
        }
        assert!(guest.backend.drain_dirty_pages().unwrap().is_empty());
        guest.memory.bytes().copy_from_slice(&memory);
        let without_restore = guest.command(0);
        eprintln!("empty-dirty no-maintenance control={without_restore:#x}");
        assert_eq!(guest.command(1), 0xbbbb);
        guest.backend.save().unwrap();
        guest.backend.drain_dirty_pages().unwrap();
        for _ in 0..32 {
            assert_eq!(guest.command(0), 0xbbbb);
        }
        assert!(guest.backend.drain_dirty_pages().unwrap().is_empty());
        let before = guest.backend.save().unwrap();
        let before_memory = guest.memory.bytes().to_vec();
        assert!(matches!(
            guest.backend.validate_restore_state(&state),
            Err(vmm_backend::BackendError::Arm64ReusedVcpuRestoreUnsafe)
        ));
        guest.backend.validate_import_state(&state).unwrap();
        assert!(matches!(
            guest.backend.restore(&state),
            Err(vmm_backend::BackendError::Arm64ReusedVcpuRestoreUnsafe)
        ));
        let mut after = guest.backend.save().unwrap();
        assert!(after.vtimer.counter >= before.vtimer.counter);
        after.vtimer.counter = before.vtimer.counter;
        assert_eq!(after, before);
        assert_eq!(guest.memory.bytes(), before_memory);
        assert_eq!(guest.command(0), 0xbbbb);
        guest.memory.bytes().copy_from_slice(&memory);
        assert_eq!(guest.command(3), 0xaaaa);
        drop(guest);
        let mut fresh = Guest::new();
        fresh.backend.restore(&state).unwrap();
        fresh.memory.bytes().copy_from_slice(&memory);
        let restored = fresh.command(0);
        eprintln!("empty-dirty restore: reused=refused-unchanged fresh={restored:#x}");
        assert_eq!(restored, 0xaaaa);
    }

    #[test]
    #[ignore = "live ARM KVM; run pinned to one host CPU"]
    fn executable_bytes_restore_requires_fresh_vm_after_warmed_detour() {
        let _pinned = PinnedCpu::current();
        let instruction = |value: u32| 0xd280_0001 | (value << 5);
        let mut original = Guest::with_read_instruction(instruction(0xaaaa));
        assert_eq!(original.command(0), 0xaaaa);
        let state = original.backend.save().unwrap();
        let memory = original.memory.bytes().to_vec();
        drop(original);
        let mut detour = Guest::with_read_instruction(instruction(0xbbbb));
        for _ in 0..32 {
            assert_eq!(detour.command(0), 0xbbbb);
        }
        detour.backend.drain_dirty_pages().unwrap();
        assert!(detour.backend.drain_dirty_pages().unwrap().is_empty());
        assert!(matches!(
            detour.backend.restore(&state),
            Err(vmm_backend::BackendError::Arm64ReusedVcpuRestoreUnsafe)
        ));
        assert_eq!(detour.command(0), 0xbbbb);
        drop(detour);
        let mut fresh = Guest::with_read_instruction(instruction(0xbbbb));
        fresh.memory.bytes().copy_from_slice(&memory);
        fresh.backend.restore(&state).unwrap();
        assert_eq!(fresh.command(0), 0xaaaa);
    }
}
