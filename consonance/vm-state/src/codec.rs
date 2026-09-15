// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet};

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::error::VmStateError;
use crate::types::{
    DebugRegs, DeviceBlob, MpState, MsrBlock, TimerEntry, TimerQueueState, VcpuRegs, VcpuSregs,
    VtimeState, Xcrs, XsaveImage,
};
use crate::wire::{
    DebugRegsWire, DebugRegsWireV5, EventsWire, HeaderWire, MsrPairWire, RegsWire, SregsWire,
    SregsWireV5, TimerEntryWire, VtimeWire, XcrsWire, XsaveRestoreBvWire,
};
use crate::{
    ARCH_X86_64, VM_STATE_CPU_VERSION, VM_STATE_ENGINE_VERSION, VM_STATE_LEGACY_VERSION,
    VM_STATE_MAGIC, VM_STATE_VERSION, VmState,
};

const TAG_REGS: u16 = 1;
const TAG_SREGS: u16 = 2;
const TAG_XCRS: u16 = 3;
const TAG_DEBUGREGS: u16 = 4;
const TAG_EVENTS: u16 = 5;
const TAG_MP_STATE: u16 = 6;
const TAG_MSRS: u16 = 7;
const TAG_XSAVE: u16 = 8;
const TAG_VTIME: u16 = 9;
const TAG_TIMERS: u16 = 10;
const TAG_HYPERCALL: u16 = 11;
const TAG_DEVICES: u16 = 12;
const TAG_CONTRACT_HASH: u16 = 13;

const LEGACY_SECTION_COUNT: u16 = 13;

const TAG_ENGINE_STATE: u16 = 14;

const TAG_XSAVE_RESTORE_BV: u16 = 15;

const HEADER_LEN: usize = 10;

const MP_STATE_RUNNABLE: u8 = 0;
const MP_STATE_HALTED: u8 = 1;

const CONTRACT_HASH_LEN: usize = 32;

impl VmState {
    pub fn encode(&self) -> Result<Vec<u8>, VmStateError> {
        let extended_cpu = has_extended_cpu_fields(self);
        let version = if self.xsave_restore_bv.is_some() {
            VM_STATE_VERSION
        } else if extended_cpu {
            VM_STATE_CPU_VERSION
        } else if self.engine_state.is_empty() {
            VM_STATE_LEGACY_VERSION
        } else {
            VM_STATE_ENGINE_VERSION
        };
        let section_count = LEGACY_SECTION_COUNT
            + u16::from(!self.engine_state.is_empty())
            + u16::from(self.xsave_restore_bv.is_some());
        let mut out = Vec::new();
        out.extend_from_slice(
            HeaderWire {
                magic: VM_STATE_MAGIC.into(),
                version: version.into(),
                arch: ARCH_X86_64.into(),
                section_count: section_count.into(),
            }
            .as_bytes(),
        );

        put_section(&mut out, TAG_REGS, RegsWire::from(&self.regs).as_bytes())?;
        if version == VM_STATE_CPU_VERSION || version == VM_STATE_VERSION {
            put_section(
                &mut out,
                TAG_SREGS,
                SregsWireV5::from(&self.sregs).as_bytes(),
            )?;
        } else {
            put_section(&mut out, TAG_SREGS, SregsWire::from(&self.sregs).as_bytes())?;
        }
        put_section(&mut out, TAG_XCRS, XcrsWire::from(&self.xcrs).as_bytes())?;
        if version == VM_STATE_CPU_VERSION || version == VM_STATE_VERSION {
            put_section(
                &mut out,
                TAG_DEBUGREGS,
                DebugRegsWireV5::from(&self.debugregs).as_bytes(),
            )?;
        } else {
            put_section(
                &mut out,
                TAG_DEBUGREGS,
                DebugRegsWire::from(&self.debugregs).as_bytes(),
            )?;
        }
        put_section(
            &mut out,
            TAG_EVENTS,
            EventsWire::from(&self.events).as_bytes(),
        )?;
        put_section(&mut out, TAG_MP_STATE, &[encode_mp_state(self.mp_state)])?;
        put_section(&mut out, TAG_MSRS, &encode_msrs(&self.msrs)?)?;
        put_section(&mut out, TAG_XSAVE, &self.xsave.0)?;
        put_section(&mut out, TAG_VTIME, VtimeWire::from(&self.vtime).as_bytes())?;
        put_section(&mut out, TAG_TIMERS, &encode_timers(&self.timers)?)?;
        put_section(&mut out, TAG_HYPERCALL, &self.hypercall)?;
        put_section(&mut out, TAG_DEVICES, &self.devices.0)?;
        put_section(&mut out, TAG_CONTRACT_HASH, &self.contract_hash)?;
        if !self.engine_state.is_empty() {
            put_section(&mut out, TAG_ENGINE_STATE, &self.engine_state)?;
        }
        if let Some(value) = self.xsave_restore_bv {
            put_section(
                &mut out,
                TAG_XSAVE_RESTORE_BV,
                XsaveRestoreBvWire::from(value).as_bytes(),
            )?;
        }

        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<VmState, VmStateError> {
        let header = HeaderWire::read_from_prefix(bytes)
            .map_err(|_| VmStateError::Truncated)?
            .0;
        let magic = header.magic.get();
        if magic != VM_STATE_MAGIC {
            return Err(VmStateError::BadMagic(magic));
        }
        let version = header.version.get();
        if version != VM_STATE_LEGACY_VERSION
            && version != VM_STATE_ENGINE_VERSION
            && version != VM_STATE_CPU_VERSION
            && version != VM_STATE_VERSION
        {
            return Err(VmStateError::UnsupportedVersion(version));
        }
        let arch = header.arch.get();
        if arch != ARCH_X86_64 {
            return Err(VmStateError::UnsupportedArch(arch));
        }
        let section_count = header.section_count.get();

        let mut r = Reader::new(&bytes[HEADER_LEN..]);
        let mut last_tag: Option<u16> = None;

        let mut regs = None;
        let mut sregs = None;
        let mut xcrs = None;
        let mut debugregs = None;
        let mut events = None;
        let mut mp_state = None;
        let mut msrs = None;
        let mut xsave = None;
        let mut vtime = None;
        let mut timers = None;
        let mut hypercall = None;
        let mut devices = None;
        let mut contract_hash = None;
        let mut engine_state = None;
        let mut xsave_restore_bv = None;

        for _ in 0..section_count {
            let tag = r.u16()?;
            let len = r.u32()? as usize;
            let payload = r.take(len)?;

            if let Some(prev) = last_tag
                && tag <= prev
            {
                return Err(if tag == prev {
                    VmStateError::DuplicateTag(tag)
                } else {
                    VmStateError::SectionOrder(tag)
                });
            }
            last_tag = Some(tag);

            match tag {
                TAG_REGS => regs = Some(VcpuRegs::from(&read_fixed::<RegsWire>(payload)?)),
                TAG_SREGS if version == VM_STATE_CPU_VERSION || version == VM_STATE_VERSION => {
                    sregs = Some(VcpuSregs::from(&read_fixed::<SregsWireV5>(payload)?));
                }
                TAG_SREGS => sregs = Some(VcpuSregs::from(&read_fixed::<SregsWire>(payload)?)),
                TAG_XCRS => xcrs = Some(Xcrs::from(&read_fixed::<XcrsWire>(payload)?)),
                TAG_DEBUGREGS if version == VM_STATE_CPU_VERSION || version == VM_STATE_VERSION => {
                    debugregs = Some(DebugRegs::from(&read_fixed::<DebugRegsWireV5>(payload)?));
                }
                TAG_DEBUGREGS => {
                    debugregs = Some(DebugRegs::from(&read_fixed::<DebugRegsWire>(payload)?));
                }
                TAG_EVENTS => {
                    events = Some(
                        read_fixed::<EventsWire>(payload)?
                            .to_events()
                            .ok_or(VmStateError::InvalidField)?,
                    );
                }
                TAG_MP_STATE => mp_state = Some(decode_mp_state(payload)?),
                TAG_MSRS => msrs = Some(decode_msrs(payload)?),
                TAG_XSAVE => xsave = Some(XsaveImage(payload.to_vec())),
                TAG_VTIME => vtime = Some(VtimeState::from(&read_fixed::<VtimeWire>(payload)?)),
                TAG_TIMERS => timers = Some(decode_timers(payload)?),
                TAG_HYPERCALL => hypercall = Some(payload.to_vec()),
                TAG_DEVICES => devices = Some(DeviceBlob(payload.to_vec())),
                TAG_CONTRACT_HASH => contract_hash = Some(decode_contract_hash(payload)?),
                TAG_ENGINE_STATE
                    if version == VM_STATE_ENGINE_VERSION
                        || version == VM_STATE_CPU_VERSION
                        || version == VM_STATE_VERSION =>
                {
                    engine_state = Some(payload.to_vec());
                }
                TAG_ENGINE_STATE => return Err(VmStateError::UnknownTag(TAG_ENGINE_STATE)),
                TAG_XSAVE_RESTORE_BV if version == VM_STATE_VERSION => {
                    let value = read_fixed::<XsaveRestoreBvWire>(payload)?;
                    xsave_restore_bv = Some((&value).into());
                }
                TAG_XSAVE_RESTORE_BV => {
                    return Err(VmStateError::UnknownTag(TAG_XSAVE_RESTORE_BV));
                }
                other => return Err(VmStateError::UnknownTag(other)),
            }
        }

        if !r.at_end() {
            return Err(VmStateError::TrailingBytes);
        }

        let engine_state = match version {
            VM_STATE_LEGACY_VERSION => Vec::new(),
            VM_STATE_ENGINE_VERSION => {
                let state = engine_state.ok_or(VmStateError::MissingSection(TAG_ENGINE_STATE))?;
                if state.is_empty() {
                    return Err(VmStateError::InvalidField);
                }
                state
            }
            VM_STATE_CPU_VERSION => {
                let sregs = sregs
                    .as_ref()
                    .ok_or(VmStateError::MissingSection(TAG_SREGS))?;
                let debugregs = debugregs
                    .as_ref()
                    .ok_or(VmStateError::MissingSection(TAG_DEBUGREGS))?;
                if !has_extended_cpu_values(sregs, debugregs) {
                    return Err(VmStateError::InvalidField);
                }
                match engine_state {
                    Some(state) if state.is_empty() => return Err(VmStateError::InvalidField),
                    Some(state) => state,
                    None => Vec::new(),
                }
            }
            VM_STATE_VERSION => {
                let restore_bv =
                    xsave_restore_bv.ok_or(VmStateError::MissingSection(TAG_XSAVE_RESTORE_BV))?;
                let engine_state = match engine_state {
                    Some(state) if state.is_empty() => return Err(VmStateError::InvalidField),
                    Some(state) => state,
                    None => Vec::new(),
                };
                xsave_restore_bv = Some(restore_bv);
                engine_state
            }
            _ => unreachable!("version was checked above"),
        };

        Ok(VmState {
            regs: regs.ok_or(VmStateError::MissingSection(TAG_REGS))?,
            sregs: sregs.ok_or(VmStateError::MissingSection(TAG_SREGS))?,
            xcrs: xcrs.ok_or(VmStateError::MissingSection(TAG_XCRS))?,
            debugregs: debugregs.ok_or(VmStateError::MissingSection(TAG_DEBUGREGS))?,
            events: events.ok_or(VmStateError::MissingSection(TAG_EVENTS))?,
            mp_state: mp_state.ok_or(VmStateError::MissingSection(TAG_MP_STATE))?,
            msrs: msrs.ok_or(VmStateError::MissingSection(TAG_MSRS))?,
            xsave: xsave.ok_or(VmStateError::MissingSection(TAG_XSAVE))?,
            xsave_restore_bv,
            vtime: vtime.ok_or(VmStateError::MissingSection(TAG_VTIME))?,
            timers: timers.ok_or(VmStateError::MissingSection(TAG_TIMERS))?,
            hypercall: hypercall.ok_or(VmStateError::MissingSection(TAG_HYPERCALL))?,
            devices: devices.ok_or(VmStateError::MissingSection(TAG_DEVICES))?,
            contract_hash: contract_hash.ok_or(VmStateError::MissingSection(TAG_CONTRACT_HASH))?,
            engine_state,
        })
    }

    pub fn peek_version(bytes: &[u8]) -> Result<u16, VmStateError> {
        let header = HeaderWire::read_from_prefix(bytes)
            .map_err(|_| VmStateError::Truncated)?
            .0;
        let magic = header.magic.get();
        if magic != VM_STATE_MAGIC {
            return Err(VmStateError::BadMagic(magic));
        }
        Ok(header.version.get())
    }
}

fn has_extended_cpu_fields(state: &VmState) -> bool {
    has_extended_cpu_values(&state.sregs, &state.debugregs)
}

fn has_extended_cpu_values(sregs: &VcpuSregs, debugregs: &DebugRegs) -> bool {
    sregs.flags != 0 || sregs.pdptrs != [0; 4] || debugregs.flags != 0
}

pub(crate) fn put_section(out: &mut Vec<u8>, tag: u16, payload: &[u8]) -> Result<(), VmStateError> {
    let len = u32::try_from(payload.len()).map_err(|_| VmStateError::InvalidField)?;
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

pub(crate) fn read_fixed<W: FromBytes + KnownLayout + Immutable>(
    payload: &[u8],
) -> Result<W, VmStateError> {
    W::read_from_bytes(payload).map_err(|_| VmStateError::InvalidField)
}

pub(crate) fn encode_mp_state(mp: MpState) -> u8 {
    match mp {
        MpState::Runnable => MP_STATE_RUNNABLE,
        MpState::Halted => MP_STATE_HALTED,
    }
}

pub(crate) fn decode_mp_state(payload: &[u8]) -> Result<MpState, VmStateError> {
    match payload {
        [MP_STATE_RUNNABLE] => Ok(MpState::Runnable),
        [MP_STATE_HALTED] => Ok(MpState::Halted),
        _ => Err(VmStateError::InvalidField),
    }
}

fn encode_msrs(msrs: &MsrBlock) -> Result<Vec<u8>, VmStateError> {
    let count = u32::try_from(msrs.0.len()).map_err(|_| VmStateError::InvalidField)?;
    let mut payload = Vec::with_capacity(4 + msrs.0.len() * 12);
    payload.extend_from_slice(&count.to_le_bytes());
    for (&index, &value) in &msrs.0 {
        let pair = MsrPairWire {
            index: index.into(),
            value: value.into(),
        };
        payload.extend_from_slice(pair.as_bytes());
    }
    Ok(payload)
}

fn decode_msrs(payload: &[u8]) -> Result<MsrBlock, VmStateError> {
    let count = le_u32(payload, 0)? as usize;
    let body = payload.get(4..).ok_or(VmStateError::InvalidField)?;
    let want = count.checked_mul(12).ok_or(VmStateError::InvalidField)?;
    if body.len() != want {
        return Err(VmStateError::InvalidField);
    }
    let mut map = BTreeMap::new();
    let mut prev: Option<u32> = None;
    let (chunks, remainder) = body.as_chunks::<12>();
    debug_assert!(remainder.is_empty());
    for chunk in chunks {
        let pair = read_fixed::<MsrPairWire>(chunk)?;
        let index = pair.index.get();
        if let Some(p) = prev
            && index <= p
        {
            return Err(VmStateError::InvalidField);
        }
        prev = Some(index);
        map.insert(index, pair.value.get());
    }
    Ok(MsrBlock(map))
}

fn validate_timers(entries: &[TimerEntry], next_seq: u64) -> Result<(), VmStateError> {
    let mut prev_key: Option<(u64, u64)> = None;
    let mut tokens = BTreeSet::new();
    for e in entries {
        let key = (e.deadline_vns, e.seq);
        if let Some(p) = prev_key
            && key <= p
        {
            return Err(VmStateError::InvalidField);
        }
        prev_key = Some(key);
        if e.seq >= next_seq {
            return Err(VmStateError::InvalidField);
        }
        if !tokens.insert(e.token) {
            return Err(VmStateError::InvalidField);
        }
    }
    Ok(())
}

pub(crate) fn encode_timers(timers: &TimerQueueState) -> Result<Vec<u8>, VmStateError> {
    let count = u32::try_from(timers.entries.len()).map_err(|_| VmStateError::InvalidField)?;
    validate_timers(&timers.entries, timers.next_seq)?;

    let mut payload = Vec::with_capacity(12 + timers.entries.len() * 32);
    payload.extend_from_slice(&timers.next_seq.to_le_bytes());
    payload.extend_from_slice(&count.to_le_bytes());
    for e in &timers.entries {
        let w = TimerEntryWire {
            deadline_vns: e.deadline_vns.into(),
            seq: e.seq.into(),
            token: e.token.into(),
            period_vns: e.period_vns.into(),
        };
        payload.extend_from_slice(w.as_bytes());
    }
    Ok(payload)
}

pub(crate) fn decode_timers(payload: &[u8]) -> Result<TimerQueueState, VmStateError> {
    let next_seq = le_u64(payload, 0)?;
    let count = le_u32(payload, 8)? as usize;
    let body = payload.get(12..).ok_or(VmStateError::InvalidField)?;
    let want = count.checked_mul(32).ok_or(VmStateError::InvalidField)?;
    if body.len() != want {
        return Err(VmStateError::InvalidField);
    }
    let mut entries = Vec::with_capacity(count);
    let (chunks, remainder) = body.as_chunks::<32>();
    debug_assert!(remainder.is_empty());
    for chunk in chunks {
        let w = read_fixed::<TimerEntryWire>(chunk)?;
        entries.push(TimerEntry {
            deadline_vns: w.deadline_vns.get(),
            seq: w.seq.get(),
            token: w.token.get(),
            period_vns: w.period_vns.get(),
        });
    }
    validate_timers(&entries, next_seq)?;
    Ok(TimerQueueState { entries, next_seq })
}

pub(crate) fn decode_contract_hash(payload: &[u8]) -> Result<[u8; 32], VmStateError> {
    <[u8; CONTRACT_HASH_LEN]>::try_from(payload).map_err(|_| VmStateError::InvalidField)
}

pub(crate) fn le_u32(buf: &[u8], offset: usize) -> Result<u32, VmStateError> {
    let bytes = buf
        .get(offset..offset + 4)
        .ok_or(VmStateError::InvalidField)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub(crate) fn le_u64(buf: &[u8], offset: usize) -> Result<u64, VmStateError> {
    let bytes = buf
        .get(offset..offset + 8)
        .ok_or(VmStateError::InvalidField)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub(crate) fn at_end(&self) -> bool {
        self.pos == self.buf.len()
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], VmStateError> {
        let end = self.pos.checked_add(n).ok_or(VmStateError::Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(VmStateError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    pub(crate) fn u16(&mut self) -> Result<u16, VmStateError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, VmStateError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}
