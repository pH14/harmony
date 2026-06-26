// SPDX-License-Identifier: AGPL-3.0-or-later
//! Byte-deterministic serialization of the switch's mutable state.
//!
//! Layout (all integers little-endian):
//!
//! ```text
//! header:     magic:u32  version:u16
//! scalars:    l0:u64  next_seq:u64
//! partitions: count:u32  then [a:u32 b:u32 start:u64 end:u64]              (a<=b, strictly ascending)
//! throttles:  count:u32  then [src:u32 dst:u32 max:u32 per:u64
//!                              start:u64 end:u64 cur_index:u64 count:u32]  (ascending by link)
//! pending:    count:u32  then [at:u64 seq:u64 dst:u32 len:u32 frame[len]]  (ascending by (at,seq))
//! held:       count:u32  then [src:u32 dst:u32 nframes:u32
//!                              then [horizon:u64 len:u32 frame[len]]·nframes] (ascending by link, nframes>=1)
//! ```
//!
//! Encoding walks the `BTreeSet`/`BTreeMap` state in sorted order, so equal state
//! always yields identical bytes. Decoding is strict and total — every ordering,
//! count, length, and state invariant is checked, and any violation is
//! [`NetError::Malformed`]; it never panics on arbitrary input (conventions
//! rule 4).

use std::collections::{BTreeMap, BTreeSet};

use crate::error::NetError;
use crate::switch::{HeldFrame, Link, Switch, Throttle};
use crate::types::NetDeliver;
use crate::{NodeId, VTime};

/// Container magic: `"PVN1"` read little-endian.
const MAGIC: u32 = 0x314E_5650;
/// The format version this build writes and is the only version it decodes.
const VERSION: u16 = 1;

/// Encode the switch's mutable state to a byte-deterministic blob.
pub(crate) fn encode(s: &Switch) -> Vec<u8> {
    let mut w = Vec::new();
    w.extend_from_slice(&MAGIC.to_le_bytes());
    w.extend_from_slice(&VERSION.to_le_bytes());
    w.extend_from_slice(&s.l0.0.to_le_bytes());
    w.extend_from_slice(&s.next_seq.to_le_bytes());

    put_u32(&mut w, s.partitions.len());
    for &(a, b, start, end) in &s.partitions {
        w.extend_from_slice(&a.0.to_le_bytes());
        w.extend_from_slice(&b.0.to_le_bytes());
        w.extend_from_slice(&start.0.to_le_bytes());
        w.extend_from_slice(&end.0.to_le_bytes());
    }

    put_u32(&mut w, s.throttles.len());
    for (link, t) in &s.throttles {
        w.extend_from_slice(&link.0.0.to_le_bytes());
        w.extend_from_slice(&link.1.0.to_le_bytes());
        w.extend_from_slice(&t.max.to_le_bytes());
        w.extend_from_slice(&t.per.0.to_le_bytes());
        w.extend_from_slice(&t.start.0.to_le_bytes());
        w.extend_from_slice(&t.end.0.to_le_bytes());
        w.extend_from_slice(&t.cur_index.to_le_bytes());
        w.extend_from_slice(&t.count.to_le_bytes());
    }

    put_u32(&mut w, s.pending.len());
    for (&(at, seq), d) in &s.pending {
        w.extend_from_slice(&at.0.to_le_bytes());
        w.extend_from_slice(&seq.to_le_bytes());
        w.extend_from_slice(&d.dst.0.to_le_bytes());
        put_bytes(&mut w, &d.frame);
    }

    put_u32(&mut w, s.held.len());
    for (link, frames) in &s.held {
        w.extend_from_slice(&link.0.0.to_le_bytes());
        w.extend_from_slice(&link.1.0.to_le_bytes());
        put_u32(&mut w, frames.len());
        for hf in frames {
            w.extend_from_slice(&hf.horizon.0.to_le_bytes());
            put_bytes(&mut w, &hf.frame);
        }
    }

    w
}

/// Decode a blob from [`encode`] onto `s`, replacing its mutable state but
/// leaving its [`NodeMap`](crate::NodeMap) intact.
pub(crate) fn decode_into(s: &mut Switch, bytes: &[u8]) -> Result<(), NetError> {
    let mut r = Reader::new(bytes);

    if r.u32()? != MAGIC || r.u16()? != VERSION {
        return Err(NetError::Malformed);
    }
    let l0 = VTime(r.u64()?);
    let next_seq = r.u64()?;

    // Partitions: a <= b, strictly ascending tuples.
    let mut partitions = BTreeSet::new();
    let mut prev_part: Option<(NodeId, NodeId, VTime, VTime)> = None;
    for _ in 0..r.u32()? {
        let a = NodeId(r.u32()?);
        let b = NodeId(r.u32()?);
        let start = VTime(r.u64()?);
        let end = VTime(r.u64()?);
        if a > b {
            return Err(NetError::Malformed);
        }
        let tuple = (a, b, start, end);
        if prev_part.is_some_and(|p| tuple <= p) {
            return Err(NetError::Malformed);
        }
        prev_part = Some(tuple);
        partitions.insert(tuple);
    }

    // Throttles: keyed by directed link, strictly ascending by link.
    let mut throttles = BTreeMap::new();
    let mut prev_link: Option<Link> = None;
    for _ in 0..r.u32()? {
        let link = (NodeId(r.u32()?), NodeId(r.u32()?));
        let max = r.u32()?;
        let per = VTime(r.u64()?);
        let start = VTime(r.u64()?);
        let end = VTime(r.u64()?);
        let cur_index = r.u64()?;
        let count = r.u32()?;
        // `count` is admitted-so-far in the current window and can never exceed
        // `max` via `set_throttle`/`throttle_blocks`; a blob claiming `count > max`
        // is corrupt (it would clog or admit a different number of frames on
        // restore than the recording did).
        if prev_link.is_some_and(|p| link <= p) || count > max {
            return Err(NetError::Malformed);
        }
        prev_link = Some(link);
        throttles.insert(
            link,
            Throttle {
                max,
                per,
                start,
                end,
                cur_index,
                count,
            },
        );
    }

    // Pending: strictly ascending by (at, seq); every seq < next_seq.
    let mut pending = BTreeMap::new();
    let mut prev_key: Option<(VTime, u64)> = None;
    for _ in 0..r.u32()? {
        let at = VTime(r.u64()?);
        let seq = r.u64()?;
        let dst = NodeId(r.u32()?);
        let frame = r.bytes()?.to_vec();
        let key = (at, seq);
        if prev_key.is_some_and(|p| key <= p) || seq >= next_seq {
            return Err(NetError::Malformed);
        }
        prev_key = Some(key);
        pending.insert(key, NetDeliver { dst, frame, at });
    }

    // Held reorder buffer: strictly ascending by link, each link non-empty.
    let mut held: BTreeMap<Link, Vec<HeldFrame>> = BTreeMap::new();
    let mut prev_held: Option<Link> = None;
    for _ in 0..r.u32()? {
        let link = (NodeId(r.u32()?), NodeId(r.u32()?));
        let nframes = r.u32()?;
        if nframes == 0 || prev_held.is_some_and(|p| link <= p) {
            return Err(NetError::Malformed);
        }
        prev_held = Some(link);
        let mut frames = Vec::new();
        for _ in 0..nframes {
            let horizon = VTime(r.u64()?);
            let frame = r.bytes()?.to_vec();
            frames.push(HeldFrame { frame, horizon });
        }
        held.insert(link, frames);
    }

    if !r.at_end() {
        return Err(NetError::Malformed);
    }

    // Commit only after a fully valid parse (leave `self.nodes` untouched).
    s.l0 = l0;
    s.next_seq = next_seq;
    s.partitions = partitions;
    s.throttles = throttles;
    s.pending = pending;
    s.held = held;
    Ok(())
}

/// Append a `u32` length-prefixed byte blob.
fn put_bytes(w: &mut Vec<u8>, b: &[u8]) {
    put_u32(w, b.len());
    w.extend_from_slice(b);
}

/// Append a length as a `u32`, saturating (lengths here are never near
/// `u32::MAX`; saturation keeps the path total rather than panicking).
fn put_u32(w: &mut Vec<u8>, n: usize) {
    w.extend_from_slice(&u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes());
}

/// A forward-only cursor; every read past end-of-buffer is
/// [`NetError::Malformed`]. Never allocates from an untrusted length: byte blobs
/// are sliced (bounds-checked against the *actual* buffer) before copying.
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

    fn take(&mut self, n: usize) -> Result<&'a [u8], NetError> {
        let end = self.pos.checked_add(n).ok_or(NetError::Malformed)?;
        let slice = self.buf.get(self.pos..end).ok_or(NetError::Malformed)?;
        self.pos = end;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16, NetError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, NetError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, NetError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Read a `u32`-length-prefixed byte blob.
    fn bytes(&mut self) -> Result<&'a [u8], NetError> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}
