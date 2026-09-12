// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    feature = "contract-tests"
))]

use vmm_backend::contract::{BackendFixture, ContractReport, Scenario, run_all};
use vmm_backend::{Backend, CpuidModel, Gpa, KvmBackend, MsrFilter, MsrRange, X86Policy};

const RAM_LEN: usize = 0x1_0000;
const ENTRY: u64 = 0x1000;
const DIRTY_ENTRY: u64 = 0x3000;
const DIRTY_GFNS: [u64; 2] = [2, 5];

struct GuestMem {
    ptr: *mut u8,
    layout: std::alloc::Layout,
    len: usize,
}

impl GuestMem {
    fn new(len: usize) -> Self {
        assert_eq!(len % 4096, 0, "guest RAM must be page-sized");
        let layout = std::alloc::Layout::from_size_align(len, 4096).expect("layout");
        // SAFETY: non-zero size, power-of-two align.
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "guest RAM alloc failed");
        Self { ptr, layout, len }
    }
    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `ptr`/`len` came from `alloc_zeroed`; exclusive borrow.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for GuestMem {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`layout` from `alloc_zeroed`; freed once.
        unsafe { std::alloc::dealloc(self.ptr, self.layout) };
    }
}

fn stub(scenario: Scenario) -> Option<Vec<u8>> {
    const HALT_LOOP: [u8; 3] = [0xF4, 0xEB, 0xFD];
    let head: Vec<u8> = match scenario {
        Scenario::Idle => Vec::new(),
        Scenario::PortIn => vec![0xBA, 0xF8, 0x03, 0xEC],
        Scenario::MmioLoad => return None,
        Scenario::Rdmsr => vec![0x66, 0xB9, 0x78, 0x56, 0x34, 0x12, 0x0F, 0x32],
        Scenario::Wrmsr => vec![
            0x66, 0xB9, 0x78, 0x56, 0x34, 0x12, 0x66, 0x31, 0xC0, 0x66, 0x31, 0xD2, 0x0F, 0x30,
        ],
        Scenario::Cpuid => return None,
        Scenario::Hypercall => return None,
    };
    let mut code = head;
    code.extend_from_slice(&HALT_LOOP);
    Some(code)
}

const DIRTY_STUB: &[u8] = &[
    0xC6, 0x06, 0x00, 0x20, 0x01, 0xC6, 0x06, 0x00, 0x50, 0x01, 0xF4,
];

fn policy() -> X86Policy {
    X86Policy {
        cpuid: CpuidModel::default(),
        msr_filter: MsrFilter {
            allow_inkernel: vec![MsrRange {
                base: 0x174,
                count: 3,
            }],
        },
    }
}

fn require_kvm() {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm missing — the contract exam's hardware leg needs bare-metal x86-64 with \
         VMX. Run it on the determinism box: taskset -c 1 cargo test -p vmm-backend \
         --all-features --test contract_kvm -- --ignored --test-threads=1"
    );
}

trait LiveBackend: Backend<A = vmm_backend::X86> + Sized {
    fn open() -> Self;
    fn enable_dirty_log(&mut self);
    fn load(&mut self, gpa: Gpa, bytes: &[u8]);
    fn map(&mut self, gpa: Gpa, host: &mut [u8]);
}

impl LiveBackend for KvmBackend {
    fn open() -> Self {
        KvmBackend::new().unwrap_or_else(|e| {
            panic!("KvmBackend::new failed ({e}); needs /dev/kvm + VMX on the determinism box")
        })
    }
    fn enable_dirty_log(&mut self) {
        self.set_dirty_log_enabled(true);
    }
    fn load(&mut self, gpa: Gpa, bytes: &[u8]) {
        self.write_guest(gpa, bytes).expect("write_guest");
    }
    fn map(&mut self, gpa: Gpa, host: &mut [u8]) {
        // SAFETY: `host` is the fixture's boxed, page-aligned `GuestMem`, which
        // outlives every backend the exam holds and is not aliased while the
        // guest runs.
        unsafe { self.map_memory(gpa, host) }.expect("map_memory");
    }
}

fn enter_real_mode_at<B: LiveBackend>(backend: &mut B, entry: u64) {
    let mut st = backend.save().expect("save for setup");
    st.sregs.cs.base = 0;
    st.sregs.cs.selector = 0;
    st.sregs.ds.base = 0;
    st.sregs.ds.selector = 0;
    st.regs.rip = entry;
    st.regs.rflags = 0x2;
    backend.restore(&st).expect("restore setup state");
}

struct KvmFixture<B: LiveBackend> {
    name: &'static str,
    mems: Vec<GuestMem>,
    _marker: std::marker::PhantomData<B>,
}

impl<B: LiveBackend> KvmFixture<B> {
    fn new(name: &'static str) -> Self {
        KvmFixture {
            name,
            mems: Vec::new(),
            _marker: std::marker::PhantomData,
        }
    }

    fn boot(&mut self, code: &[u8], entry: u64) -> B {
        let mut backend = B::open();
        backend.enable_dirty_log();
        self.mems.push(GuestMem::new(RAM_LEN));
        let mem = self.mems.last_mut().expect("just pushed");
        backend.map(Gpa(0), mem.as_mut_slice());
        backend.load(Gpa(entry), code);
        enter_real_mode_at(&mut backend, entry);
        backend
    }
}

impl<B: LiveBackend> BackendFixture for KvmFixture<B> {
    type B = B;

    fn name(&self) -> &'static str {
        self.name
    }

    fn spawn(&mut self, scenario: Scenario) -> Option<B> {
        let code = stub(scenario)?;
        Some(self.boot(&code, ENTRY))
    }

    fn policy(&self) -> X86Policy {
        policy()
    }

    fn dirty_pages(&mut self, backend: &mut B) -> Option<Vec<u64>> {
        backend.load(Gpa(DIRTY_ENTRY), DIRTY_STUB);
        enter_real_mode_at(backend, DIRTY_ENTRY);
        backend.run().expect("run the dirty-page stub to its halt");
        Some(DIRTY_GFNS.to_vec())
    }
}

const REQUIRED_EVERYWHERE: &[&str] = &[
    "ordering/not_configured",
    "ordering/completion_grid",
    "exactness/dirty_log",
    "fixpoint/save_restore_save",
    "interrupts/one_overwritable_slot",
];

#[track_caller]
fn assert_ran(report: &ContractReport, exams: &[&'static str]) {
    for exam in exams {
        assert!(
            report.did_run(exam),
            "{exam} did not run against {}: {report:?}",
            report.backend
        );
    }
}

#[test]
#[ignore = "live KVM; run on the determinism box with --ignored (see file header)"]
fn stock_kvm_backend_passes_the_contract_exam() {
    require_kvm();
    let mut fx: KvmFixture<KvmBackend> = KvmFixture::new("kvm-stock");
    let report = run_all(&mut fx);
    println!("[CONTRACT] {report:#?}");

    assert_ran(&report, REQUIRED_EVERYWHERE);
    assert!(
        !report.declined.is_empty(),
        "stock KVM's declines are part of its contract; an empty decline list means the exam \
         stopped recording them"
    );
}
