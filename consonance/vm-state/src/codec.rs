// SPDX-License-Identifier: AGPL-3.0-or-later
//! The TLV encoder, decoder, and version peek.
//!
//! Container layout (all integers little-endian):
//!
//! ```text
//! header:  magic:u32  version:u16  section_count:u16
//! section: tag:u16  len:u32  payload[len]      (repeated, ascending tag order)
//! ```
//!
//! Every v1 tag is present exactly once; sections are emitted in ascending tag
//! order. Decoding is strict and total — see [`VmStateError`].

use std::collections::{BTreeMap, BTreeSet};

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::error::VmStateError;
use crate::types::{
    DebugRegs, DeviceBlob, MpState, MsrBlock, TimerEntry, TimerQueueState, VcpuRegs, VcpuSregs,
    VtimeState, Xcrs, XsaveImage,
};
use crate::wire::{
    DebugRegsWire, EventsWire, HeaderWire, MsrPairWire, RegsWire, SregsWire, TimerEntryWire,
    VtimeWire, XcrsWire,
};
use crate::{VM_STATE_MAGIC, VM_STATE_VERSION, VmState};

// Section tags, in their canonical ascending order. Every v1 blob carries all
// of them exactly once; there are no optional sections.
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

/// The number of sections every v1 blob carries.
const SECTION_COUNT: u16 = 13;

/// Length of the fixed container header (magic + version + section count).
const HEADER_LEN: usize = 8;

const MP_STATE_RUNNABLE: u8 = 0;
const MP_STATE_HALTED: u8 = 1;

const CONTRACT_HASH_LEN: usize = 32;

impl VmState {
    /// Encode to the versioned TLV blob. Deterministic: equal `VmState` ⇒ equal
    /// bytes (MSRs via the `BTreeMap`'s sorted order; timer entries written in the
    /// `(deadline_vns, seq)` order the caller already holds them in — see the
    /// errors below; all fixed records fully initialized with no padding).
    ///
    /// # Errors
    ///
    /// - [`VmStateError::FractionalRatio`] if `vtime.ratio_den != 1` — such a
    ///   config cannot be restored exactly (INTEGRATION.md §4), so the blob is
    ///   refused rather than written.
    /// - [`VmStateError::InvalidField`] if `timers` violates a task-05
    ///   `TimerQueue` invariant: entries not strictly ascending/unique by
    ///   `(deadline_vns, seq)`, a duplicate `token`, or any `seq >= next_seq`
    ///   (see `validate_timers`). `encode` does **not** silently fix these —
    ///   silent canonicalization would break `decode(encode(s)?) == s`, so a
    ///   non-conforming queue is rejected and the caller fixes it.
    /// - [`VmStateError::InvalidField`] if a variable-length section would exceed
    ///   `u32::MAX` bytes (not reachable for any real machine state).
    pub fn encode(&self) -> Result<Vec<u8>, VmStateError> {
        // Integer-ratio invariant: enforce at the codec boundary so an
        // un-restorable-exactly timeline can never be written.
        if self.vtime.ratio_den != 1 {
            return Err(VmStateError::FractionalRatio);
        }

        let mut out = Vec::new();
        out.extend_from_slice(
            HeaderWire {
                magic: VM_STATE_MAGIC.into(),
                version: VM_STATE_VERSION.into(),
                section_count: SECTION_COUNT.into(),
            }
            .as_bytes(),
        );

        put_section(&mut out, TAG_REGS, RegsWire::from(&self.regs).as_bytes())?;
        put_section(&mut out, TAG_SREGS, SregsWire::from(&self.sregs).as_bytes())?;
        put_section(&mut out, TAG_XCRS, XcrsWire::from(&self.xcrs).as_bytes())?;
        put_section(
            &mut out,
            TAG_DEBUGREGS,
            DebugRegsWire::from(&self.debugregs).as_bytes(),
        )?;
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

        Ok(out)
    }

    /// Decode a blob produced by [`VmState::encode`]. Strict: validates magic,
    /// version, section count, ordering, and every field; never panics on
    /// arbitrary input.
    ///
    /// # Errors
    ///
    /// Returns the matching [`VmStateError`] for a bad magic, an unsupported
    /// version, a truncated or trailing buffer, an unknown / duplicate /
    /// out-of-order / missing section tag, or an out-of-range field value.
    pub fn decode(bytes: &[u8]) -> Result<VmState, VmStateError> {
        let header = HeaderWire::read_from_prefix(bytes)
            .map_err(|_| VmStateError::Truncated)?
            .0;
        let magic = header.magic.get();
        if magic != VM_STATE_MAGIC {
            return Err(VmStateError::BadMagic(magic));
        }
        let version = header.version.get();
        if version != VM_STATE_VERSION {
            return Err(VmStateError::UnsupportedVersion(version));
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

        for _ in 0..section_count {
            let tag = r.u16()?;
            let len = r.u32()? as usize;
            let payload = r.take(len)?;

            // Sections must be STRICTLY ascending, so each tag appears at most
            // once. `tag <= prev` means "not strictly greater than the previous":
            // equal is a duplicate, smaller is out of order. Folding both into one
            // comparison — rather than a separate `tag == prev` guard followed by
            // `tag < prev` — keeps the boundary observable: a duplicate-tag blob
            // distinguishes `<=` from `<`, and an out-of-order blob distinguishes
            // the inner `==`, so neither operator has an untestable mutant. (With
            // the split form the `<` was redundant with the earlier `==` return
            // and `< vs <=` was an equivalent mutant.)
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
                TAG_SREGS => sregs = Some(VcpuSregs::from(&read_fixed::<SregsWire>(payload)?)),
                TAG_XCRS => xcrs = Some(Xcrs::from(&read_fixed::<XcrsWire>(payload)?)),
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
                other => return Err(VmStateError::UnknownTag(other)),
            }
        }

        if !r.at_end() {
            return Err(VmStateError::TrailingBytes);
        }

        Ok(VmState {
            regs: regs.ok_or(VmStateError::MissingSection(TAG_REGS))?,
            sregs: sregs.ok_or(VmStateError::MissingSection(TAG_SREGS))?,
            xcrs: xcrs.ok_or(VmStateError::MissingSection(TAG_XCRS))?,
            debugregs: debugregs.ok_or(VmStateError::MissingSection(TAG_DEBUGREGS))?,
            events: events.ok_or(VmStateError::MissingSection(TAG_EVENTS))?,
            mp_state: mp_state.ok_or(VmStateError::MissingSection(TAG_MP_STATE))?,
            msrs: msrs.ok_or(VmStateError::MissingSection(TAG_MSRS))?,
            xsave: xsave.ok_or(VmStateError::MissingSection(TAG_XSAVE))?,
            vtime: vtime.ok_or(VmStateError::MissingSection(TAG_VTIME))?,
            timers: timers.ok_or(VmStateError::MissingSection(TAG_TIMERS))?,
            hypercall: hypercall.ok_or(VmStateError::MissingSection(TAG_HYPERCALL))?,
            devices: devices.ok_or(VmStateError::MissingSection(TAG_DEVICES))?,
            contract_hash: contract_hash.ok_or(VmStateError::MissingSection(TAG_CONTRACT_HASH))?,
        })
    }

    /// The format version a blob was written with, read from the header without
    /// decoding the body. Validates the magic but accepts any version (so a
    /// caller can distinguish an unsupported version from a corrupt blob).
    ///
    /// # Errors
    ///
    /// [`VmStateError::Truncated`] if the buffer is shorter than the header, or
    /// [`VmStateError::BadMagic`] if the magic does not match.
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

/// Append one `tag:u16 len:u32 payload` section.
fn put_section(out: &mut Vec<u8>, tag: u16, payload: &[u8]) -> Result<(), VmStateError> {
    let len = u32::try_from(payload.len()).map_err(|_| VmStateError::InvalidField)?;
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

/// Read a fixed-layout wire record from an exact-length payload, mapping a size
/// mismatch to [`VmStateError::InvalidField`].
fn read_fixed<W: FromBytes + KnownLayout + Immutable>(payload: &[u8]) -> Result<W, VmStateError> {
    W::read_from_bytes(payload).map_err(|_| VmStateError::InvalidField)
}

fn encode_mp_state(mp: MpState) -> u8 {
    match mp {
        MpState::Runnable => MP_STATE_RUNNABLE,
        MpState::Halted => MP_STATE_HALTED,
    }
}

fn decode_mp_state(payload: &[u8]) -> Result<MpState, VmStateError> {
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
    // BTreeMap iterates in ascending key order — deterministic regardless of
    // the order MSRs were captured/inserted in.
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
    for chunk in body.chunks_exact(12) {
        let pair = read_fixed::<MsrPairWire>(chunk)?;
        let index = pair.index.get();
        // Strictly ascending indices: rejects a duplicate or out-of-order list
        // and guarantees the BTreeMap round-trips exactly.
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

/// Validate the task-05 `TimerQueue` invariants a queue must satisfy to restore
/// faithfully. Any violation is [`VmStateError::InvalidField`]:
///
/// 1. **Canonical firing order** — entries strictly ascending and unique by
///    `(deadline_vns, seq)` (task-05 fires same-deadline timers in `seq`/FIFO
///    order, so this is the order they must be stored and replayed in).
/// 2. **Unique tokens** — task-05's queue keys a `token -> entry` index, so a
///    duplicate `token` would make a later cancel/reschedule hit the wrong entry.
/// 3. **`seq < next_seq`** — `next_seq` is the queue's next insertion counter; a
///    stored `seq >= next_seq` would collide with the seq the restored queue
///    hands out for its next same-deadline insertion.
///
/// Checking here (rather than silently fixing) is what makes
/// `decode(encode(s)?) == s` hold for every `VmState` `encode` accepts.
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

fn encode_timers(timers: &TimerQueueState) -> Result<Vec<u8>, VmStateError> {
    let count = u32::try_from(timers.entries.len()).map_err(|_| VmStateError::InvalidField)?;
    // Entries must already satisfy the task-05 TimerQueue invariants (see
    // validate_timers): canonical (deadline_vns, seq) order, unique tokens, and
    // every seq < next_seq. Reject a non-conforming queue rather than silently
    // fixing it, so the round-trip contract holds for every accepted VmState.
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

fn decode_timers(payload: &[u8]) -> Result<TimerQueueState, VmStateError> {
    let next_seq = le_u64(payload, 0)?;
    let count = le_u32(payload, 8)? as usize;
    let body = payload.get(12..).ok_or(VmStateError::InvalidField)?;
    let want = count.checked_mul(32).ok_or(VmStateError::InvalidField)?;
    if body.len() != want {
        return Err(VmStateError::InvalidField);
    }
    let mut entries = Vec::with_capacity(count);
    for chunk in body.chunks_exact(32) {
        let w = read_fixed::<TimerEntryWire>(chunk)?;
        entries.push(TimerEntry {
            deadline_vns: w.deadline_vns.get(),
            seq: w.seq.get(),
            token: w.token.get(),
            period_vns: w.period_vns.get(),
        });
    }
    // Enforce the same task-05 invariants on decode that encode does, so a
    // hand-crafted blob can't smuggle in a queue that wouldn't restore faithfully
    // (decode stays strict and symmetric with encode).
    validate_timers(&entries, next_seq)?;
    Ok(TimerQueueState { entries, next_seq })
}

fn decode_contract_hash(payload: &[u8]) -> Result<[u8; 32], VmStateError> {
    <[u8; CONTRACT_HASH_LEN]>::try_from(payload).map_err(|_| VmStateError::InvalidField)
}

/// Read a little-endian `u32` at `offset`, or [`VmStateError::InvalidField`] if
/// the slice is too short (a malformed section, not a truncated buffer).
fn le_u32(buf: &[u8], offset: usize) -> Result<u32, VmStateError> {
    let bytes = buf
        .get(offset..offset + 4)
        .ok_or(VmStateError::InvalidField)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Read a little-endian `u64` at `offset`, or [`VmStateError::InvalidField`].
fn le_u64(buf: &[u8], offset: usize) -> Result<u64, VmStateError> {
    let bytes = buf
        .get(offset..offset + 8)
        .ok_or(VmStateError::InvalidField)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

/// A forward-only cursor over the section stream. Every read that would pass the
/// end of the buffer yields [`VmStateError::Truncated`].
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn at_end(&self) -> bool {
        self.pos == self.buf.len()
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], VmStateError> {
        let end = self.pos.checked_add(n).ok_or(VmStateError::Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(VmStateError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16, VmStateError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, VmStateError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}
