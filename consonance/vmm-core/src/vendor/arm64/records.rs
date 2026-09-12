// SPDX-License-Identifier: AGPL-3.0-or-later

use vm_state::{
    Arm64Debug, Arm64Interrupts, Arm64Regs, Arm64SimdFp, Arm64Sysregs, Arm64VmState, Arm64Vtimer,
};
use vmm_backend::{
    Arm64CoreRegs, Arm64DebugState, Arm64GicState, Arm64InterruptState, Arm64SimdFpState,
    Arm64SysregFile, Arm64VcpuState, Arm64VtimerState,
};

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

pub(crate) fn vcpu_state_from(s: &Arm64VmState) -> Arm64VcpuState {
    Arm64VcpuState {
        core: from_vm_regs(&s.regs),
        sysregs: from_vm_sysregs(&s.sysregs),
        simd_fp: Arm64SimdFpState {
            q: s.simd_fp.q,
            fpcr: s.simd_fp.fpcr,
            fpsr: s.simd_fp.fpsr,
        },
        debug: Arm64DebugState {
            breakpoint_value: s.debug.breakpoint_value,
            breakpoint_control: s.debug.breakpoint_control,
            watchpoint_value: s.debug.watchpoint_value,
            watchpoint_control: s.debug.watchpoint_control,
            mdscr_el1: s.debug.mdscr_el1,
            trap_debug_exceptions: s.debug.trap_debug_exceptions,
            trap_debug_reg_accesses: s.debug.trap_debug_reg_accesses,
        },
        vtimer: Arm64VtimerState {
            cntv_ctl_el0: s.vtimer.cntv_ctl_el0,
            cntv_cval_el0: s.vtimer.cntv_cval_el0,
            masked: s.vtimer.masked,
            offset: s.vtimer.offset,
        },
        interrupts: Arm64InterruptState {
            irq: s.interrupts.irq,
            fiq: s.interrupts.fiq,
        },
        mp_state: from_vm_mp_state(s.mp_state),
        gic: None,
    }
}

pub(crate) fn gic_from_backend(s: &Arm64GicState) -> gicv3::GicState {
    gicv3::GicState {
        version: s.version,
        impl_spis: s.impl_spis,
        timer_hz: s.timer_hz,
        timer_intid: s.timer_intid,
        gicd_ctlr: s.gicd_ctlr,
        group: s.group,
        enable: s.enable,
        pending: s.pending,
        active: s.active,
        line_level: s.line_level,
        priority: s.priority,
        pmr: s.pmr,
        igrpen1: s.igrpen1,
        cntv_ctl: s.cntv_ctl,
        cntv_cval: s.cntv_cval,
        timer_fired: s.timer_fired,
    }
}

pub(crate) fn gic_to_backend(s: &gicv3::GicState) -> Arm64GicState {
    Arm64GicState {
        version: s.version,
        impl_spis: s.impl_spis,
        timer_hz: s.timer_hz,
        timer_intid: s.timer_intid,
        gicd_ctlr: s.gicd_ctlr,
        group: s.group,
        enable: s.enable,
        pending: s.pending,
        active: s.active,
        line_level: s.line_level,
        priority: s.priority,
        pmr: s.pmr,
        igrpen1: s.igrpen1,
        cntv_ctl: s.cntv_ctl,
        cntv_cval: s.cntv_cval,
        timer_fired: s.timer_fired,
    }
}

pub(crate) fn fill_vcpu_state(out: &mut Arm64VmState, s: &Arm64VcpuState) {
    out.regs = to_vm_regs(&s.core);
    out.sysregs = to_vm_sysregs(&s.sysregs);
    out.simd_fp = Arm64SimdFp {
        q: s.simd_fp.q,
        fpcr: s.simd_fp.fpcr,
        fpsr: s.simd_fp.fpsr,
    };
    out.debug = Arm64Debug {
        breakpoint_value: s.debug.breakpoint_value,
        breakpoint_control: s.debug.breakpoint_control,
        watchpoint_value: s.debug.watchpoint_value,
        watchpoint_control: s.debug.watchpoint_control,
        mdscr_el1: s.debug.mdscr_el1,
        trap_debug_exceptions: s.debug.trap_debug_exceptions,
        trap_debug_reg_accesses: s.debug.trap_debug_reg_accesses,
    };
    out.vtimer = Arm64Vtimer {
        cntv_ctl_el0: s.vtimer.cntv_ctl_el0,
        cntv_cval_el0: s.vtimer.cntv_cval_el0,
        masked: s.vtimer.masked,
        offset: s.vtimer.offset,
    };
    out.interrupts = Arm64Interrupts {
        irq: s.interrupts.irq,
        fiq: s.interrupts.fiq,
    };
    out.mp_state = to_vm_mp_state(s.mp_state);
}

const DEVICE_BLOB_MAGIC: u32 = 0x3156_4441;
const DEVICE_BLOB_VERSION_BASE: u16 = 1;
const DEVICE_BLOB_VERSION_GIC: u16 = 2;
const DEVICE_BLOB_VERSION_DOORBELL: u16 = 3;
const DEVICE_BLOB_VERSION_GIC_DOORBELL: u16 = 4;
const DEVICE_BLOB_VERSION_PVCLOCK_LEGACY: u16 = 5;
const DEVICE_BLOB_VERSION_GIC_PVCLOCK_LEGACY: u16 = 6;
const DEVICE_BLOB_VERSION_DOORBELL_PVCLOCK_LEGACY: u16 = 7;
const DEVICE_BLOB_VERSION_GIC_DOORBELL_PVCLOCK_LEGACY: u16 = 8;
const DEVICE_BLOB_VERSION_PVCLOCK: u16 = 9;
const DEVICE_BLOB_VERSION_GIC_PVCLOCK: u16 = 10;
const DEVICE_BLOB_VERSION_DOORBELL_PVCLOCK: u16 = 11;
const DEVICE_BLOB_VERSION_GIC_DOORBELL_PVCLOCK: u16 = 12;
const DOORBELL_BLOB_LEN: usize = 4 * 4096;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Arm64ClockeventState {
    pub deadline: Option<u64>,
    pub line_asserted: bool,
    pub assertions: u64,
    pub acknowledgements: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Arm64PvclockState {
    pub gpa: Option<u64>,
    pub registrable: bool,
    pub armed: bool,
    pub virtual_time: bool,
    pub clockevent: Arm64ClockeventState,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct Arm64DeviceState {
    pub clock_offset: u64,
    pub report_stream: Vec<u32>,
    pub uart_capture: Vec<u8>,
    pub uart_regs: [u32; 5],
    pub gic: Option<gicv3::GicState>,
    pub doorbell: Vec<u8>,
    pub pvclock: Option<Arm64PvclockState>,
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn encode_clockevent_state(out: &mut Vec<u8>, state: Arm64ClockeventState) {
    match state.deadline {
        Some(deadline) => {
            out.push(1);
            out.extend_from_slice(&deadline.to_le_bytes());
        }
        None => out.push(0),
    }
    out.push(u8::from(state.line_asserted));
    out.extend_from_slice(&state.assertions.to_le_bytes());
    out.extend_from_slice(&state.acknowledgements.to_le_bytes());
}

pub(crate) fn encode_gic_state(out: &mut Vec<u8>, s: &gicv3::GicState) {
    put_u32(out, s.version);
    put_u32(out, s.impl_spis);
    out.extend_from_slice(&s.timer_hz.to_le_bytes());
    put_u32(out, s.timer_intid);
    put_u32(out, s.gicd_ctlr);
    for file in [&s.group, &s.enable, &s.pending, &s.active, &s.line_level] {
        for w in file {
            put_u32(out, *w);
        }
    }
    out.extend_from_slice(&s.priority);
    out.push(s.pmr);
    out.push(u8::from(s.igrpen1));
    out.extend_from_slice(&s.cntv_ctl.to_le_bytes());
    out.extend_from_slice(&s.cntv_cval.to_le_bytes());
    out.push(u8::from(s.timer_fired));
}

fn decode_gic_state(c: &mut Cursor<'_>) -> Result<gicv3::GicState, SnapshotError> {
    let version = c.u32()?;
    let impl_spis = c.u32()?;
    let timer_hz = c.u64()?;
    let timer_intid = c.u32()?;
    let gicd_ctlr = c.u32()?;
    let mut files = [[0u32; 32]; 5];
    for file in &mut files {
        for w in file.iter_mut() {
            *w = c.u32()?;
        }
    }
    let [group, enable, pending, active, line_level] = files;
    let mut priority = [0u8; 1020];
    priority.copy_from_slice(c.take(1020)?);
    let pmr_byte = c.take(1)?[0];
    let igrpen1 = match c.take(1)?[0] {
        0 => false,
        1 => true,
        _ => return Err(SnapshotError::DeviceBlob("bad igrpen1 flag")),
    };
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
        line_level,
        priority,
        pmr: pmr_byte,
        igrpen1,
        cntv_ctl,
        cntv_cval,
        timer_fired,
    })
}

pub(crate) fn encode_device_blob(d: &Arm64DeviceState) -> vm_state::DeviceBlob {
    let mut v = Vec::new();
    put_u32(&mut v, DEVICE_BLOB_MAGIC);
    let pending_pvclock = d.pvclock.is_some_and(|pv| pv.gpa.is_some() && !pv.armed);
    let version = match (d.gic.is_some(), !d.doorbell.is_empty(), d.pvclock.is_some()) {
        (false, false, false) => DEVICE_BLOB_VERSION_BASE,
        (true, false, false) => DEVICE_BLOB_VERSION_GIC,
        (false, true, false) => DEVICE_BLOB_VERSION_DOORBELL,
        (true, true, false) => DEVICE_BLOB_VERSION_GIC_DOORBELL,
        (false, false, true) => {
            if pending_pvclock {
                DEVICE_BLOB_VERSION_PVCLOCK
            } else {
                DEVICE_BLOB_VERSION_PVCLOCK_LEGACY
            }
        }
        (true, false, true) => {
            if pending_pvclock {
                DEVICE_BLOB_VERSION_GIC_PVCLOCK
            } else {
                DEVICE_BLOB_VERSION_GIC_PVCLOCK_LEGACY
            }
        }
        (false, true, true) => {
            if pending_pvclock {
                DEVICE_BLOB_VERSION_DOORBELL_PVCLOCK
            } else {
                DEVICE_BLOB_VERSION_DOORBELL_PVCLOCK_LEGACY
            }
        }
        (true, true, true) => {
            if pending_pvclock {
                DEVICE_BLOB_VERSION_GIC_DOORBELL_PVCLOCK
            } else {
                DEVICE_BLOB_VERSION_GIC_DOORBELL_PVCLOCK_LEGACY
            }
        }
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
    if !d.doorbell.is_empty() {
        put_u32(&mut v, d.doorbell.len() as u32);
        v.extend_from_slice(&d.doorbell);
    }
    if let Some(pv) = d.pvclock {
        match pv.gpa {
            Some(gpa) => {
                v.push(1);
                v.extend_from_slice(&gpa.to_le_bytes());
            }
            None => v.push(0),
        }
        v.push(u8::from(pv.registrable));
        if pending_pvclock {
            v.push(u8::from(pv.armed));
        }
        v.push(u8::from(pv.virtual_time));
        encode_clockevent_state(&mut v, pv.clockevent);
    }
    vm_state::DeviceBlob(v)
}

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

pub(crate) fn decode_device_blob(bytes: &[u8]) -> Result<Arm64DeviceState, SnapshotError> {
    let mut c = Cursor { buf: bytes, pos: 0 };
    if c.u32()? != DEVICE_BLOB_MAGIC {
        return Err(SnapshotError::DeviceBlob("bad arm64 device-blob magic"));
    }
    let version = c.u16()?;
    let (has_gic, has_doorbell, has_pvclock, has_pvclock_armed) = match version {
        DEVICE_BLOB_VERSION_BASE => (false, false, false, false),
        DEVICE_BLOB_VERSION_GIC => (true, false, false, false),
        DEVICE_BLOB_VERSION_DOORBELL => (false, true, false, false),
        DEVICE_BLOB_VERSION_GIC_DOORBELL => (true, true, false, false),
        DEVICE_BLOB_VERSION_PVCLOCK_LEGACY => (false, false, true, false),
        DEVICE_BLOB_VERSION_GIC_PVCLOCK_LEGACY => (true, false, true, false),
        DEVICE_BLOB_VERSION_DOORBELL_PVCLOCK_LEGACY => (false, true, true, false),
        DEVICE_BLOB_VERSION_GIC_DOORBELL_PVCLOCK_LEGACY => (true, true, true, false),
        DEVICE_BLOB_VERSION_PVCLOCK => (false, false, true, true),
        DEVICE_BLOB_VERSION_GIC_PVCLOCK => (true, false, true, true),
        DEVICE_BLOB_VERSION_DOORBELL_PVCLOCK => (false, true, true, true),
        DEVICE_BLOB_VERSION_GIC_DOORBELL_PVCLOCK => (true, true, true, true),
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
        if len != DOORBELL_BLOB_LEN {
            return Err(SnapshotError::DeviceBlob(
                "doorbell record length contradicts the version flag",
            ));
        }
        c.take(len)?.to_vec()
    } else {
        Vec::new()
    };
    let pvclock = if has_pvclock {
        let gpa = match c.take(1)?[0] {
            0 => None,
            1 => Some(c.u64()?),
            _ => return Err(SnapshotError::DeviceBlob("bad pvclock gpa flag")),
        };
        let registrable = match c.take(1)?[0] {
            0 => false,
            1 => true,
            _ => return Err(SnapshotError::DeviceBlob("bad pvclock registrable flag")),
        };
        if gpa.is_some() && !registrable {
            return Err(SnapshotError::DeviceBlob(
                "registered pvclock page is marked non-registrable",
            ));
        }
        let armed = if has_pvclock_armed {
            match c.take(1)?[0] {
                0 => false,
                _ => {
                    return Err(SnapshotError::DeviceBlob(
                        "current pvclock record must represent a pending registration",
                    ));
                }
            }
        } else {
            gpa.is_some()
        };
        if has_pvclock_armed && gpa.is_none() {
            return Err(SnapshotError::DeviceBlob(
                "current pvclock record is missing its registered GPA",
            ));
        }
        let virtual_time = match c.take(1)?[0] {
            0 => false,
            1 => true,
            _ => return Err(SnapshotError::DeviceBlob("bad V-time mode flag")),
        };
        let deadline = match c.take(1)?[0] {
            0 => None,
            1 => Some(c.u64()?),
            _ => return Err(SnapshotError::DeviceBlob("bad clockevent deadline flag")),
        };
        let line_asserted = match c.take(1)?[0] {
            0 => false,
            1 => true,
            _ => return Err(SnapshotError::DeviceBlob("bad clockevent line flag")),
        };
        if deadline.is_some() && line_asserted {
            return Err(SnapshotError::DeviceBlob(
                "clockevent cannot retain a deadline while its line is asserted",
            ));
        }
        let assertions = c.u64()?;
        let acknowledgements = c.u64()?;
        if acknowledgements > assertions {
            return Err(SnapshotError::DeviceBlob(
                "clockevent ACK count exceeds assertion count",
            ));
        }
        Some(Arm64PvclockState {
            gpa,
            registrable,
            armed,
            virtual_time,
            clockevent: Arm64ClockeventState {
                deadline,
                line_asserted,
                assertions,
                acknowledgements,
            },
        })
    } else {
        None
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
        pvclock,
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
            pvclock: None,
        }
    }

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

    fn sample_with_doorbell() -> Arm64DeviceState {
        Arm64DeviceState {
            doorbell: (0..DOORBELL_BLOB_LEN as u32).map(|i| i as u8).collect(),
            ..sample()
        }
    }

    fn sample_with_pvclock() -> Arm64DeviceState {
        Arm64DeviceState {
            pvclock: Some(Arm64PvclockState {
                gpa: Some(0x4031_1000),
                registrable: true,
                armed: true,
                virtual_time: true,
                clockevent: Arm64ClockeventState {
                    deadline: None,
                    line_asserted: true,
                    assertions: 7,
                    acknowledgements: 6,
                },
            }),
            ..sample()
        }
    }

    fn sample_with_pending_pvclock() -> Arm64DeviceState {
        let mut pending = sample_with_pvclock();
        pending.pvclock.as_mut().unwrap().armed = false;
        pending
    }

    fn sample_with_empty_pvclock() -> Arm64DeviceState {
        Arm64DeviceState {
            pvclock: Some(Arm64PvclockState {
                gpa: None,
                registrable: false,
                armed: false,
                virtual_time: false,
                clockevent: Arm64ClockeventState {
                    deadline: None,
                    line_asserted: false,
                    assertions: 0,
                    acknowledgements: 0,
                },
            }),
            ..sample()
        }
    }

    #[test]
    fn device_blob_round_trips() {
        let gic_and_doorbell = Arm64DeviceState {
            doorbell: sample_with_doorbell().doorbell,
            ..sample_with_gic()
        };
        let mut gic_and_pvclock = sample_with_gic();
        gic_and_pvclock.pvclock = sample_with_pvclock().pvclock;
        let mut doorbell_and_pvclock = sample_with_doorbell();
        doorbell_and_pvclock.pvclock = sample_with_pvclock().pvclock;
        let mut all = gic_and_doorbell.clone();
        all.pvclock = sample_with_pvclock().pvclock;
        for d in [
            sample(),
            sample_with_gic(),
            sample_with_doorbell(),
            gic_and_doorbell,
            sample_with_pvclock(),
            sample_with_empty_pvclock(),
            gic_and_pvclock,
            doorbell_and_pvclock,
            all,
        ] {
            let blob = encode_device_blob(&d);
            assert_eq!(decode_device_blob(&blob.0).unwrap(), d);
        }
    }

    #[test]
    fn current_pvclock_record_round_trips_a_pending_registration() {
        let pending = sample_with_pending_pvclock();
        let blob = encode_device_blob(&pending).0;
        assert_eq!(
            u16::from_le_bytes([blob[4], blob[5]]),
            DEVICE_BLOB_VERSION_PVCLOCK
        );
        assert_eq!(decode_device_blob(&blob).unwrap(), pending);
    }

    #[test]
    fn current_pvclock_versions_round_trip_every_device_composition() {
        let mut gic_and_pvclock = sample_with_gic();
        gic_and_pvclock.pvclock = sample_with_pending_pvclock().pvclock;

        let mut doorbell_and_pvclock = sample_with_doorbell();
        doorbell_and_pvclock.pvclock = sample_with_pending_pvclock().pvclock;

        let mut all = sample_with_gic();
        all.doorbell = sample_with_doorbell().doorbell;
        all.pvclock = sample_with_pending_pvclock().pvclock;

        for (state, version) in [
            (sample_with_pending_pvclock(), DEVICE_BLOB_VERSION_PVCLOCK),
            (gic_and_pvclock, DEVICE_BLOB_VERSION_GIC_PVCLOCK),
            (doorbell_and_pvclock, DEVICE_BLOB_VERSION_DOORBELL_PVCLOCK),
            (all, DEVICE_BLOB_VERSION_GIC_DOORBELL_PVCLOCK),
        ] {
            let blob = encode_device_blob(&state).0;
            assert_eq!(u16::from_le_bytes([blob[4], blob[5]]), version);

            assert_eq!(decode_device_blob(&blob).unwrap(), state);
        }
    }

    #[test]
    fn legacy_pvclock_versions_derive_armed_from_the_gpa() {
        let mut gic_and_pvclock = sample_with_gic();
        gic_and_pvclock.pvclock = sample_with_pvclock().pvclock;
        let mut doorbell_and_pvclock = sample_with_doorbell();
        doorbell_and_pvclock.pvclock = sample_with_pvclock().pvclock;
        let mut all = gic_and_pvclock.clone();
        all.doorbell = sample_with_doorbell().doorbell;

        for (mut state, legacy_version) in [
            (sample_with_pvclock(), DEVICE_BLOB_VERSION_PVCLOCK_LEGACY),
            (gic_and_pvclock, DEVICE_BLOB_VERSION_GIC_PVCLOCK_LEGACY),
            (
                doorbell_and_pvclock,
                DEVICE_BLOB_VERSION_DOORBELL_PVCLOCK_LEGACY,
            ),
            (all, DEVICE_BLOB_VERSION_GIC_DOORBELL_PVCLOCK_LEGACY),
        ] {
            state.pvclock.as_mut().unwrap().armed = false;
            let mut blob = encode_device_blob(&state).0;
            let armed_index = blob.len() - 20;
            assert_eq!(blob[armed_index], 0);
            blob.remove(armed_index);
            blob[4..6].copy_from_slice(&legacy_version.to_le_bytes());
            state.pvclock.as_mut().unwrap().armed = true;
            assert_eq!(encode_device_blob(&state).0, blob);
            assert_eq!(decode_device_blob(&blob).unwrap(), state);
        }

        let unregistered = encode_device_blob(&sample_with_empty_pvclock()).0;
        assert_eq!(
            u16::from_le_bytes([unregistered[4], unregistered[5]]),
            DEVICE_BLOB_VERSION_PVCLOCK_LEGACY
        );
        assert_eq!(
            decode_device_blob(&unregistered).unwrap(),
            sample_with_empty_pvclock()
        );
    }

    #[test]
    fn archived_legacy_pvclock_fixtures_round_trip_byte_exactly() {
        for (blob, version) in [
            (
                include_bytes!("../../../tests/fixtures/harmony-arm64-v5-pvclock.bin").as_slice(),
                DEVICE_BLOB_VERSION_PVCLOCK_LEGACY,
            ),
            (
                include_bytes!("../../../tests/fixtures/harmony-arm64-v6-gic-pvclock.bin")
                    .as_slice(),
                DEVICE_BLOB_VERSION_GIC_PVCLOCK_LEGACY,
            ),
            (
                include_bytes!("../../../tests/fixtures/harmony-arm64-v7-doorbell-pvclock.bin")
                    .as_slice(),
                DEVICE_BLOB_VERSION_DOORBELL_PVCLOCK_LEGACY,
            ),
            (
                include_bytes!("../../../tests/fixtures/harmony-arm64-v8-gic-doorbell-pvclock.bin")
                    .as_slice(),
                DEVICE_BLOB_VERSION_GIC_DOORBELL_PVCLOCK_LEGACY,
            ),
        ] {
            assert_eq!(u16::from_le_bytes([blob[4], blob[5]]), version);
            let decoded = decode_device_blob(blob).unwrap();
            let pvclock = decoded.pvclock.expect("legacy fixture carries pvclock");
            assert_eq!(pvclock.gpa, Some(0x4031_1000));
            assert!(pvclock.registrable);
            assert!(pvclock.armed, "a legacy GPA implies an armed registration");
            assert_eq!(encode_device_blob(&decoded).0, blob);
        }
    }

    #[test]
    fn backend_gic_conversion_preserves_every_nondefault_field() {
        let state = sample_with_gic().gic.unwrap();
        let backend = gic_to_backend(&state);
        assert_eq!(gic_from_backend(&backend), state);
        assert_ne!(backend, Arm64GicState::default());
    }

    #[test]
    fn device_blob_decode_is_strict_and_total() {
        let blob = encode_device_blob(&sample()).0;
        for n in 0..blob.len() {
            assert!(decode_device_blob(&blob[..n]).is_err());
        }
        let mut trailing = blob.clone();
        trailing.push(0);
        assert!(decode_device_blob(&trailing).is_err());
        let mut foreign = blob;
        foreign[..4].copy_from_slice(&0x3156_4544u32.to_le_bytes());
        assert!(decode_device_blob(&foreign).is_err());
    }

    #[test]
    fn decode_rejects_a_doorbell_version_with_the_wrong_doorbell_length() {
        for base in [sample_with_doorbell(), {
            let mut d = sample_with_gic();
            d.doorbell = sample_with_doorbell().doorbell;
            d
        }] {
            let good = encode_device_blob(&base).0;
            let len_field = good.len() - DOORBELL_BLOB_LEN - 4;
            let mut crafted = good[..len_field].to_vec();
            crafted.extend_from_slice(&0u32.to_le_bytes());
            assert!(
                matches!(
                    decode_device_blob(&crafted),
                    Err(SnapshotError::DeviceBlob(_))
                ),
                "a doorbell version with a zero-length doorbell record must fail closed"
            );
        }
    }

    #[test]
    fn decode_rejects_impossible_clockevent_and_pvclock_flags() {
        let good = encode_device_blob(&sample_with_pvclock()).0;
        let mut impossible = sample_with_pvclock();
        let pv = impossible.pvclock.as_mut().unwrap();
        pv.clockevent.deadline = Some(9);
        let impossible = encode_device_blob(&impossible).0;
        assert!(decode_device_blob(&impossible).is_err());

        let mut bad_bool = good;
        let line_flag = bad_bool.len() - 17;
        bad_bool[line_flag] = 2;
        assert!(decode_device_blob(&bad_bool).is_err());

        let mut armed = sample_with_pvclock();
        armed.pvclock.as_mut().unwrap().armed = false;
        let mut armed = encode_device_blob(&armed).0;
        let armed_flag = armed.len() - 20;
        armed[armed_flag] = 1;
        assert!(decode_device_blob(&armed).is_err());

        let mut missing_gpa_state = sample_with_pvclock();
        missing_gpa_state.pvclock.as_mut().unwrap().armed = false;
        let mut missing_gpa = encode_device_blob(&missing_gpa_state).0;
        let armed_flag = missing_gpa.len() - 20;
        let gpa_flag = armed_flag - 10;
        missing_gpa.drain(gpa_flag + 1..gpa_flag + 9);
        missing_gpa[gpa_flag] = 0;
        assert!(decode_device_blob(&missing_gpa).is_err());

        let mut nonregistrable = sample_with_pvclock();
        nonregistrable.pvclock.as_mut().unwrap().armed = false;
        nonregistrable.pvclock.as_mut().unwrap().registrable = false;
        assert!(decode_device_blob(&encode_device_blob(&nonregistrable).0).is_err());

        let mut pending = sample_with_pvclock();
        pending.pvclock.as_mut().unwrap().armed = false;
        let mut bad_armed = encode_device_blob(&pending).0;
        let armed_flag = bad_armed.len() - 20;
        bad_armed[armed_flag] = 2;
        assert!(decode_device_blob(&bad_armed).is_err());
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
