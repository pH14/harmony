// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]

use std::sync::Mutex;
use std::time::Duration;

use vmm_backend::{
    Arm64Policy, Arm64VcpuState, Backend, BackendError, CommonExit, Exit, Gpa, HvfBackend,
    IdRegModel,
};

const RAM_GPA: u64 = 0x4000_0000;
const RAM_BYTES: usize = 0x10_0000;
const MMIO_GPA: u64 = 0x0900_0000;

const OFF_CODE: usize = 0x0000;
const OFF_L1: usize = 0x1000;
const OFF_L2_RAM: usize = 0x2000;
const OFF_L3: usize = 0x3000;
const OFF_PAGE_A: usize = 0x4000;
const OFF_PAGE_B: usize = 0x5000;
const OFF_L2_MMIO: usize = 0x6000;
const OFF_SCRATCH: usize = 0x8000;

const PROBE_L2_INDEX: usize = 1;
const MMIO_L2_INDEX: usize = 72;
const PROBE_VA: u64 = 0x4020_0000;

const VALUE_A: u64 = 0xaaaa;
const VALUE_B: u64 = 0xbbbb;
const ASID: u64 = 1;

const SCTLR_M: u64 = 1;
const SCTLR_C: u64 = 1 << 2;
const SCTLR_I: u64 = 1 << 12;
const CPACR_FPEN: u64 = 0b11 << 20;
const PSTATE_EL1H_MASKED: u64 = 0x3c5;

const DESC_TABLE: u64 = 0b11;
const DESC_BLOCK: u64 = 0b01;
const DESC_PAGE: u64 = 0b11;
const ATTR_NORMAL: u64 = (1 << 10) | (3 << 8);
const ATTR_DEVICE: u64 = (1 << 54) | (1 << 53) | (1 << 10) | (1 << 2);

const MAIR: u64 = 0xff;
const TCR: u64 =
    25 | (1 << 8) | (1 << 10) | (3 << 12) | (25 << 16) | (1 << 23) | (2 << 30) | (2 << 32);

const MRS_CNTVCT_X0: u32 = 0xd53b_e040;
const TLBI_VMALLE1IS: u32 = 0xd508_831f;
const DSB_ISH: u32 = 0xd503_3b9f;
const ISB: u32 = 0xd503_3fdf;
const B_SELF: u32 = 0x1400_0000;

const WARM_ROUNDS: usize = 64;
const HOST_PAUSE: Duration = Duration::from_millis(20);
const COUNTER_SETTLE_TICKS: u64 = 1 << 20;
const ROUNDS: usize = 96;
const BRANCH_AT: usize = 37;

static ONE_VM_PER_PROCESS: Mutex<()> = Mutex::new(());

fn movz(rd: u32, imm: u32, shift: u32) -> u32 {
    0xd280_0000 | ((shift / 16) << 21) | (imm << 5) | rd
}

fn movk(rd: u32, imm: u32, shift: u32) -> u32 {
    0xf280_0000 | ((shift / 16) << 21) | (imm << 5) | rd
}

fn add(rd: u32, rn: u32, rm: u32) -> u32 {
    0x8b00_0000 | (rm << 16) | (rn << 5) | rd
}

fn eor(rd: u32, rn: u32, rm: u32) -> u32 {
    0xca00_0000 | (rm << 16) | (rn << 5) | rd
}

fn mul(rd: u32, rn: u32, rm: u32) -> u32 {
    0x9b00_7c00 | (rm << 16) | (rn << 5) | rd
}

fn str_x(rt: u32, rn: u32) -> u32 {
    0xf900_0000 | (rn << 5) | rt
}

fn ldr_x(rt: u32, rn: u32) -> u32 {
    0xf940_0000 | (rn << 5) | rt
}

fn add_2d(rd: u32, rn: u32, rm: u32) -> u32 {
    0x4ee0_8400 | (rm << 16) | (rn << 5) | rd
}

fn umov_low_lane(rd: u32, rn: u32) -> u32 {
    0x4e08_3c00 | (rn << 5) | rd
}

fn branch_back(words: usize) -> u32 {
    B_SELF | ((-(words as i32)) as u32 & 0x03ff_ffff)
}

fn read_page(flush_first: bool) -> Vec<u32> {
    let mut code = vec![
        movz(1, (PROBE_VA >> 16) as u32, 16),
        movz(2, (MMIO_GPA >> 16) as u32, 16),
    ];
    if flush_first {
        code.extend([TLBI_VMALLE1IS, DSB_ISH, ISB]);
    }
    code.extend([ldr_x(0, 1), str_x(0, 2), B_SELF]);
    code
}

fn read_counter() -> Vec<u32> {
    vec![
        movz(1, (MMIO_GPA >> 16) as u32, 16),
        MRS_CNTVCT_X0,
        str_x(0, 1),
        branch_back(2),
    ]
}

fn mixed_work() -> Vec<u32> {
    let scratch = RAM_GPA + OFF_SCRATCH as u64;
    let mut code = vec![
        movz(1, 0x9e37, 48),
        movk(1, 0x79b9, 32),
        movk(1, 0x7f4a, 16),
        movk(1, 0x7c15, 0),
        movz(2, (scratch >> 16) as u32 & 0xffff, 16),
        movk(2, scratch as u32 & 0xffff, 0),
        movz(9, (MMIO_GPA >> 16) as u32 & 0xffff, 16),
        movz(3, 0x1234, 0),
        movz(4, 0x5678, 0),
        movz(5, 0x9abc, 0),
        movz(6, 0x0001, 0),
        movz(8, 0x0000, 0),
    ];
    let body = [
        add(3, 3, 1),
        eor(4, 4, 3),
        mul(5, 5, 1),
        add(6, 6, 4),
        str_x(3, 2),
        ldr_x(7, 2),
        add(8, 8, 7),
        eor(8, 8, 5),
        add_2d(0, 0, 1),
        umov_low_lane(10, 0),
        add(8, 8, 10),
        str_x(8, 9),
    ];
    code.extend(body);
    code.push(branch_back(body.len()));
    code
}

struct GuestMem {
    ptr: *mut u8,
    layout: std::alloc::Layout,
}

impl GuestMem {
    fn new() -> Self {
        let layout = std::alloc::Layout::from_size_align(RAM_BYTES, 16 * 1024).expect("layout");
        // SAFETY: the layout has a non-zero size and a power-of-two alignment.
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "guest RAM allocation failed");
        Self { ptr, layout }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `ptr` came from `alloc_zeroed` with `RAM_BYTES`, and the
        // exclusive borrow of `self` makes this the only live reference.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, RAM_BYTES) }
    }

    fn write_u64(&mut self, offset: usize, value: u64) {
        self.as_mut_slice()[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn write_code(&mut self, words: &[u32]) {
        for (slot, word) in words.iter().enumerate() {
            let at = OFF_CODE + slot * 4;
            self.as_mut_slice()[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
    }
}

impl Drop for GuestMem {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `alloc_zeroed` with this layout, and `Guest`
        // drops the backend that mapped it before this memory.
        unsafe { std::alloc::dealloc(self.ptr, self.layout) };
    }
}

struct Guest {
    backend: HvfBackend,
    mem: GuestMem,
    start: Arm64VcpuState,
    _vm: std::sync::MutexGuard<'static, ()>,
}

impl Guest {
    fn new(program: &[u32]) -> Self {
        let vm = ONE_VM_PER_PROCESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut mem = GuestMem::new();
        mem.write_u64(OFF_L1, (RAM_GPA + OFF_L2_MMIO as u64) | DESC_TABLE);
        mem.write_u64(OFF_L1 + 8, (RAM_GPA + OFF_L2_RAM as u64) | DESC_TABLE);
        mem.write_u64(
            OFF_L2_MMIO + MMIO_L2_INDEX * 8,
            MMIO_GPA | ATTR_DEVICE | DESC_BLOCK,
        );
        mem.write_u64(OFF_L2_RAM, RAM_GPA | ATTR_NORMAL | DESC_BLOCK);
        mem.write_u64(
            OFF_L2_RAM + PROBE_L2_INDEX * 8,
            (RAM_GPA + OFF_L3 as u64) | DESC_TABLE,
        );
        mem.write_u64(OFF_PAGE_A, VALUE_A);
        mem.write_u64(OFF_PAGE_B, VALUE_B);
        mem.write_code(program);

        let mut backend = HvfBackend::new()
            .expect("HvfBackend::new needs Apple silicon and the hypervisor entitlement");
        backend
            .set_policy(&Arm64Policy::default())
            .expect("set_policy");
        // SAFETY: `Guest` declares `backend` before `mem`, so the mapping is
        // dropped before the allocation; the allocation never moves; the
        // host writes to it only between runs; and its layout gives the
        // 16 KiB alignment HVF requires.
        unsafe { backend.map_memory(Gpa(RAM_GPA), mem.as_mut_slice()) }.expect("map_memory");
        backend.invalidate_instruction_cache(mem.ptr as usize, RAM_BYTES);

        let mut start = backend.save().expect("save");
        start.core.pc = RAM_GPA + OFF_CODE as u64;
        start.core.pstate = PSTATE_EL1H_MASKED;
        start.sysregs.sctlr_el1 |= SCTLR_M | SCTLR_C | SCTLR_I;
        start.sysregs.cpacr_el1 |= CPACR_FPEN;
        start.sysregs.ttbr0_el1 = (ASID << 48) | (RAM_GPA + OFF_L1 as u64);
        start.sysregs.tcr_el1 = TCR;
        start.sysregs.mair_el1 = MAIR;
        Self {
            backend,
            mem,
            start,
            _vm: vm,
        }
    }

    fn load(&mut self, program: &[u32]) {
        self.mem.write_code(program);
        self.backend
            .invalidate_instruction_cache(self.mem.ptr as usize, RAM_BYTES);
    }

    fn map_probe_page(&mut self, page: usize) {
        self.mem
            .write_u64(OFF_L3, (RAM_GPA + page as u64) | ATTR_NORMAL | DESC_PAGE);
    }

    fn restore(&mut self, state: &Arm64VcpuState) {
        self.backend.restore(state).expect("restore");
    }

    fn restart(&mut self) {
        let start = self.start;
        self.restore(&start);
    }

    fn save(&mut self) -> Arm64VcpuState {
        self.backend.save().expect("save")
    }

    fn next_store(&mut self) -> u64 {
        match self.backend.run() {
            Ok(Exit::Common(CommonExit::Mmio {
                gpa,
                write: Some(value),
                ..
            })) if gpa == Gpa(MMIO_GPA) => value,
            other => panic!("expected a guest store to {MMIO_GPA:#x}, got {other:?}"),
        }
    }

    fn stores(&mut self, count: usize) -> Vec<u64> {
        (0..count).map(|_| self.next_store()).collect()
    }
}

fn warm_translation_to_page_a(guest: &mut Guest) {
    guest.map_probe_page(OFF_PAGE_A);
    guest.load(&read_page(true));
    guest.restart();
    assert_eq!(guest.next_store(), VALUE_A, "first read of page A");
    guest.load(&read_page(false));
    for round in 0..WARM_ROUNDS {
        guest.restart();
        assert_eq!(
            guest.next_store(),
            VALUE_A,
            "warm-up read {round} of page A"
        );
    }
}

#[test]
#[ignore = "live HVF; run on Apple silicon with --ignored (see the vmm-backend README)"]
fn restore_drops_guest_translations_cached_before_it() {
    let mut guest = Guest::new(&read_page(false));

    warm_translation_to_page_a(&mut guest);
    guest.map_probe_page(OFF_PAGE_B);
    guest.load(&read_page(true));
    guest.restart();
    assert_eq!(
        guest.next_store(),
        VALUE_B,
        "the guest cannot see a replaced page table entry even after its own TLB flush"
    );

    warm_translation_to_page_a(&mut guest);
    guest.map_probe_page(OFF_PAGE_B);
    guest.restart();
    assert_eq!(
        guest.next_store(),
        VALUE_B,
        "the guest read through a translation cached before the restore"
    );
}

#[test]
#[ignore = "live HVF; run on Apple silicon with --ignored (see the vmm-backend README)"]
fn restore_puts_the_guest_counter_back() {
    let mut guest = Guest::new(&read_counter());

    guest.restart();
    let before = guest.next_store();
    std::thread::sleep(HOST_PAUSE);
    let continued = guest.next_store().wrapping_sub(before);

    guest.restart();
    let before = guest.next_store();
    std::thread::sleep(HOST_PAUSE);
    guest.restart();
    let rewound = (guest.next_store().wrapping_sub(before) as i64).unsigned_abs();

    assert!(
        continued > 0 && rewound < continued / 4,
        "a restore must return the guest counter to the snapshot's value: it advanced \
         {rewound} ticks across a restore and {continued} ticks without one"
    );
}

#[test]
#[ignore = "live HVF; run on Apple silicon with --ignored (see the vmm-backend README)"]
fn save_and_restore_between_steps_preserve_what_the_guest_computes() {
    let mut guest = Guest::new(&mixed_work());
    for (lane, slot) in guest.start.simd_fp.q.iter_mut().enumerate() {
        slot[0] = lane as u8;
        slot[8] = (lane as u8) ^ 0x5a;
    }

    guest.restart();
    let straight = guest.stores(ROUNDS);

    guest.restart();
    let mut round_tripped = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        round_tripped.push(guest.next_store());
        let here = guest.save();
        guest.restore(&here);
    }
    assert_eq!(
        straight, round_tripped,
        "a save and restore between steps changed the guest's results"
    );

    guest.restart();
    guest.stores(BRANCH_AT);
    let fork = guest.save();
    let first = guest.stores(ROUNDS - BRANCH_AT);
    guest.restore(&fork);
    let second = guest.stores(ROUNDS - BRANCH_AT);
    assert_eq!(first, second, "two branches from one snapshot diverged");
    assert_eq!(
        straight[BRANCH_AT..],
        first[..],
        "a branch from a snapshot differs from the straight run"
    );
}

fn same_state(observed: &Arm64VcpuState, expected: &Arm64VcpuState) -> bool {
    let advance = observed
        .vtimer
        .counter
        .wrapping_sub(expected.vtimer.counter);
    let mut observed = *observed;
    observed.vtimer.counter = expected.vtimer.counter;
    observed == *expected && advance < COUNTER_SETTLE_TICKS
}

type Perturbation = fn(&mut Arm64VcpuState);

#[test]
#[ignore = "live HVF; run on Apple silicon with --ignored (see the vmm-backend README)"]
fn every_vcpu_state_class_survives_a_restore() {
    let mut guest = Guest::new(&[B_SELF]);
    let baseline = guest.save();
    let classes: [(&str, Perturbation); 6] = [
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
            state.vtimer.cntv_ctl_el0 ^= 0b01;
        }),
        ("pending-interrupts", |state| {
            state.interrupts.irq = !state.interrupts.irq;
            state.interrupts.fiq = !state.interrupts.fiq;
        }),
    ];
    for (name, perturb) in classes {
        let mut expected = baseline;
        perturb(&mut expected);
        guest.restore(&expected);
        assert!(
            same_state(&guest.save(), &expected),
            "{name}: the perturbed state did not read back"
        );
        guest.restore(&baseline);
        assert!(
            same_state(&guest.save(), &baseline),
            "{name}: the baseline did not come back"
        );
    }
}

#[test]
#[ignore = "live HVF; run on Apple silicon with --ignored (see the vmm-backend README)"]
fn restore_rejects_a_timer_state_hvf_cannot_hold() {
    let mut guest = Guest::new(&[B_SELF]);
    let baseline = guest.save();
    let invalid: [(&str, Perturbation); 2] = [
        ("unmasked", |state| state.vtimer.masked = false),
        ("control-bits", |state| state.vtimer.cntv_ctl_el0 |= 0b100),
    ];
    for (name, perturb) in invalid {
        let mut state = baseline;
        perturb(&mut state);
        assert!(
            matches!(
                guest.backend.restore(&state),
                Err(BackendError::InvalidState)
            ),
            "{name}: restore accepted a timer state HVF cannot hold"
        );
        assert!(
            same_state(&guest.save(), &baseline),
            "{name}: a rejected restore changed the vCPU"
        );
    }
}

#[test]
#[ignore = "live HVF; run on Apple silicon with --ignored (see the vmm-backend README)"]
fn set_policy_rejects_an_id_field_above_the_host() {
    let _vm = ONE_VM_PER_PROCESS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut backend = HvfBackend::new()
        .expect("HvfBackend::new needs Apple silicon and the hypervisor entitlement");
    let policy = Arm64Policy {
        id_regs: IdRegModel {
            regs: [(0xc038, 0xf << 4)].into_iter().collect(),
        },
        ..Arm64Policy::default()
    };
    assert!(matches!(
        backend.set_policy(&policy),
        Err(BackendError::IdRegisterAboveHost {
            encoding: 0xc038,
            ..
        })
    ));
}

#[test]
#[ignore = "live HVF; run on Apple silicon with --ignored (see the vmm-backend README)"]
fn set_policy_compares_every_policy_with_the_host() {
    let _vm = ONE_VM_PER_PROCESS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut backend = HvfBackend::new()
        .expect("HvfBackend::new needs Apple silicon and the hypervisor entitlement");
    let isar0 = |value: u64| Arm64Policy {
        id_regs: IdRegModel {
            regs: [(0xc030, value)].into_iter().collect(),
        },
        ..Arm64Policy::default()
    };
    backend
        .set_policy(&isar0(0))
        .expect("a policy with every field at zero is within the host");
    backend
        .set_policy(&isar0(0x20))
        .expect("a later policy is checked against the host, not the earlier policy");
}
