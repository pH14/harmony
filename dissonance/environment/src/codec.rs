// SPDX-License-Identifier: AGPL-3.0-or-later
//! Byte-exact, panic-free serialization shared by every public `encode`/`decode`
//! method ([`Answer`], [`FaultPolicy`](crate::FaultPolicy), [`EnvSpec`](crate::EnvSpec)).
//!
//! All integers are little-endian. Writing walks state in a fixed/sorted order
//! so equal values always yield identical bytes (no `HashMap` order reaches a
//! byte). Reading is strict and total: every length and tag is bounds-checked
//! against the *actual* buffer before use, so arbitrary input can only produce
//! [`EnvError::Malformed`] / [`EnvError::BadVersion`], never a panic or an
//! out-of-bounds read (conventions rule 4).

use crate::catalog::{Answer, CorruptSpec, Fault};
use crate::error::EnvError;
use crate::host::{Action, BitMask, HostFault, Ratio};
use crate::{MAX_SUPPLY_LEN, VTime};

// Answer tags.
const ANS_NOMINAL: u8 = 0;
const ANS_SUPPLY: u8 = 1;
const ANS_FAULT: u8 = 2;

// Action plane tags — which control plane an override belongs to.
const ACT_HOST: u8 = 0;
const ACT_GUEST: u8 = 1;

// HostFault tags — stable discriminants; a recorded reproducer's replay depends
// on them, exactly like the guest `Fault` tags above.
const HF_SKEW_TIME: u8 = 0;
const HF_SET_CLOCK_RATE: u8 = 1;
const HF_CORRUPT_MEMORY: u8 = 2;
const HF_INJECT_INTERRUPT: u8 = 3;

// Fault tags — stable discriminants; a recorded EnvSpec replay depends on them.
const F_NET_DROP: u8 = 0;
const F_NET_DELAY: u8 = 1;
const F_NET_REORDER: u8 = 2;
const F_NET_DUP: u8 = 3;
const F_NET_CORRUPT: u8 = 4;
const F_BLOCK_EIO: u8 = 5;
const F_BLOCK_LATENCY: u8 = 6;
const F_BLOCK_TORN: u8 = 7;
const F_BLOCK_NOSPC: u8 = 8;
const F_PROC_PAUSE: u8 = 9;
const F_PROC_KILL: u8 = 10;
const F_PROC_RESTART: u8 = 11;

/// Append a `u16` little-endian.
pub(crate) fn put_u16(w: &mut Vec<u8>, v: u16) {
    w.extend_from_slice(&v.to_le_bytes());
}

/// Append a `u32` little-endian.
pub(crate) fn put_u32(w: &mut Vec<u8>, v: u32) {
    w.extend_from_slice(&v.to_le_bytes());
}

/// Append a `u64` little-endian.
pub(crate) fn put_u64(w: &mut Vec<u8>, v: u64) {
    w.extend_from_slice(&v.to_le_bytes());
}

/// Append a count as a `u32`, saturating (the counts here are never near
/// `u32::MAX`; saturation keeps the path total rather than panicking).
pub(crate) fn put_len(w: &mut Vec<u8>, n: usize) {
    put_u32(w, u32::try_from(n).unwrap_or(u32::MAX));
}

/// Append a `u32`-length-prefixed byte blob.
pub(crate) fn put_bytes(w: &mut Vec<u8>, b: &[u8]) {
    put_len(w, b.len());
    w.extend_from_slice(b);
}

/// Serialize one [`Fault`].
pub(crate) fn write_fault(w: &mut Vec<u8>, f: &Fault) {
    match f {
        Fault::NetDrop => w.push(F_NET_DROP),
        Fault::NetDelay(VTime(d)) => {
            w.push(F_NET_DELAY);
            put_u64(w, *d);
        }
        Fault::NetReorder => w.push(F_NET_REORDER),
        Fault::NetDup => w.push(F_NET_DUP),
        Fault::NetCorrupt(CorruptSpec { offset, xor }) => {
            w.push(F_NET_CORRUPT);
            put_u32(w, *offset);
            w.push(*xor);
        }
        Fault::BlockEio => w.push(F_BLOCK_EIO),
        Fault::BlockLatency(VTime(d)) => {
            w.push(F_BLOCK_LATENCY);
            put_u64(w, *d);
        }
        Fault::BlockTorn(n) => {
            w.push(F_BLOCK_TORN);
            put_u32(w, *n);
        }
        Fault::BlockNospc => w.push(F_BLOCK_NOSPC),
        Fault::ProcPause(VTime(d)) => {
            w.push(F_PROC_PAUSE);
            put_u64(w, *d);
        }
        Fault::ProcKill => w.push(F_PROC_KILL),
        Fault::ProcRestart => w.push(F_PROC_RESTART),
    }
}

/// Deserialize one [`Fault`].
pub(crate) fn read_fault(r: &mut Reader) -> Result<Fault, EnvError> {
    let f = match r.u8()? {
        F_NET_DROP => Fault::NetDrop,
        F_NET_DELAY => Fault::NetDelay(VTime(r.u64()?)),
        F_NET_REORDER => Fault::NetReorder,
        F_NET_DUP => Fault::NetDup,
        F_NET_CORRUPT => Fault::NetCorrupt(CorruptSpec {
            offset: r.u32()?,
            xor: r.u8()?,
        }),
        F_BLOCK_EIO => Fault::BlockEio,
        F_BLOCK_LATENCY => Fault::BlockLatency(VTime(r.u64()?)),
        F_BLOCK_TORN => Fault::BlockTorn(r.u32()?),
        F_BLOCK_NOSPC => Fault::BlockNospc,
        F_PROC_PAUSE => Fault::ProcPause(VTime(r.u64()?)),
        F_PROC_KILL => Fault::ProcKill,
        F_PROC_RESTART => Fault::ProcRestart,
        _ => return Err(EnvError::Malformed),
    };
    Ok(f)
}

/// Serialize one [`Answer`].
pub(crate) fn write_answer(w: &mut Vec<u8>, a: &Answer) {
    match a {
        Answer::Nominal => w.push(ANS_NOMINAL),
        Answer::Supply(v) => {
            w.push(ANS_SUPPLY);
            put_bytes(w, v);
        }
        Answer::Fault(f) => {
            w.push(ANS_FAULT);
            write_fault(w, f);
        }
    }
}

/// Deserialize one [`Answer`]. A [`Answer::Supply`] longer than
/// [`MAX_SUPPLY_LEN`] is rejected (it could never be admissible at the seam).
pub(crate) fn read_answer(r: &mut Reader) -> Result<Answer, EnvError> {
    let a = match r.u8()? {
        ANS_NOMINAL => Answer::Nominal,
        ANS_SUPPLY => {
            let b = r.bytes()?;
            if b.len() > MAX_SUPPLY_LEN as usize {
                return Err(EnvError::Malformed);
            }
            Answer::Supply(b.to_vec())
        }
        ANS_FAULT => Answer::Fault(read_fault(r)?),
        _ => return Err(EnvError::Malformed),
    };
    Ok(a)
}

/// Serialize one [`HostFault`].
pub(crate) fn write_host_fault(w: &mut Vec<u8>, f: &HostFault) {
    match f {
        HostFault::SkewTime(VTime(d)) => {
            w.push(HF_SKEW_TIME);
            put_u64(w, *d);
        }
        HostFault::SetClockRate(r) => {
            w.push(HF_SET_CLOCK_RATE);
            put_u64(w, r.num());
            put_u64(w, r.den());
        }
        HostFault::CorruptMemory {
            gpa,
            mask: BitMask(mask),
        } => {
            w.push(HF_CORRUPT_MEMORY);
            put_u64(w, *gpa);
            put_u64(w, *mask);
        }
        HostFault::InjectInterrupt { vector } => {
            w.push(HF_INJECT_INTERRUPT);
            w.push(*vector);
        }
    }
}

/// Deserialize one [`HostFault`]. A zero `Ratio` denominator is rejected (a
/// constructed `Ratio` can never hold one), so a mutated blob cannot smuggle in a
/// divide-by-zero.
pub(crate) fn read_host_fault(r: &mut Reader) -> Result<HostFault, EnvError> {
    let f = match r.u8()? {
        HF_SKEW_TIME => HostFault::SkewTime(VTime(r.u64()?)),
        HF_SET_CLOCK_RATE => {
            let num = r.u64()?;
            let den = r.u64()?;
            HostFault::SetClockRate(Ratio::new(num, den).ok_or(EnvError::Malformed)?)
        }
        HF_CORRUPT_MEMORY => HostFault::CorruptMemory {
            gpa: r.u64()?,
            mask: BitMask(r.u64()?),
        },
        HF_INJECT_INTERRUPT => HostFault::InjectInterrupt { vector: r.u8()? },
        _ => return Err(EnvError::Malformed),
    };
    Ok(f)
}

/// Serialize one [`Action`] (a one-byte plane tag, then the plane's encoding).
pub(crate) fn write_action(w: &mut Vec<u8>, a: &Action) {
    match a {
        Action::Host(f) => {
            w.push(ACT_HOST);
            write_host_fault(w, f);
        }
        Action::Guest(ans) => {
            w.push(ACT_GUEST);
            write_answer(w, ans);
        }
    }
}

/// Deserialize one [`Action`].
pub(crate) fn read_action(r: &mut Reader) -> Result<Action, EnvError> {
    let a = match r.u8()? {
        ACT_HOST => Action::Host(read_host_fault(r)?),
        ACT_GUEST => Action::Guest(read_answer(r)?),
        _ => return Err(EnvError::Malformed),
    };
    Ok(a)
}

/// A forward-only cursor over a byte buffer. Every read past end-of-buffer is
/// [`EnvError::Malformed`]; byte blobs are sliced (bounds-checked against the
/// real buffer) before any copy, so an untrusted length can never force an
/// out-of-bounds read or an unbounded allocation.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Wrap a buffer.
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Whether every byte has been consumed (used to reject trailing bytes).
    pub(crate) fn at_end(&self) -> bool {
        self.pos == self.buf.len()
    }

    /// Borrow the next `n` bytes, advancing the cursor.
    fn take(&mut self, n: usize) -> Result<&'a [u8], EnvError> {
        let end = self.pos.checked_add(n).ok_or(EnvError::Malformed)?;
        let slice = self.buf.get(self.pos..end).ok_or(EnvError::Malformed)?;
        self.pos = end;
        Ok(slice)
    }

    /// Read a `u8`.
    pub(crate) fn u8(&mut self) -> Result<u8, EnvError> {
        Ok(self.take(1)?[0])
    }

    /// Read a `u16` little-endian.
    pub(crate) fn u16(&mut self) -> Result<u16, EnvError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    /// Read a `u32` little-endian.
    pub(crate) fn u32(&mut self) -> Result<u32, EnvError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read a `u64` little-endian.
    pub(crate) fn u64(&mut self) -> Result<u64, EnvError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Read a `u32`-length-prefixed byte blob.
    pub(crate) fn bytes(&mut self) -> Result<&'a [u8], EnvError> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}
