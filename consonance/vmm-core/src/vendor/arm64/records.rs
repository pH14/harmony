// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arm64 `vm_state` record-set glue: conversions between the live
//! [`vmm_backend::Arm64VcpuState`] and `vm-state`'s arm64 plain-data records,
//! plus the vmm-core-owned arm64 device blob (the `vm_state::DeviceBlob`
//! payload).
//!
//! The record sets mirror one another field-for-field (rule #2 keeps the two
//! crates dependency-free; consistency by review), so the conversions are flat
//! copies — the arm64 analogue of `vendor::x86::records`' `to_vm_*`/`from_vm_*`
//! adapters. The record set is the **skeleton subset**; `TODO(AA-6)` owns the
//! full sysreg set and both sides grow together.

use vm_state::{Arm64Regs, Arm64Sysregs, Arm64VmState};
use vmm_backend::{Arm64CoreRegs, Arm64SysregFile, Arm64VcpuState};

use crate::snapshot::SnapshotError;

pub(crate) fn to_vm_regs(c: &Arm64CoreRegs) -> Arm64Regs {
    Arm64Regs {
        x: c.x,
        sp: c.sp,
        pc: c.pc,
        pstate: c.pstate,
        sp_el1: c.sp_el1,
        elr_el1: c.elr_el1,
        spsr_el1: c.spsr_el1,
    }
}

pub(crate) fn from_vm_regs(r: &Arm64Regs) -> Arm64CoreRegs {
    Arm64CoreRegs {
        x: r.x,
        sp: r.sp,
        pc: r.pc,
        pstate: r.pstate,
        sp_el1: r.sp_el1,
        elr_el1: r.elr_el1,
        spsr_el1: r.spsr_el1,
    }
}

pub(crate) fn to_vm_sysregs(s: &Arm64SysregFile) -> Arm64Sysregs {
    Arm64Sysregs {
        sctlr_el1: s.sctlr_el1,
        ttbr0_el1: s.ttbr0_el1,
        ttbr1_el1: s.ttbr1_el1,
        tcr_el1: s.tcr_el1,
        mair_el1: s.mair_el1,
        vbar_el1: s.vbar_el1,
        cpacr_el1: s.cpacr_el1,
        esr_el1: s.esr_el1,
        far_el1: s.far_el1,
        tpidr_el0: s.tpidr_el0,
        tpidr_el1: s.tpidr_el1,
        cntkctl_el1: s.cntkctl_el1,
    }
}

pub(crate) fn from_vm_sysregs(s: &Arm64Sysregs) -> Arm64SysregFile {
    Arm64SysregFile {
        sctlr_el1: s.sctlr_el1,
        ttbr0_el1: s.ttbr0_el1,
        ttbr1_el1: s.ttbr1_el1,
        tcr_el1: s.tcr_el1,
        mair_el1: s.mair_el1,
        vbar_el1: s.vbar_el1,
        cpacr_el1: s.cpacr_el1,
        esr_el1: s.esr_el1,
        far_el1: s.far_el1,
        tpidr_el0: s.tpidr_el0,
        tpidr_el1: s.tpidr_el1,
        cntkctl_el1: s.cntkctl_el1,
    }
}

pub(crate) fn to_vm_mp_state(m: vmm_backend::MpState) -> vm_state::MpState {
    match m {
        vmm_backend::MpState::Runnable => vm_state::MpState::Runnable,
        vmm_backend::MpState::Halted => vm_state::MpState::Halted,
    }
}

pub(crate) fn from_vm_mp_state(m: vm_state::MpState) -> vmm_backend::MpState {
    match m {
        vm_state::MpState::Runnable => vmm_backend::MpState::Runnable,
        vm_state::MpState::Halted => vmm_backend::MpState::Halted,
    }
}

/// Build the live vCPU record set from a decoded snapshot.
pub(crate) fn vcpu_state_from(s: &Arm64VmState) -> Arm64VcpuState {
    Arm64VcpuState {
        core: from_vm_regs(&s.regs),
        sysregs: from_vm_sysregs(&s.sysregs),
        mp_state: from_vm_mp_state(s.mp_state),
    }
}

/// Fill a snapshot's vCPU records from the live vCPU state.
pub(crate) fn fill_vcpu_state(out: &mut Arm64VmState, s: &Arm64VcpuState) {
    out.regs = to_vm_regs(&s.core);
    out.sysregs = to_vm_sysregs(&s.sysregs);
    out.mp_state = to_vm_mp_state(s.mp_state);
}

// ---------------------------------------------------------------------------
// The vmm-core arm64 device blob: the bytes carried in `vm_state::DeviceBlob`.
//
// The arm64 sibling of the x86 `DEV1` blob: a small, versioned, little-endian
// record vmm-core owns end to end (the vm-state codec never interprets it).
// Total decode, no panic (rule #4).
// ---------------------------------------------------------------------------

/// Device-blob magic: `"ADV1"` read little-endian (distinct from x86's
/// `"DEV1"`, so a cross-wired blob fails on magic even before the container's
/// arch tag would have caught it).
const DEVICE_BLOB_MAGIC: u32 = 0x3156_4441;
/// Device-blob layout version for a VM with **no GICv3 wired**: the guest
/// clock-offset register, the ordered conformance report stream, and the
/// PL011 residual state.
const DEVICE_BLOB_VERSION_BASE: u16 = 1;
/// Device-blob layout version for a VM with the userspace GICv3 wired: v1
/// plus a trailing [`gicv3::GicState`] record. **The version IS the wiring
/// flag** (the x86 pvclock-blob pattern): an unwired VM encodes
/// [`DEVICE_BLOB_VERSION_BASE`] with no trailing record at all, and the
/// decoder accepts exactly these two versions.
const DEVICE_BLOB_VERSION_GIC: u16 = 2;
/// v1 + a trailing length-prefixed **doorbell-pages** record — the dedicated
/// arm64 hypercall-transport ABI memslot (review r11), guest-visible memory that
/// must survive save/restore/branch. No GIC.
const DEVICE_BLOB_VERSION_DOORBELL: u16 = 3;
/// v2 + the doorbell record: the GIC **and** the doorbell pages.
const DEVICE_BLOB_VERSION_GIC_DOORBELL: u16 = 4;

/// Everything the vmm-core arm64 device blob carries.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct Arm64DeviceState {
    /// The guest clock-offset register the engine re-applies with its V-time
    /// commit (the arm64 analogue of `IA32_TSC_ADJUST`; the concrete guest
    /// register it backs is the paravirt clock page's — `hm-rk5`'s seam).
    pub clock_offset: u64,
    /// The ordered conformance report stream — guest-observable output that
    /// feeds `observable_digest`, restored so a branch resumes it.
    pub report_stream: Vec<u32>,
    /// The PL011 serial capture (so a restored continuation reproduces
    /// byte-identical console output).
    pub uart_capture: Vec<u8>,
    /// The PL011 configuration-register shadows (`IBRD`, `FBRD`, `LCR_H`,
    /// `CR`, `IMSC`).
    pub uart_regs: [u32; 5],
    /// The GICv3 fabric state (register files + PMR + the virtual timer),
    /// present exactly when the sealing VM had the fabric wired.
    pub gic: Option<gicv3::GicState>,
    /// The dedicated hypercall-transport ABI pages (`REQ_GPA`/`RESP_GPA`) — a
    /// separate arm64 memslot (RAM is high), so their bytes are **not** in the
    /// main-RAM snapshot and must ride here to survive save/restore/branch
    /// (review r11). Empty exactly when the VM never mapped them (x86-style /
    /// unwired composition); non-empty ⇒ exactly `2 · HC_PAGE` bytes.
    pub doorbell: Vec<u8>,
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Encode a [`gicv3::GicState`] in fixed declaration order (all POD, LE; the
/// same bytes the `GICV` hash chunk carries, so the two never disagree).
pub(crate) fn encode_gic_state(out: &mut Vec<u8>, s: &gicv3::GicState) {
    put_u32(out, s.version);
    put_u32(out, s.impl_spis);
    out.extend_from_slice(&s.timer_hz.to_le_bytes());
    put_u32(out, s.timer_intid);
    put_u32(out, s.gicd_ctlr);
    for file in [&s.group, &s.enable, &s.pending, &s.active] {
        for w in file {
            put_u32(out, *w);
        }
    }
    out.extend_from_slice(&s.priority);
    out.push(s.pmr);
    out.extend_from_slice(&s.cntv_ctl.to_le_bytes());
    out.extend_from_slice(&s.cntv_cval.to_le_bytes());
    out.push(u8::from(s.timer_fired));
}

/// Decode a [`gicv3::GicState`] (the reverse of [`encode_gic_state`]; the
/// coherence validation itself is [`gicv3::Gicv3::restore`]'s).
fn decode_gic_state(c: &mut Cursor<'_>) -> Result<gicv3::GicState, SnapshotError> {
    let version = c.u32()?;
    let impl_spis = c.u32()?;
    let timer_hz = c.u64()?;
    let timer_intid = c.u32()?;
    let gicd_ctlr = c.u32()?;
    let mut files = [[0u32; 32]; 4];
    for file in &mut files {
        for w in file.iter_mut() {
            *w = c.u32()?;
        }
    }
    let [group, enable, pending, active] = files;
    let mut priority = [0u8; 1020];
    priority.copy_from_slice(c.take(1020)?);
    let pmr_byte = c.take(1)?[0];
    let cntv_ctl = c.u64()?;
    let cntv_cval = c.u64()?;
    let timer_fired = match c.take(1)?[0] {
        0 => false,
        1 => true,
        _ => return Err(SnapshotError::DeviceBlob("bad timer_fired flag")),
    };
    Ok(gicv3::GicState {
        version,
        impl_spis,
        timer_hz,
        timer_intid,
        gicd_ctlr,
        group,
        enable,
        pending,
        active,
        priority,
        pmr: pmr_byte,
        cntv_ctl,
        cntv_cval,
        timer_fired,
    })
}

/// Encode the device blob (deterministic; fixed field order).
pub(crate) fn encode_device_blob(d: &Arm64DeviceState) -> vm_state::DeviceBlob {
    let mut v = Vec::new();
    put_u32(&mut v, DEVICE_BLOB_MAGIC);
    // The version IS the wiring flag (the x86 pvclock-blob pattern), now over two
    // independent optional records: the GIC and the doorbell pages.
    let version = match (d.gic.is_some(), !d.doorbell.is_empty()) {
        (false, false) => DEVICE_BLOB_VERSION_BASE,
        (true, false) => DEVICE_BLOB_VERSION_GIC,
        (false, true) => DEVICE_BLOB_VERSION_DOORBELL,
        (true, true) => DEVICE_BLOB_VERSION_GIC_DOORBELL,
    };
    v.extend_from_slice(&version.to_le_bytes());
    v.extend_from_slice(&d.clock_offset.to_le_bytes());
    put_u32(&mut v, d.report_stream.len() as u32);
    for w in &d.report_stream {
        put_u32(&mut v, *w);
    }
    put_u32(&mut v, d.uart_capture.len() as u32);
    v.extend_from_slice(&d.uart_capture);
    for r in d.uart_regs {
        put_u32(&mut v, r);
    }
    if let Some(gic) = &d.gic {
        encode_gic_state(&mut v, gic);
    }
    // The doorbell record trails the GIC (length-prefixed), present exactly on
    // the doorbell-bearing versions.
    if !d.doorbell.is_empty() {
        put_u32(&mut v, d.doorbell.len() as u32);
        v.extend_from_slice(&d.doorbell);
    }
    vm_state::DeviceBlob(v)
}

/// A forward-only little-endian cursor; every over-read is a decode error.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SnapshotError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(SnapshotError::DeviceBlob("length overflow"))?;
        let s = self
            .buf
            .get(self.pos..end)
            .ok_or(SnapshotError::DeviceBlob("truncated"))?;
        self.pos = end;
        Ok(s)
    }

    fn u16(&mut self) -> Result<u16, SnapshotError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, SnapshotError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, SnapshotError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
}

/// Decode the device blob. Strict and total: bad magic/version, truncation,
/// and trailing bytes are all loud errors, never a best-effort restore.
pub(crate) fn decode_device_blob(bytes: &[u8]) -> Result<Arm64DeviceState, SnapshotError> {
    let mut c = Cursor { buf: bytes, pos: 0 };
    if c.u32()? != DEVICE_BLOB_MAGIC {
        return Err(SnapshotError::DeviceBlob("bad arm64 device-blob magic"));
    }
    let version = c.u16()?;
    let (has_gic, has_doorbell) = match version {
        DEVICE_BLOB_VERSION_BASE => (false, false),
        DEVICE_BLOB_VERSION_GIC => (true, false),
        DEVICE_BLOB_VERSION_DOORBELL => (false, true),
        DEVICE_BLOB_VERSION_GIC_DOORBELL => (true, true),
        _ => {
            return Err(SnapshotError::DeviceBlob(
                "unsupported arm64 device-blob version",
            ));
        }
    };
    let clock_offset = c.u64()?;
    let report_len = c.u32()? as usize;
    let mut report_stream = Vec::with_capacity(report_len.min(4096));
    for _ in 0..report_len {
        report_stream.push(c.u32()?);
    }
    let cap_len = c.u32()? as usize;
    let uart_capture = c.take(cap_len)?.to_vec();
    let mut uart_regs = [0u32; 5];
    for r in &mut uart_regs {
        *r = c.u32()?;
    }
    let gic = if has_gic {
        Some(decode_gic_state(&mut c)?)
    } else {
        None
    };
    let doorbell = if has_doorbell {
        let len = c.u32()? as usize;
        c.take(len)?.to_vec()
    } else {
        Vec::new()
    };
    if c.pos != bytes.len() {
        return Err(SnapshotError::DeviceBlob("trailing bytes"));
    }
    Ok(Arm64DeviceState {
        clock_offset,
        report_stream,
        uart_capture,
        uart_regs,
        gic,
        doorbell,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Arm64DeviceState {
        Arm64DeviceState {
            clock_offset: 0xDEAD_BEEF,
            report_stream: vec![1, 2, 3],
            uart_capture: b"hello".to_vec(),
            uart_regs: [13, 1, 0x70, 0x301, 0x10],
            gic: None,
            doorbell: Vec::new(),
        }
    }

    /// A wired-fabric sample: the version-is-the-wiring-flag (v2) shape.
    fn sample_with_gic() -> Arm64DeviceState {
        let mut gic = gicv3::Gicv3::new(gicv3::GicConfig {
            impl_spis: 32,
            timer_hz: 62_500_000,
            timer_intid: 27,
        })
        .unwrap();
        gic.raise(40).unwrap();
        gic.set_pmr(0x80);
        gic.write_cntv_cval(125);
        gic.write_cntv_ctl(gicv3::CNTV_CTL_ENABLE);
        Arm64DeviceState {
            gic: Some(gic.snapshot()),
            ..sample()
        }
    }

    /// A doorbell-bearing sample: distinctive 2-page ABI-page bytes (review r11).
    fn sample_with_doorbell() -> Arm64DeviceState {
        Arm64DeviceState {
            doorbell: (0..8192u32).map(|i| i as u8).collect(),
            ..sample()
        }
    }

    #[test]
    fn device_blob_round_trips() {
        // All four version shapes: base (v1), gic (v2), doorbell (v3), gic +
        // doorbell (v4) — the version is the two-flag wiring witness.
        let gic_and_doorbell = Arm64DeviceState {
            doorbell: sample_with_doorbell().doorbell,
            ..sample_with_gic()
        };
        for d in [
            sample(),
            sample_with_gic(),
            sample_with_doorbell(),
            gic_and_doorbell,
        ] {
            let blob = encode_device_blob(&d);
            assert_eq!(decode_device_blob(&blob.0).unwrap(), d);
        }
    }

    #[test]
    fn device_blob_decode_is_strict_and_total() {
        let blob = encode_device_blob(&sample()).0;
        // Every truncation point errors, never panics.
        for n in 0..blob.len() {
            assert!(decode_device_blob(&blob[..n]).is_err());
        }
        // Trailing bytes are rejected.
        let mut trailing = blob.clone();
        trailing.push(0);
        assert!(decode_device_blob(&trailing).is_err());
        // A foreign (x86 "DEV1") magic is rejected.
        let mut foreign = blob;
        foreign[..4].copy_from_slice(&0x3156_4544u32.to_le_bytes());
        assert!(decode_device_blob(&foreign).is_err());
    }

    #[test]
    fn vcpu_conversions_are_lossless_mirrors() {
        let mut live = Arm64VcpuState::default();
        live.core.x[0] = 1;
        live.core.x[30] = 30;
        live.core.pc = 0x8_0000;
        live.core.pstate = 0x3c5;
        live.sysregs.sctlr_el1 = 0x30d0_0800;
        live.sysregs.cntkctl_el1 = 3;
        live.mp_state = vmm_backend::MpState::Halted;

        let mut snap = Arm64VmState::default();
        fill_vcpu_state(&mut snap, &live);
        assert_eq!(vcpu_state_from(&snap), live);
    }
}
