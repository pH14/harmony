// SPDX-License-Identifier: AGPL-3.0-or-later
//! Empirical probe for the stock-KVM x86 surface on shared hosts (the
//! GitHub-hosted runners of the X0 milestone): CPU identity, `/dev/kvm`
//! access, the KVM capability table, and one minimal real-mode guest run
//! through the public `KvmBackend`.
//!
//! Output is `KEY=VALUE` lines so a watcher can parse results by name; the
//! final line is `PROBE=PASS`, or `PROBE=FAIL` plus `FAIL_REASON=`.

#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
mod x86 {
    use std::fs;
    use std::os::fd::AsRawFd;

    use vmm_backend::{
        Backend, CommonExit, CpuidModel, Exit, Gpa, KvmBackend, MsrFilter, MsrRange, X86Exit,
        X86Policy,
    };

    /// `KVM_CHECK_EXTENSION` (`_IO(KVMIO, 0x03)`), issued per capability id so
    /// the table covers ids the `kvm-ioctls` `Cap` enum does not name.
    const KVM_CHECK_EXTENSION: libc::c_ulong = 0xAE03;

    /// Capability ids the backend uses today plus the ones the x86 bring-up
    /// decisions read (delivery shape, clock control, state save size).
    const CAPS: &[(&str, u32)] = &[
        ("IRQCHIP", 0),
        ("HLT", 1),
        ("USER_MEMORY", 3),
        ("EXT_CPUID", 7),
        ("NR_VCPUS", 9),
        ("NR_MEMSLOTS", 10),
        ("ADJUST_CLOCK", 39),
        ("XSAVE", 55),
        ("TSC_CONTROL", 60),
        ("GET_TSC_KHZ", 61),
        ("MAX_VCPUS", 66),
        ("TSC_DEADLINE_TIMER", 72),
        ("SPLIT_IRQCHIP", 121),
        ("X2APIC_API", 129),
        ("IMMEDIATE_EXIT", 136),
        ("EXCEPTION_PAYLOAD", 164),
        ("X86_USER_SPACE_MSR", 188),
        ("X86_MSR_FILTER", 189),
        ("XSAVE2", 208),
        ("X86_TRIPLE_FAULT_EVENT", 218),
        ("X86_NOTIFY_VMEXIT", 219),
        ("X86_DETERMINISTIC_INTERCEPTS", 245),
    ];

    /// One identity-mapped guest RAM region, page-aligned (the `map_memory`
    /// host alignment invariant), reached by the backend through a raw pointer.
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

    fn cpuinfo_field(cpuinfo: &str, key: &str) -> String {
        cpuinfo
            .lines()
            .find(|l| l.split(':').next().map(str::trim) == Some(key))
            .and_then(|l| l.split(':').nth(1))
            .map(str::trim)
            .unwrap_or("unknown")
            .to_owned()
    }

    fn print_host_identity() {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        println!("CPU_VENDOR={}", cpuinfo_field(&cpuinfo, "vendor_id"));
        println!("CPU_MODEL_NAME={}", cpuinfo_field(&cpuinfo, "model name"));
        println!("CPU_FAMILY={}", cpuinfo_field(&cpuinfo, "cpu family"));
        println!("CPU_MODEL={}", cpuinfo_field(&cpuinfo, "model"));
        println!("CPU_STEPPING={}", cpuinfo_field(&cpuinfo, "stepping"));
        let under_hypervisor = cpuinfo_field(&cpuinfo, "flags")
            .split_whitespace()
            .any(|f| f == "hypervisor");
        println!("HYPERVISOR_FLAG={}", u8::from(under_hypervisor));
        let nproc = std::thread::available_parallelism().map_or(0, std::num::NonZero::get);
        println!("NPROC={nproc}");
        let kernel = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        println!("KERNEL={}", kernel.trim());
    }

    pub fn run() -> Result<(), String> {
        print_host_identity();

        let dev_kvm = std::path::Path::new("/dev/kvm").exists();
        println!("DEV_KVM={}", if dev_kvm { "present" } else { "absent" });
        if !dev_kvm {
            return Err("dev_kvm_absent".to_owned());
        }

        let kvm = kvm_ioctls::Kvm::new().map_err(|e| {
            println!("DEV_KVM_OPEN=err:{e}");
            "kvm_open".to_owned()
        })?;
        println!("DEV_KVM_OPEN=ok");
        let api = kvm.get_api_version();
        println!("KVM_API_VERSION={api}");
        if api != 12 {
            return Err("kvm_api_version".to_owned());
        }
        let fd = kvm.as_raw_fd();
        for (name, id) in CAPS {
            // SAFETY: `KVM_CHECK_EXTENSION` on a valid `/dev/kvm` fd takes the
            // capability id by value and dereferences no userspace pointer.
            let value = unsafe { libc::ioctl(fd, KVM_CHECK_EXTENSION, libc::c_ulong::from(*id)) };
            println!("CAP_{name}={value}");
        }
        drop(kvm);

        // One minimal guest through the public backend, exactly the
        // `kvm_smoke` bring-up shape:
        //   mov dx, 0x3f8 ; mov al, 0x42 ; out dx, al ; hlt
        let code: &[u8] = &[0xBA, 0xF8, 0x03, 0xB0, 0x42, 0xEE, 0xF4];

        // Declared before `backend` so the mapped RAM outlives it.
        let mut mem = GuestMem::new(0x10000);
        let mut backend = KvmBackend::new().map_err(|e| {
            println!("BACKEND_NEW=err:{e}");
            "backend_new".to_owned()
        })?;
        println!("BACKEND_NEW=ok");

        // SAFETY: `mem` is page-aligned, outlives `backend`, and is not
        // touched from this thread while the guest runs.
        unsafe { backend.map_memory(Gpa(0), mem.as_mut_slice()) }
            .map_err(|e| format!("map_memory:{e}"))?;
        backend
            .set_policy(&X86Policy {
                cpuid: CpuidModel::default(),
                msr_filter: MsrFilter {
                    // SYSENTER MSRs (0x174..0x177) — present, harmless, in-kernel.
                    allow_inkernel: vec![MsrRange {
                        base: 0x174,
                        count: 3,
                    }],
                },
            })
            .map_err(|e| format!("set_policy:{e}"))?;
        backend
            .write_guest(Gpa(0x1000), code)
            .map_err(|e| format!("write_guest:{e}"))?;

        let mut st = backend.save().map_err(|e| format!("save:{e}"))?;
        st.sregs.cs.base = 0;
        st.sregs.cs.selector = 0;
        st.regs.rip = 0x1000;
        st.regs.rflags = 0x2;
        backend.restore(&st).map_err(|e| format!("restore:{e}"))?;

        match backend.run().map_err(|e| format!("run_io:{e}"))? {
            Exit::Arch(X86Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(0x42),
            }) => println!("GUEST_IO_EXIT=ok"),
            other => {
                println!("GUEST_IO_EXIT=unexpected:{other:?}");
                return Err("guest_io".to_owned());
            }
        }
        match backend.run().map_err(|e| format!("run_hlt:{e}"))? {
            Exit::Common(CommonExit::Idle) => println!("GUEST_HLT_EXIT=ok"),
            other => {
                println!("GUEST_HLT_EXIT=unexpected:{other:?}");
                return Err("guest_hlt".to_owned());
            }
        }
        let counts = backend.exit_counts();
        println!("EXITS_IO={}", counts.io);
        println!("EXITS_IDLE={}", counts.idle);
        println!("EXITS_TOTAL={}", counts.total());
        println!("BACKEND_NAME={}", backend.capabilities().name);
        println!("PROBE=PASS");
        Ok(())
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
fn main() {
    if let Err(reason) = x86::run() {
        println!("PROBE=FAIL");
        println!("FAIL_REASON={reason}");
        std::process::exit(1);
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64", not(miri))))]
fn main() {
    eprintln!("x86_kvm_probe requires a Linux x86-64 host outside Miri");
    std::process::exit(2);
}
