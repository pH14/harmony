// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest window — the KVM half.
//!
//! One VM, one vCPU, stock KVM. The vCPU runs the real-mode loop payload from
//! [`crate::guest`] while the work clock counts with `exclude_host` set, so the
//! count is guest execution and nothing else. The same differential the
//! host-side measurement uses judges it against the payload's branch analysis.
//!
//! The same vCPU carries the save/restore fixpoint: read every state component,
//! write it all back, read it again, and compare bytes.

use std::os::fd::AsRawFd;

use kvm_bindings::{Msrs, kvm_msr_entry, kvm_userspace_memory_region, kvm_xsave};
use kvm_ioctls::{Cap, Kvm, VcpuExit, VcpuFd, VmFd};

use crate::guest::{
    GUEST_PHYS, GUEST_RAM_BYTES, GuestError, StateCapture, compare_states, emit_loop_payload,
    guest_oracle_delta,
};
use crate::perf::Scope;
use crate::perf_sys::PerfCounter;
use crate::report::Record;
use crate::stage1::{MeasurementPlan, Stage1Error, interrupts_for_core};

/// `_IOR(KVMIO, 0xcf, struct kvm_xsave)` — read the host-sized XSAVE image. The
/// request number carries the base struct's size; the kernel copies however many
/// bytes `KVM_CAP_XSAVE2` reports.
/// How far past its current value a time base is written when the probe tests
/// whether the write takes effect. Four billion ticks is over a second of even
/// the slowest of them, so no elapsed time can account for the difference — and
/// a fresh vCPU's counters start near zero, which leaves no room to displace
/// one downwards.
const TIME_BASE_DISPLACEMENT: u64 = 1 << 32;

/// The timestamp-counter rate assumed when KVM will not report one. It only
/// scales a sanity bound; the displacement probe is what settles a write.
const NOMINAL_TSC_HZ: u64 = 3_000_000_000;

const KVM_GET_XSAVE2: u64 = ioc(2, 0xAE, 0xCF, size_of::<kvm_xsave>() as u64);
/// `_IOW(KVMIO, 0xa5, struct kvm_xsave)` — write the XSAVE image. One ioctl for
/// both the legacy 4 KiB image and the larger one.
const KVM_SET_XSAVE: u64 = ioc(1, 0xAE, 0xA5, size_of::<kvm_xsave>() as u64);

/// Build a Linux ioctl request number: direction in bits 30-31, size in bits
/// 16-29, type in bits 8-15, number in bits 0-7.
const fn ioc(dir: u64, typ: u64, nr: u64, size: u64) -> u64 {
    (dir << 30) | (size << 16) | (typ << 8) | nr
}

fn kvm_error(what: &str, error: &kvm_ioctls::Error) -> GuestError {
    GuestError::Kvm {
        what: what.to_string(),
        detail: error.to_string(),
    }
}

/// One state component's bytes, as the kernel handed them over.
///
/// # Safety
/// `value` must come from a `kvm-bindings` struct the kernel filled: those are
/// created by `mem::zeroed`, so every byte including padding is initialized.
unsafe fn state_bytes<T>(value: &T) -> Vec<u8> {
    // SAFETY: the caller guarantees every byte of `*value` is initialized, and
    // the slice borrows for the length of one `T`.
    unsafe { std::slice::from_raw_parts(std::ptr::from_ref(value).cast::<u8>(), size_of::<T>()) }
        .to_vec()
}

/// A minimal VM: 64 KiB of guest RAM at physical zero, one vCPU, nothing else.
pub struct GuestWindow {
    vcpu: VcpuFd,
    ram: *mut u8,
    /// Kept so the memslot stays registered for as long as the window lives.
    _vm: VmFd,
    /// The host XSAVE image size, when the host advertises one beyond the legacy
    /// 4 KiB struct.
    xsave2_size: Option<usize>,
    /// The MSRs the host says it can save and restore.
    msr_indices: Vec<u32>,
    /// The indices the host-wide list named that this vCPU refused to be
    /// written, kept so the fixpoint can say what its scope excludes.
    msr_indices_not_owned: Vec<u32>,
}

impl GuestWindow {
    /// Create the VM, map its RAM, and create the vCPU.
    ///
    /// # Errors
    /// [`GuestError::Kvm`] when KVM is absent or any setup call fails.
    pub fn open() -> Result<GuestWindow, GuestError> {
        let kvm = Kvm::new().map_err(|e| kvm_error("opening /dev/kvm", &e))?;
        let vm = kvm
            .create_vm()
            .map_err(|e| kvm_error("KVM_CREATE_VM", &e))?;

        // SAFETY: an anonymous private mapping of a fixed length, at an address
        // the kernel chooses. The result is checked against MAP_FAILED below.
        let ram = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                GUEST_RAM_BYTES,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if std::ptr::eq(ram, libc::MAP_FAILED) {
            return Err(GuestError::Kvm {
                what: "mapping guest RAM".to_string(),
                detail: std::io::Error::last_os_error().to_string(),
            });
        }
        let ram = ram.cast::<u8>();

        let region = kvm_userspace_memory_region {
            slot: 0,
            flags: 0,
            guest_phys_addr: 0,
            memory_size: GUEST_RAM_BYTES as u64,
            userspace_addr: ram as u64,
        };
        // SAFETY: the region names the mapping created just above, which stays
        // alive and unmoved for as long as this window does.
        unsafe { vm.set_user_memory_region(region) }.map_err(|e| {
            // SAFETY: `ram` is the mapping this function made and no one else
            // holds it, because the window is not yet constructed.
            unsafe { libc::munmap(ram.cast(), GUEST_RAM_BYTES) };
            kvm_error("KVM_SET_USER_MEMORY_REGION", &e)
        })?;

        let vcpu = vm
            .create_vcpu(0)
            .map_err(|e| kvm_error("KVM_CREATE_VCPU", &e))?;

        // A vCPU with no CPUID model supports no optional feature, and KVM
        // refuses to write an MSR whose feature the vCPU does not have — while
        // still naming that MSR in the host-wide index list the fixpoint reads.
        // Giving the vCPU the host's supported model makes the state the window
        // captures the state it can also write back.
        #[cfg(target_arch = "x86_64")]
        {
            let cpuid = kvm
                .get_supported_cpuid(kvm_bindings::KVM_MAX_CPUID_ENTRIES)
                .map_err(|e| kvm_error("KVM_GET_SUPPORTED_CPUID", &e))?;
            vcpu.set_cpuid2(&cpuid)
                .map_err(|e| kvm_error("KVM_SET_CPUID2", &e))?;
        }

        let advertised = vm.check_extension_int(Cap::Xsave2);
        let xsave2_size = usize::try_from(advertised)
            .ok()
            .filter(|n| *n > size_of::<kvm_xsave>());
        let listed = kvm
            .get_msr_index_list()
            .map_err(|e| kvm_error("KVM_GET_MSR_INDEX_LIST", &e))?
            .as_slice()
            .to_vec();
        // The index list is the host's: every MSR this kernel can handle for
        // some vCPU. This vCPU owns a subset of it, because an MSR whose
        // feature this window does not configure is one KVM refuses to write.
        // The subset is fixed here, on a vCPU that has not yet run, so the
        // fixpoint's scope is a property of the window rather than a reaction
        // to a refusal partway through a round trip.
        let (msr_indices, msr_indices_not_owned) = partition_owned_msrs(&vcpu, &listed);

        Ok(GuestWindow {
            vcpu,
            ram,
            _vm: vm,
            xsave2_size,
            msr_indices,
            msr_indices_not_owned,
        })
    }

    /// The rate of this vCPU's timestamp counter, in ticks per second.
    ///
    /// KVM reports it in kilohertz. A host that will not say falls back to a
    /// nominal rate; the bound it feeds is a sanity bound, and the displacement
    /// probe is what actually settles whether a write took effect.
    #[must_use]
    pub fn tsc_hz(&self) -> u64 {
        self.vcpu
            .get_tsc_khz()
            .ok()
            .map_or(NOMINAL_TSC_HZ, |khz| u64::from(khz) * 1_000)
    }

    /// Test every time base against what it is declared to be: written a value
    /// far past the one it holds — far enough that no elapsed time could account
    /// for the difference — and read back.
    ///
    /// Returns the registers confirmed read-only, and the failures. A register
    /// [`crate::guest::READ_ONLY_MSRS`] declares is expected to ignore the
    /// write, and taking one would mean the declaration is wrong. Every other
    /// register is expected to take it, and ignoring one is a restore that
    /// failed however `KVM_SET_MSRS` reported it.
    #[must_use]
    pub fn probe_time_base_writes(&self) -> (Vec<String>, Vec<String>) {
        let mut read_only = Vec::new();
        let mut failures = Vec::new();
        for base in &crate::guest::TIME_BASE_MSRS {
            if !self.msr_indices.contains(&base.index) {
                continue;
            }
            let declared = crate::guest::read_only_reason(base.index);
            let Some(held) = self.read_one_msr(base.index) else {
                continue;
            };
            let Some(displaced) = held.checked_add(TIME_BASE_DISPLACEMENT) else {
                continue;
            };
            if !self.write_one_msr(base.index, displaced) {
                failures.push(format!(
                    "{} ({:#010x}): refused a write of {displaced:#x} while holding {held:#x}",
                    base.name, base.index
                ));
                continue;
            }
            let Some(after) = self.read_one_msr(base.index) else {
                continue;
            };
            // The register only advances, so a write it took reads back at or
            // past the displaced value. One it ignored reads back near where it
            // started, four billion ticks short.
            let took_the_write = after >= displaced;
            match (declared, took_the_write) {
                (Some(reason), false) => read_only.push(format!(
                    "{} ({:#010x}): written {displaced:#x}, {TIME_BASE_DISPLACEMENT} past the \
                     {held:#x} it held, and read back {after:#x}, so the write was ignored as \
                     declared. {reason}",
                    base.name, base.index
                )),
                (Some(_), true) => failures.push(format!(
                    "{} ({:#010x}): declared read-only, yet a write of {displaced:#x} took \
                     effect and it read back {after:#x}. The declaration is wrong",
                    base.name, base.index
                )),
                (None, false) => failures.push(format!(
                    "{} ({:#010x}): written {displaced:#x}, which is {TIME_BASE_DISPLACEMENT} \
                     past the {held:#x} it held, and read back {after:#x}. The write was \
                     accepted and ignored, so this register does not restore",
                    base.name, base.index
                )),
                (None, true) => {}
            }
        }
        (read_only, failures)
    }

    /// One MSR's current value, or nothing if this vCPU will not read it.
    fn read_one_msr(&self, index: u32) -> Option<u64> {
        let entry = kvm_msr_entry {
            index,
            ..Default::default()
        };
        let mut msrs = Msrs::from_entries(&[entry]).ok()?;
        (self.vcpu.get_msrs(&mut msrs) == Ok(1))
            .then(|| msrs.as_slice().first().map(|e| e.data))
            .flatten()
    }

    /// Write one MSR, reporting whether the ioctl took it.
    fn write_one_msr(&self, index: u32, data: u64) -> bool {
        let entry = kvm_msr_entry {
            index,
            data,
            ..Default::default()
        };
        Msrs::from_entries(&[entry]).is_ok_and(|msrs| self.vcpu.set_msrs(&msrs) == Ok(1))
    }

    /// The MSRs the host-wide index list names that this vCPU does not own, in
    /// the order the list named them.
    #[must_use]
    pub fn msrs_not_owned(&self) -> Vec<String> {
        self.msr_indices_not_owned
            .iter()
            .map(|index| format!("{index:#010x}"))
            .collect()
    }

    /// Place the loop payload and point the vCPU at it, in real mode.
    fn arm(&self, n: u32) -> Result<(), GuestError> {
        let payload = emit_loop_payload(n);
        // SAFETY: `ram` is this window's live mapping of GUEST_RAM_BYTES, and
        // the payload fits below that length at GUEST_PHYS — asserted by a unit
        // test on the portable constants.
        unsafe {
            std::ptr::copy_nonoverlapping(
                payload.as_ptr(),
                self.ram.add(GUEST_PHYS as usize),
                payload.len(),
            );
        }
        let mut sregs = self
            .vcpu
            .get_sregs()
            .map_err(|e| kvm_error("KVM_GET_SREGS", &e))?;
        sregs.cs.base = 0;
        sregs.cs.selector = 0;
        self.vcpu
            .set_sregs(&sregs)
            .map_err(|e| kvm_error("KVM_SET_SREGS", &e))?;

        let regs = kvm_bindings::kvm_regs {
            rip: GUEST_PHYS,
            // Bit 1 is reserved and reads as one; nothing else is set.
            rflags: 0x2,
            ..Default::default()
        };
        self.vcpu
            .set_regs(&regs)
            .map_err(|e| kvm_error("KVM_SET_REGS", &e))
    }

    /// Run until the guest halts. Any other exit is a fault, not a completed
    /// payload.
    fn run_to_halt(&mut self) -> Result<(), GuestError> {
        loop {
            match self.vcpu.run() {
                Ok(VcpuExit::Hlt) => return Ok(()),
                Ok(VcpuExit::Intr) => {}
                Ok(other) => {
                    return Err(GuestError::NotHalted {
                        reason: exit_reason(&other),
                    });
                }
                Err(e) if e.errno() == libc::EINTR => {}
                Err(e) => return Err(kvm_error("KVM_RUN", &e)),
            }
        }
    }

    /// Read every state component the fixpoint covers.
    ///
    /// # Errors
    /// [`GuestError::Kvm`] when a read fails.
    pub fn save_state(&self) -> Result<StateCapture, GuestError> {
        let mut capture = StateCapture::default();
        let regs = self
            .vcpu
            .get_regs()
            .map_err(|e| kvm_error("KVM_GET_REGS", &e))?;
        // SAFETY: `kvm-bindings` structs are created by `mem::zeroed` and filled
        // by the kernel, so every byte including padding is initialized. The
        // same holds for each component below.
        capture.push("regs", unsafe { state_bytes(&regs) });

        let sregs = self
            .vcpu
            .get_sregs()
            .map_err(|e| kvm_error("KVM_GET_SREGS", &e))?;
        capture.push("sregs", unsafe { state_bytes(&sregs) });

        capture.push("xsave", self.save_xsave()?);

        let xcrs = self
            .vcpu
            .get_xcrs()
            .map_err(|e| kvm_error("KVM_GET_XCRS", &e))?;
        capture.push("xcrs", unsafe { state_bytes(&xcrs) });

        capture.push("msrs", self.save_msrs()?);

        let events = self
            .vcpu
            .get_vcpu_events()
            .map_err(|e| kvm_error("KVM_GET_VCPU_EVENTS", &e))?;
        capture.push("vcpu-events", unsafe { state_bytes(&events) });

        Ok(capture)
    }

    /// Write a saved state back.
    ///
    /// # Errors
    /// [`GuestError::IncompleteCapture`] when the capture does not cover every
    /// required component, [`GuestError::Kvm`] when a write fails.
    pub fn restore_state(&self, capture: &StateCapture) -> Result<(), GuestError> {
        let missing = capture.missing();
        if !missing.is_empty() {
            return Err(GuestError::IncompleteCapture { missing });
        }
        for component in &capture.components {
            match component.name {
                "regs" => {
                    let regs = from_state_bytes::<kvm_bindings::kvm_regs>("regs", component)?;
                    self.vcpu
                        .set_regs(&regs)
                        .map_err(|e| kvm_error("KVM_SET_REGS", &e))?;
                }
                "sregs" => {
                    let sregs = from_state_bytes::<kvm_bindings::kvm_sregs>("sregs", component)?;
                    self.vcpu
                        .set_sregs(&sregs)
                        .map_err(|e| kvm_error("KVM_SET_SREGS", &e))?;
                }
                "xsave" => self.restore_xsave(&component.bytes)?,
                "xcrs" => {
                    let xcrs = from_state_bytes::<kvm_bindings::kvm_xcrs>("xcrs", component)?;
                    self.vcpu
                        .set_xcrs(&xcrs)
                        .map_err(|e| kvm_error("KVM_SET_XCRS", &e))?;
                }
                "msrs" => self.restore_msrs(&component.bytes)?,
                "vcpu-events" => {
                    let events = from_state_bytes::<kvm_bindings::kvm_vcpu_events>(
                        "vcpu-events",
                        component,
                    )?;
                    self.vcpu
                        .set_vcpu_events(&events)
                        .map_err(|e| kvm_error("KVM_SET_VCPU_EVENTS", &e))?;
                }
                other => {
                    return Err(GuestError::Kvm {
                        what: format!("restoring state component {other:?}"),
                        detail: "no writer for this component".to_string(),
                    });
                }
            }
        }
        Ok(())
    }

    /// The host-sized XSAVE image where the host advertises one, else the legacy
    /// 4 KiB image.
    fn save_xsave(&self) -> Result<Vec<u8>, GuestError> {
        let Some(len) = self.xsave2_size else {
            let xsave = self
                .vcpu
                .get_xsave()
                .map_err(|e| kvm_error("KVM_GET_XSAVE", &e))?;
            // SAFETY: filled by the kernel into a zeroed struct.
            return Ok(unsafe { state_bytes(&xsave) });
        };
        let mut bytes = vec![0u8; len];
        // SAFETY: the ioctl writes exactly the `KVM_CAP_XSAVE2`-advertised number
        // of bytes, which is `len`, into a buffer of that length.
        let rc = unsafe {
            libc::ioctl(
                self.vcpu.as_raw_fd(),
                KVM_GET_XSAVE2 as libc::c_ulong,
                bytes.as_mut_ptr(),
            )
        };
        if rc < 0 {
            return Err(GuestError::Kvm {
                what: "KVM_GET_XSAVE2".to_string(),
                detail: std::io::Error::last_os_error().to_string(),
            });
        }
        Ok(bytes)
    }

    /// Write an XSAVE image back at whatever size it was saved at.
    fn restore_xsave(&self, bytes: &[u8]) -> Result<(), GuestError> {
        let expect = self.xsave2_size.unwrap_or(size_of::<kvm_xsave>());
        if bytes.len() != expect {
            return Err(GuestError::Kvm {
                what: "KVM_SET_XSAVE".to_string(),
                detail: format!("{} saved bytes, but this host wants {expect}", bytes.len()),
            });
        }
        // SAFETY: the ioctl reads the host XSAVE size from `bytes`, whose length
        // was just checked to be exactly that size.
        let rc = unsafe {
            libc::ioctl(
                self.vcpu.as_raw_fd(),
                KVM_SET_XSAVE as libc::c_ulong,
                bytes.as_ptr(),
            )
        };
        if rc < 0 {
            return Err(GuestError::Kvm {
                what: "KVM_SET_XSAVE".to_string(),
                detail: std::io::Error::last_os_error().to_string(),
            });
        }
        Ok(())
    }

    /// Every MSR the host's index list names, as `index,data` pairs.
    fn save_msrs(&self) -> Result<Vec<u8>, GuestError> {
        if self.msr_indices.is_empty() {
            return Ok(Vec::new());
        }
        let entries: Vec<kvm_msr_entry> = self
            .msr_indices
            .iter()
            .map(|index| kvm_msr_entry {
                index: *index,
                ..Default::default()
            })
            .collect();
        let mut msrs = Msrs::from_entries(&entries).map_err(|e| GuestError::Kvm {
            what: "building the MSR list".to_string(),
            detail: format!("{e:?}"),
        })?;
        let read = self
            .vcpu
            .get_msrs(&mut msrs)
            .map_err(|e| kvm_error("KVM_GET_MSRS", &e))?;
        // A short read leaves the tail of the list at its default, which would
        // round-trip as a stable zero and read as a fixpoint over MSRs that were
        // never captured.
        if read != entries.len() {
            return Err(GuestError::Kvm {
                what: "KVM_GET_MSRS".to_string(),
                detail: format!(
                    "{read} of {} MSRs were read; the read stopped at {}",
                    entries.len(),
                    msr_name(&entries, read)
                ),
            });
        }
        Ok(encode_msrs(msrs.as_slice()))
    }

    /// Write back the MSRs a save captured.
    fn restore_msrs(&self, bytes: &[u8]) -> Result<(), GuestError> {
        let pairs = decode_msrs(bytes)?;
        if pairs.is_empty() {
            return Ok(());
        }
        let entries: Vec<kvm_msr_entry> = pairs
            .iter()
            .map(|(index, data)| kvm_msr_entry {
                index: *index,
                data: *data,
                ..Default::default()
            })
            .collect();
        let msrs = Msrs::from_entries(&entries).map_err(|e| GuestError::Kvm {
            what: "building the MSR list".to_string(),
            detail: format!("{e:?}"),
        })?;
        let written = self
            .vcpu
            .set_msrs(&msrs)
            .map_err(|e| kvm_error("KVM_SET_MSRS", &e))?;
        if written != entries.len() {
            return Err(GuestError::Kvm {
                what: "KVM_SET_MSRS".to_string(),
                detail: format!(
                    "{written} of {} MSRs were written; the write stopped at {}",
                    entries.len(),
                    msr_name(&entries, written)
                ),
            });
        }
        Ok(())
    }
}

impl Drop for GuestWindow {
    fn drop(&mut self) {
        // SAFETY: `ram` is this window's own mapping and nothing else holds it
        // once the vCPU and VM fds above have been dropped.
        unsafe { libc::munmap(self.ram.cast(), GUEST_RAM_BYTES) };
    }
}

/// The MSR a partial `KVM_GET_MSRS`/`KVM_SET_MSRS` stopped at. Both ioctls walk
/// the list in order and return how many they got through, so the entry at that
/// position is the one that refused.
/// Split the host's MSR index list into the ones this vCPU owns and the ones it
/// does not, by writing each MSR the value it already holds.
///
/// A write of an MSR's own value changes nothing when it succeeds. A refusal
/// means the vCPU has no such register: KVM gates the paravirtual MSRs on
/// features this window does not configure, while still naming them in the
/// host-wide list.
fn partition_owned_msrs(vcpu: &VcpuFd, listed: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let mut owned = Vec::with_capacity(listed.len());
    let mut refused = Vec::new();
    for index in listed {
        if msr_round_trips(vcpu, *index) {
            owned.push(*index);
        } else {
            refused.push(*index);
        }
    }
    (owned, refused)
}

/// Whether this vCPU both reads and writes back one MSR.
fn msr_round_trips(vcpu: &VcpuFd, index: u32) -> bool {
    let entry = kvm_msr_entry {
        index,
        ..Default::default()
    };
    let Ok(mut read) = Msrs::from_entries(&[entry]) else {
        return false;
    };
    if vcpu.get_msrs(&mut read) != Ok(1) {
        return false;
    }
    let Some(held) = read.as_slice().first().copied() else {
        return false;
    };
    let Ok(write) = Msrs::from_entries(&[held]) else {
        return false;
    };
    vcpu.set_msrs(&write) == Ok(1)
}

fn msr_name(entries: &[kvm_msr_entry], stopped_at: usize) -> String {
    entries.get(stopped_at).map_or_else(
        || "no entry: the count is past the end of the list".to_string(),
        |entry| format!("MSR {:#010x}", entry.index),
    )
}

/// An MSR list as bytes: each entry is a four-byte index then an eight-byte
/// value, little-endian. Fixed-width so a byte comparison over the whole
/// component is a comparison of every MSR.
fn encode_msrs(entries: &[kvm_msr_entry]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(entries.len() * 12);
    for entry in entries {
        bytes.extend_from_slice(&entry.index.to_le_bytes());
        bytes.extend_from_slice(&entry.data.to_le_bytes());
    }
    bytes
}

/// Read back what [`encode_msrs`] wrote.
fn decode_msrs(bytes: &[u8]) -> Result<Vec<(u32, u64)>, GuestError> {
    crate::guest::decode_msr_pairs(bytes)
}

/// Rebuild a state component from its saved bytes.
fn from_state_bytes<T: Default>(
    name: &str,
    component: &crate::guest::StateComponent,
) -> Result<T, GuestError> {
    if component.bytes.len() != size_of::<T>() {
        return Err(GuestError::Kvm {
            what: format!("restoring state component {name:?}"),
            detail: format!(
                "{} saved bytes, but this host's struct is {}",
                component.bytes.len(),
                size_of::<T>()
            ),
        });
    }
    let mut value = T::default();
    // SAFETY: `value` is a freshly zeroed `T` and `component.bytes` was just
    // checked to hold exactly `size_of::<T>()` bytes; the two do not overlap.
    unsafe {
        std::ptr::copy_nonoverlapping(
            component.bytes.as_ptr(),
            std::ptr::from_mut(&mut value).cast::<u8>(),
            size_of::<T>(),
        );
    }
    Ok(value)
}

/// The `kvm_run` exit reason behind an exit the measurement cannot use.
fn exit_reason(exit: &VcpuExit<'_>) -> u32 {
    match exit {
        VcpuExit::Unknown => 0,
        VcpuExit::IoIn(..) | VcpuExit::IoOut(..) => 2,
        VcpuExit::MmioRead(..) | VcpuExit::MmioWrite(..) => 6,
        VcpuExit::Shutdown => 8,
        VcpuExit::FailEntry(..) => 9,
        VcpuExit::InternalError => 17,
        // Every other exit is real but not one this payload can produce; the
        // number is not what the report acts on, the refusal is.
        _ => u32::MAX,
    }
}

/// Measure count exactness inside the guest: the same differential the
/// host-side measurement uses, with the work clock filtered to guest execution.
///
/// # Errors
/// [`Stage1Error::BadScales`] when the differential is degenerate, and any
/// counter, KVM, or read refusal.
pub fn measure_guest_exactness(
    config: u64,
    plan: &MeasurementPlan,
) -> Result<Vec<Record>, Stage1Error> {
    if plan.n1 >= plan.n2 {
        return Err(Stage1Error::BadScales {
            payload: "guest_loop".to_string(),
            n1: plan.n1,
            n2: plan.n2,
        });
    }
    let mut window = GuestWindow::open().map_err(guest_to_stage1)?;
    let counter = PerfCounter::open_counting(config, Scope::GuestOnly)?;
    let mut records = Vec::new();
    for rep in 0..plan.reps {
        let mut counts = [0u64; 2];
        let mut irqs = [0u64; 2];
        let mut multiplexed = false;
        for (slot, n) in [plan.n1, plan.n2].into_iter().enumerate() {
            let iterations = u32::try_from(n).map_err(|_| Stage1Error::Unavailable {
                what: "the guest loop payload".to_string(),
                detail: format!("{n} iterations does not fit the payload's 32-bit counter"),
            })?;
            window.arm(iterations).map_err(guest_to_stage1)?;
            let before = interrupts_on_core(plan.core)?;
            counter.reset()?;
            counter.enable()?;
            let ran = window.run_to_halt();
            counter.disable()?;
            let after = interrupts_on_core(plan.core)?;
            ran.map_err(guest_to_stage1)?;
            let read = counter.read_timed()?;
            counts[slot] = read.value;
            irqs[slot] = after.saturating_sub(before);
            multiplexed = multiplexed || read.multiplexed();
        }
        records.push(Record::Exactness {
            payload: "guest_loop".to_string(),
            condition: "guest".to_string(),
            rep,
            n1: plan.n1,
            n2: plan.n2,
            count_n1: counts[0],
            count_n2: counts[1],
            oracle_delta: guest_oracle_delta(plan.n1, plan.n2),
            // Each iteration retires exactly one taken branch, and the run's
            // final iteration falls through: the differential cancels the
            // difference, which is why this is the per-iteration count.
            events_per_iteration: 1,
            multiplexed,
            irqs_n1: irqs[0],
            irqs_n2: irqs[1],
        });
    }
    Ok(records)
}

/// Save the vCPU state, write it back, save again, and compare.
///
/// What this demonstrates, and what it does not. The round trip restores a state
/// onto the vCPU that already holds it, so for a register that holds still, a
/// write that took effect and a write that was silently dropped look the same.
/// The registers that move on their own are the ones where the difference shows,
/// and those are written a displaced value and read back, which settles it. A
/// register that neither moves on its own nor is displaced is covered by the
/// byte comparison alone: its state reproduces, and whether the restore path
/// mechanically reached it is not established here.
///
/// The must-restore set is the host's MSR index list less two kinds of register,
/// each named in the record with its reason: the ones this vCPU does not own,
/// which KVM refuses outright, and the ones
/// [`crate::guest::READ_ONLY_MSRS`] declares read-only on the kernel's own
/// statement that a host write to them is discarded.
///
/// # Errors
/// Any KVM refusal from the round trip.
pub fn measure_fixpoint() -> Result<Record, Stage1Error> {
    let mut window = GuestWindow::open().map_err(guest_to_stage1)?;
    // Run the payload first so the state being round-tripped is a vCPU that has
    // executed, not one that has only been created.
    window.arm(1_000).map_err(guest_to_stage1)?;
    window.run_to_halt().map_err(guest_to_stage1)?;

    let first = window.save_state().map_err(guest_to_stage1)?;
    // not order-observable: the elapsed time bounds how far a free-running
    // register can have moved between the restore and the save after it. It
    // reaches the bound in a Fixpoint record and nothing else.
    #[allow(clippy::disallowed_methods)]
    let started = std::time::Instant::now();
    window.restore_state(&first).map_err(guest_to_stage1)?;
    let second = window.save_state().map_err(guest_to_stage1)?;
    #[allow(clippy::disallowed_methods)]
    let elapsed_nanos = started.elapsed().as_nanos();

    let diff = compare_states(&first, &second);
    let tsc_hz = window.tsc_hz();
    let mut differing = diff.differing;
    let mut time_bases = Vec::new();
    for reading in &diff.time_bases {
        // A register that merely advanced proves nothing: the host's own clock
        // advances too, so an ignored write reads back as a plausible advance.
        // The advance must fit in the time the round trip took at the
        // register's own rate, doubled for the coarseness of the two clocks.
        let bound = reading.base.ticks_in(elapsed_nanos, tsc_hz) * 2 + 1_000;
        time_bases.push(reading.describe(bound));
        if u128::from(reading.advance()) > bound {
            differing.push(format!(
                "{} ({:#010x}): advanced {} across a round trip that took {elapsed_nanos}ns, \
                 more than the {bound} its own rate allows, so the value written back did \
                 not take effect",
                reading.base.name,
                reading.base.index,
                reading.advance()
            ));
        }
    }
    // The bound above is a weak discriminator when the round trip is short:
    // an ignored write and an honoured one differ only by the cost of the
    // restore. Writing a value the register cannot have reached on its own
    // settles it — the read-back either reflects the displacement or it does
    // not.
    let (read_only, write_failures) = window.probe_time_base_writes();
    differing.extend(write_failures);

    Ok(Record::Fixpoint {
        components: first.names(),
        missing: first.missing(),
        first_bytes: first.total_bytes(),
        second_bytes: second.total_bytes(),
        differing,
        time_bases,
        read_only,
        msrs_not_owned: window.msrs_not_owned(),
    })
}

fn interrupts_on_core(core: usize) -> Result<u64, Stage1Error> {
    let text = std::fs::read_to_string("/proc/interrupts").map_err(|e| Stage1Error::Read {
        what: "the interrupt counters (/proc/interrupts)".to_string(),
        detail: e.to_string(),
    })?;
    Ok(interrupts_for_core(&text, core))
}

fn guest_to_stage1(error: GuestError) -> Stage1Error {
    Stage1Error::Unavailable {
        what: "the guest window".to_string(),
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_msr_list_round_trips_through_its_byte_encoding() {
        let entries = [
            kvm_msr_entry {
                index: 0x1b,
                data: 0xfee0_0900,
                ..Default::default()
            },
            kvm_msr_entry {
                index: 0xc000_0080,
                data: 0xd01,
                ..Default::default()
            },
        ];
        let bytes = encode_msrs(&entries);
        assert_eq!(bytes.len(), 24);
        assert_eq!(
            decode_msrs(&bytes).expect("whole entries"),
            vec![(0x1b, 0xfee0_0900), (0xc000_0080, 0xd01)]
        );
    }

    #[test]
    fn a_truncated_msr_component_is_refused_rather_than_read_short() {
        let bytes = vec![0u8; 13];
        assert!(decode_msrs(&bytes).is_err());
        assert!(decode_msrs(&[]).expect("empty is whole").is_empty());
    }

    #[test]
    fn a_component_of_the_right_length_restores_the_bytes_it_was_saved_from() {
        let saved = kvm_bindings::kvm_regs {
            rip: 0x1000,
            rflags: 0x2,
            ..Default::default()
        };
        // SAFETY: `kvm-bindings` structs are zeroed on construction, so every
        // byte including padding is initialized.
        let bytes = unsafe { state_bytes(&saved) };
        let component = crate::guest::StateComponent {
            name: "regs",
            bytes,
        };
        let restored =
            from_state_bytes::<kvm_bindings::kvm_regs>("regs", &component).expect("same length");
        assert_eq!(restored.rip, 0x1000);
        assert_eq!(restored.rflags, 0x2);
        assert_eq!(restored.rax, 0);
    }

    #[test]
    fn a_component_of_the_wrong_length_is_refused_rather_than_reinterpreted() {
        let component = crate::guest::StateComponent {
            name: "regs",
            bytes: vec![0u8; 3],
        };
        let error = from_state_bytes::<kvm_bindings::kvm_regs>("regs", &component)
            .expect_err("three bytes is not a register set");
        assert!(error.to_string().contains('3'), "{error}");
    }

    #[test]
    fn the_ioctl_encoding_matches_the_numbers_the_uapi_defines() {
        // _IO(KVMIO, 0x80): direction none, size zero.
        assert_eq!(ioc(0, 0xAE, 0x80, 0), 0x0000_AE80);
        // _IOR(KVMIO, 0xcf, struct kvm_xsave): read, 4096 bytes.
        assert_eq!(ioc(2, 0xAE, 0xCF, 4096), 0x9000_AECF);
    }
}
