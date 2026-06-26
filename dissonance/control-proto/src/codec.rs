// SPDX-License-Identifier: AGPL-3.0-or-later
//! The versioned, length-delimited wire codec.
//!
//! A frame is `magic(4) · version(2) · seq(4) · len(4) · body[len]`, all integers
//! little-endian. The body is a tagged encoding of a [`Request`] or a
//! `Result<Reply, ControlError>`; every variable-length field is `u32`-length
//! prefixed.
//!
//! Encoding is **bit-deterministic and canonical**: each value has exactly one
//! byte form (fixed field order, no maps, no padding), and the declared `len`
//! always equals the body's natural size — so `encode(decode(x)) == x` for any
//! frame `decode` accepts. Decoding is **strict and total**: it bounds-checks
//! every length and tag against the *actual* buffer before use, rejects an
//! over-cap `len` from the header alone (before buffering the body), rejects a
//! body that does not consume exactly `len` bytes, and never panics or reads out
//! of bounds on arbitrary input (conventions rule 4). A frame that is merely
//! not-yet-fully-received yields `Ok(None)` ("need more"), distinct from a loud
//! [`ProtocolError`].

use crate::error::ProtocolError;
use crate::types::{
    Answer, CapFlags, Caps, CoverageGeometry, CrashInfo, CrashKind, DecisionId, Environment,
    EventRef, HashScope, Reply, Request, SnapId, StopConditions, StopMask, StopReason, VTime,
};
use crate::{MAX_FRAME_LEN, PROTO_VERSION};

/// Frame magic: `b"CTL1"` read little-endian. Pins the on-wire byte order.
const MAGIC: u32 = u32::from_le_bytes([b'C', b'T', b'L', b'1']);
/// The fixed frame header: magic(4) + version(2) + seq(4) + len(4).
const HEADER_LEN: usize = 14;

// ---- Request body discriminants. Stable; the wire format depends on them. ----
const REQ_HELLO: u8 = 1;
const REQ_SNAPSHOT: u8 = 2;
const REQ_DROP: u8 = 3;
const REQ_BRANCH: u8 = 4;
const REQ_REPLAY: u8 = 5;
const REQ_RUN: u8 = 6;
const REQ_HASH: u8 = 7;

// ---- Reply-body top-level result discriminants. ----
const RESULT_OK: u8 = 0;
const RESULT_ERR: u8 = 1;

// ---- Reply variant discriminants. ----
const REPLY_HELLO: u8 = 1;
const REPLY_SNAPID: u8 = 2;
const REPLY_UNIT: u8 = 3;
const REPLY_STOP: u8 = 4;
const REPLY_HASH: u8 = 5;

// ---- StopReason variant discriminants. ----
const SR_DEADLINE: u8 = 1;
const SR_QUIESCENT: u8 = 2;
const SR_CRASH: u8 = 3;
const SR_DECISION: u8 = 4;
const SR_SNAPSHOT_POINT: u8 = 5;
const SR_ASSERTION: u8 = 6;

// ---- CrashKind discriminants. ----
const CK_PANIC: u8 = 0;
const CK_TRIPLE_FAULT: u8 = 1;
const CK_SHUTDOWN: u8 = 2;

// ---- HashScope discriminants. ----
const HS_WHOLE: u8 = 0;
const HS_DISK: u8 = 1;
const HS_REGION: u8 = 2;

// ---- ControlError discriminants. ----
const CE_UNKNOWN_SNAPSHOT: u8 = 1;
const CE_RESTORE_FAILED: u8 = 2;
const CE_SNAPSHOT_WHILE_ARMED: u8 = 3;
const CE_NOT_QUIESCENT: u8 = 4;
const CE_BAD_ENV_VERSION: u8 = 5;
const CE_MALFORMED_ENVIRONMENT: u8 = 6;
const CE_RESOLVE_WITHOUT_DECISION: u8 = 7;
const CE_MALFORMED_ANSWER: u8 = 8;
const CE_PROTOCOL: u8 = 9;

// ---- ProtocolError discriminants (carried inside CE_PROTOCOL). ----
const PE_SHORT_FRAME: u8 = 0;
const PE_BAD_MAGIC: u8 = 1;
const PE_BAD_VERSION: u8 = 2;
const PE_BAD_LENGTH: u8 = 3;

// ---- Option present-flag. ----
const ABSENT: u8 = 0;
const PRESENT: u8 = 1;

// ========================= public codec entry points =========================

/// Encode a [`Request`] into a length-delimited frame appended to `buf`.
///
/// Fallible only on size: a body that would exceed [`MAX_FRAME_LEN`] returns
/// [`ProtocolError::BadLength`] and leaves `buf` unchanged — never a panic, a
/// truncation, or a frame the decoder's cap would reject.
pub fn encode_request(seq: u32, req: &Request, buf: &mut Vec<u8>) -> Result<(), ProtocolError> {
    let mut body = Vec::new();
    write_request(&mut body, req);
    finish_frame(seq, &body, buf)
}

/// Encode a `Result<Reply, ControlError>` into a length-delimited frame appended
/// to `buf`. Same size contract as [`encode_request`].
pub fn encode_reply(
    seq: u32,
    reply: &Result<Reply, crate::error::ControlError>,
    buf: &mut Vec<u8>,
) -> Result<(), ProtocolError> {
    let mut body = Vec::new();
    write_reply_result(&mut body, reply);
    finish_frame(seq, &body, buf)
}

/// Decode exactly one [`Request`] frame from the front of `buf`, returning
/// `(seq, request, bytes_consumed)`.
///
/// A partial frame yields `Ok(None)` ("need more"). Never panics on any input.
pub fn decode_request(buf: &[u8]) -> Result<Option<(u32, Request, usize)>, ProtocolError> {
    let Some((seq, body, consumed)) = decode_frame(buf)? else {
        return Ok(None);
    };
    let mut r = Reader::new(body);
    let req = read_request(&mut r)?;
    r.finish()?;
    Ok(Some((seq, req, consumed)))
}

/// Decode exactly one reply frame from the front of `buf`, returning
/// `(seq, Result<Reply, ControlError>, bytes_consumed)`.
///
/// A partial frame yields `Ok(None)` ("need more"). Never panics on any input.
// The nested-`Result` return type is the spec's pinned public signature
// (conventions rule 3), not a candidate for factoring.
#[allow(clippy::type_complexity)]
pub fn decode_reply(
    buf: &[u8],
) -> Result<Option<(u32, Result<Reply, crate::error::ControlError>, usize)>, ProtocolError> {
    let Some((seq, body, consumed)) = decode_frame(buf)? else {
        return Ok(None);
    };
    let mut r = Reader::new(body);
    let reply = read_reply_result(&mut r)?;
    r.finish()?;
    Ok(Some((seq, reply, consumed)))
}

// ============================== framing layer ===============================

/// Append a complete frame (header + body) to `buf`, or fail with
/// [`ProtocolError::BadLength`] leaving `buf` untouched.
fn finish_frame(seq: u32, body: &[u8], buf: &mut Vec<u8>) -> Result<(), ProtocolError> {
    if body.len() > MAX_FRAME_LEN {
        return Err(ProtocolError::BadLength);
    }
    buf.extend_from_slice(&MAGIC.to_le_bytes());
    buf.extend_from_slice(&PROTO_VERSION.to_le_bytes());
    buf.extend_from_slice(&seq.to_le_bytes());
    // body.len() <= MAX_FRAME_LEN (16 MiB) always fits in u32.
    buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
    buf.extend_from_slice(body);
    Ok(())
}

/// A framed body sliced from the input: `(seq, body, bytes_consumed)`.
type Framed<'a> = (u32, &'a [u8], usize);

/// Parse the frame header and slice out the body, validating magic/version and
/// rejecting an over-cap length **from the header alone** — before any body is
/// buffered. Returns `Ok(None)` when the header or body is not yet fully present.
fn decode_frame(buf: &[u8]) -> Result<Option<Framed<'_>>, ProtocolError> {
    if buf.len() < HEADER_LEN {
        return Ok(None);
    }
    // Indexing is in bounds: we have at least HEADER_LEN bytes.
    if u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) != MAGIC {
        return Err(ProtocolError::BadMagic);
    }
    if u16::from_le_bytes([buf[4], buf[5]]) != PROTO_VERSION {
        return Err(ProtocolError::BadVersion);
    }
    let seq = u32::from_le_bytes([buf[6], buf[7], buf[8], buf[9]]);
    let len = u32::from_le_bytes([buf[10], buf[11], buf[12], buf[13]]) as usize;
    if len > MAX_FRAME_LEN {
        // Rejected before reading/allocating the body: an untrusted length can
        // never force unbounded buffering.
        return Err(ProtocolError::BadLength);
    }
    // No overflow: len <= MAX_FRAME_LEN (16 MiB) and HEADER_LEN == 14.
    let end = HEADER_LEN + len;
    if buf.len() < end {
        return Ok(None);
    }
    Ok(Some((seq, &buf[HEADER_LEN..end], end)))
}

// ============================== request body ================================

fn write_request(w: &mut Vec<u8>, req: &Request) {
    match req {
        Request::Hello(caps) => {
            w.push(REQ_HELLO);
            write_caps(w, caps);
        }
        Request::Snapshot => w.push(REQ_SNAPSHOT),
        Request::Drop(SnapId(id)) => {
            w.push(REQ_DROP);
            put_u64(w, *id);
        }
        Request::Branch { snap, env } => {
            w.push(REQ_BRANCH);
            put_u64(w, snap.0);
            write_env(w, env);
        }
        Request::Replay(SnapId(id)) => {
            w.push(REQ_REPLAY);
            put_u64(w, *id);
        }
        Request::Run { until, resolve } => {
            w.push(REQ_RUN);
            write_stop_conditions(w, until);
            write_opt_answer(w, resolve);
        }
        Request::Hash { scope } => {
            w.push(REQ_HASH);
            write_hash_scope(w, scope);
        }
    }
}

fn read_request(r: &mut Reader) -> Result<Request, ProtocolError> {
    Ok(match r.u8()? {
        REQ_HELLO => Request::Hello(read_caps(r)?),
        REQ_SNAPSHOT => Request::Snapshot,
        REQ_DROP => Request::Drop(SnapId(r.u64()?)),
        REQ_BRANCH => Request::Branch {
            snap: SnapId(r.u64()?),
            env: read_env(r)?,
        },
        REQ_REPLAY => Request::Replay(SnapId(r.u64()?)),
        REQ_RUN => Request::Run {
            until: read_stop_conditions(r)?,
            resolve: read_opt_answer(r)?,
        },
        REQ_HASH => Request::Hash {
            scope: read_hash_scope(r)?,
        },
        _ => return Err(ProtocolError::ShortFrame),
    })
}

// =============================== reply body =================================

fn write_reply_result(w: &mut Vec<u8>, reply: &Result<Reply, crate::error::ControlError>) {
    match reply {
        Ok(reply) => {
            w.push(RESULT_OK);
            write_reply(w, reply);
        }
        Err(err) => {
            w.push(RESULT_ERR);
            write_control_error(w, err);
        }
    }
}

fn read_reply_result(
    r: &mut Reader,
) -> Result<Result<Reply, crate::error::ControlError>, ProtocolError> {
    Ok(match r.u8()? {
        RESULT_OK => Ok(read_reply(r)?),
        RESULT_ERR => Err(read_control_error(r)?),
        _ => return Err(ProtocolError::ShortFrame),
    })
}

fn write_reply(w: &mut Vec<u8>, reply: &Reply) {
    match reply {
        Reply::Hello(caps) => {
            w.push(REPLY_HELLO);
            write_caps(w, caps);
        }
        Reply::SnapId(SnapId(id)) => {
            w.push(REPLY_SNAPID);
            put_u64(w, *id);
        }
        Reply::Unit => w.push(REPLY_UNIT),
        Reply::Stop(reason) => {
            w.push(REPLY_STOP);
            write_stop_reason(w, reason);
        }
        Reply::Hash(digest) => {
            w.push(REPLY_HASH);
            w.extend_from_slice(digest);
        }
    }
}

fn read_reply(r: &mut Reader) -> Result<Reply, ProtocolError> {
    Ok(match r.u8()? {
        REPLY_HELLO => Reply::Hello(read_caps(r)?),
        REPLY_SNAPID => Reply::SnapId(SnapId(r.u64()?)),
        REPLY_UNIT => Reply::Unit,
        REPLY_STOP => Reply::Stop(read_stop_reason(r)?),
        REPLY_HASH => Reply::Hash(read_array32(r)?),
        _ => return Err(ProtocolError::ShortFrame),
    })
}

// ============================ component encoders =============================

fn write_caps(w: &mut Vec<u8>, c: &Caps) {
    put_u16(w, c.protocol_version);
    put_u16(w, c.env_version_min);
    put_u16(w, c.env_version_max);
    put_u32(w, c.coverage.map_bytes);
    w.push(c.coverage.producer);
    put_u32(w, c.flags.0);
}

fn read_caps(r: &mut Reader) -> Result<Caps, ProtocolError> {
    Ok(Caps {
        protocol_version: r.u16()?,
        env_version_min: r.u16()?,
        env_version_max: r.u16()?,
        coverage: CoverageGeometry {
            map_bytes: r.u32()?,
            producer: r.u8()?,
        },
        flags: CapFlags(r.u32()?),
    })
}

fn write_env(w: &mut Vec<u8>, env: &Environment) {
    put_u16(w, env.blob_version);
    put_bytes(w, &env.bytes);
}

/// `blob_version` is carried verbatim and never validated here — an off-version
/// blob still decodes, so the backend can answer `BadEnvVersion` (gate 4).
fn read_env(r: &mut Reader) -> Result<Environment, ProtocolError> {
    Ok(Environment {
        blob_version: r.u16()?,
        bytes: r.bytes()?.to_vec(),
    })
}

fn write_stop_conditions(w: &mut Vec<u8>, sc: &StopConditions) {
    write_opt_vtime(w, &sc.deadline);
    put_u32(w, sc.on.0);
}

fn read_stop_conditions(r: &mut Reader) -> Result<StopConditions, ProtocolError> {
    Ok(StopConditions {
        deadline: read_opt_vtime(r)?,
        on: StopMask(r.u32()?),
    })
}

fn write_hash_scope(w: &mut Vec<u8>, scope: &HashScope) {
    match scope {
        HashScope::Whole => w.push(HS_WHOLE),
        HashScope::Disk => w.push(HS_DISK),
        HashScope::Region { base, len } => {
            w.push(HS_REGION);
            put_u64(w, *base);
            put_u64(w, *len);
        }
    }
}

fn read_hash_scope(r: &mut Reader) -> Result<HashScope, ProtocolError> {
    Ok(match r.u8()? {
        HS_WHOLE => HashScope::Whole,
        HS_DISK => HashScope::Disk,
        HS_REGION => HashScope::Region {
            base: r.u64()?,
            len: r.u64()?,
        },
        _ => return Err(ProtocolError::ShortFrame),
    })
}

fn write_stop_reason(w: &mut Vec<u8>, reason: &StopReason) {
    match reason {
        StopReason::Deadline { vtime } => {
            w.push(SR_DEADLINE);
            put_u64(w, vtime.0);
        }
        StopReason::Quiescent { vtime } => {
            w.push(SR_QUIESCENT);
            put_u64(w, vtime.0);
        }
        StopReason::Crash { vtime, info } => {
            w.push(SR_CRASH);
            put_u64(w, vtime.0);
            write_crash_info(w, info);
        }
        StopReason::Decision { vtime, id, ctx } => {
            w.push(SR_DECISION);
            put_u64(w, vtime.0);
            put_u64(w, id.0);
            put_bytes(w, ctx);
        }
        StopReason::SnapshotPoint { vtime } => {
            w.push(SR_SNAPSHOT_POINT);
            put_u64(w, vtime.0);
        }
        StopReason::Assertion { vtime, ev } => {
            w.push(SR_ASSERTION);
            put_u64(w, vtime.0);
            write_event_ref(w, ev);
        }
    }
}

fn read_stop_reason(r: &mut Reader) -> Result<StopReason, ProtocolError> {
    Ok(match r.u8()? {
        SR_DEADLINE => StopReason::Deadline {
            vtime: VTime(r.u64()?),
        },
        SR_QUIESCENT => StopReason::Quiescent {
            vtime: VTime(r.u64()?),
        },
        SR_CRASH => StopReason::Crash {
            vtime: VTime(r.u64()?),
            info: read_crash_info(r)?,
        },
        SR_DECISION => StopReason::Decision {
            vtime: VTime(r.u64()?),
            id: DecisionId(r.u64()?),
            ctx: r.bytes()?.to_vec(),
        },
        SR_SNAPSHOT_POINT => StopReason::SnapshotPoint {
            vtime: VTime(r.u64()?),
        },
        SR_ASSERTION => StopReason::Assertion {
            vtime: VTime(r.u64()?),
            ev: read_event_ref(r)?,
        },
        _ => return Err(ProtocolError::ShortFrame),
    })
}

fn write_crash_info(w: &mut Vec<u8>, info: &CrashInfo) {
    w.push(match info.kind {
        CrashKind::Panic => CK_PANIC,
        CrashKind::TripleFault => CK_TRIPLE_FAULT,
        CrashKind::Shutdown => CK_SHUTDOWN,
    });
    put_bytes(w, &info.detail);
}

fn read_crash_info(r: &mut Reader) -> Result<CrashInfo, ProtocolError> {
    let kind = match r.u8()? {
        CK_PANIC => CrashKind::Panic,
        CK_TRIPLE_FAULT => CrashKind::TripleFault,
        CK_SHUTDOWN => CrashKind::Shutdown,
        _ => return Err(ProtocolError::ShortFrame),
    };
    Ok(CrashInfo {
        kind,
        detail: r.bytes()?.to_vec(),
    })
}

fn write_event_ref(w: &mut Vec<u8>, ev: &EventRef) {
    put_u32(w, ev.id);
    put_bytes(w, &ev.data);
}

fn read_event_ref(r: &mut Reader) -> Result<EventRef, ProtocolError> {
    Ok(EventRef {
        id: r.u32()?,
        data: r.bytes()?.to_vec(),
    })
}

fn write_control_error(w: &mut Vec<u8>, err: &crate::error::ControlError) {
    use crate::error::ControlError as Ce;
    match err {
        Ce::UnknownSnapshot(SnapId(id)) => {
            w.push(CE_UNKNOWN_SNAPSHOT);
            put_u64(w, *id);
        }
        Ce::RestoreFailed => w.push(CE_RESTORE_FAILED),
        Ce::SnapshotWhileArmed => w.push(CE_SNAPSHOT_WHILE_ARMED),
        Ce::NotQuiescent => w.push(CE_NOT_QUIESCENT),
        Ce::BadEnvVersion(v) => {
            w.push(CE_BAD_ENV_VERSION);
            put_u16(w, *v);
        }
        Ce::MalformedEnvironment => w.push(CE_MALFORMED_ENVIRONMENT),
        Ce::ResolveWithoutDecision => w.push(CE_RESOLVE_WITHOUT_DECISION),
        Ce::MalformedAnswer => w.push(CE_MALFORMED_ANSWER),
        Ce::Protocol(pe) => {
            w.push(CE_PROTOCOL);
            w.push(match pe {
                ProtocolError::ShortFrame => PE_SHORT_FRAME,
                ProtocolError::BadMagic => PE_BAD_MAGIC,
                ProtocolError::BadVersion => PE_BAD_VERSION,
                ProtocolError::BadLength => PE_BAD_LENGTH,
            });
        }
    }
}

fn read_control_error(r: &mut Reader) -> Result<crate::error::ControlError, ProtocolError> {
    use crate::error::ControlError as Ce;
    Ok(match r.u8()? {
        CE_UNKNOWN_SNAPSHOT => Ce::UnknownSnapshot(SnapId(r.u64()?)),
        CE_RESTORE_FAILED => Ce::RestoreFailed,
        CE_SNAPSHOT_WHILE_ARMED => Ce::SnapshotWhileArmed,
        CE_NOT_QUIESCENT => Ce::NotQuiescent,
        CE_BAD_ENV_VERSION => Ce::BadEnvVersion(r.u16()?),
        CE_MALFORMED_ENVIRONMENT => Ce::MalformedEnvironment,
        CE_RESOLVE_WITHOUT_DECISION => Ce::ResolveWithoutDecision,
        CE_MALFORMED_ANSWER => Ce::MalformedAnswer,
        CE_PROTOCOL => Ce::Protocol(match r.u8()? {
            PE_SHORT_FRAME => ProtocolError::ShortFrame,
            PE_BAD_MAGIC => ProtocolError::BadMagic,
            PE_BAD_VERSION => ProtocolError::BadVersion,
            PE_BAD_LENGTH => ProtocolError::BadLength,
            _ => return Err(ProtocolError::ShortFrame),
        }),
        _ => return Err(ProtocolError::ShortFrame),
    })
}

// =============================== option helpers =============================

fn write_opt_vtime(w: &mut Vec<u8>, v: &Option<VTime>) {
    match v {
        Some(VTime(t)) => {
            w.push(PRESENT);
            put_u64(w, *t);
        }
        None => w.push(ABSENT),
    }
}

fn read_opt_vtime(r: &mut Reader) -> Result<Option<VTime>, ProtocolError> {
    Ok(match r.u8()? {
        ABSENT => None,
        PRESENT => Some(VTime(r.u64()?)),
        _ => return Err(ProtocolError::ShortFrame),
    })
}

fn write_opt_answer(w: &mut Vec<u8>, a: &Option<Answer>) {
    match a {
        Some(Answer(bytes)) => {
            w.push(PRESENT);
            put_bytes(w, bytes);
        }
        None => w.push(ABSENT),
    }
}

fn read_opt_answer(r: &mut Reader) -> Result<Option<Answer>, ProtocolError> {
    Ok(match r.u8()? {
        ABSENT => None,
        PRESENT => Some(Answer(r.bytes()?.to_vec())),
        _ => return Err(ProtocolError::ShortFrame),
    })
}

// =============================== byte helpers ===============================

fn put_u16(w: &mut Vec<u8>, v: u16) {
    w.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(w: &mut Vec<u8>, v: u32) {
    w.extend_from_slice(&v.to_le_bytes());
}

fn put_u64(w: &mut Vec<u8>, v: u64) {
    w.extend_from_slice(&v.to_le_bytes());
}

/// Append a `u32`-length-prefixed byte blob. The length saturates at `u32::MAX`,
/// which is unreachable for an emitted frame: the whole body is capped at
/// [`MAX_FRAME_LEN`] (16 MiB) by [`finish_frame`], so any sub-blob is far smaller.
fn put_bytes(w: &mut Vec<u8>, b: &[u8]) {
    put_u32(w, u32::try_from(b.len()).unwrap_or(u32::MAX));
    w.extend_from_slice(b);
}

/// A forward-only cursor over a frame body. Every read past the end is
/// [`ProtocolError::ShortFrame`]; byte blobs are sliced (bounds-checked against
/// the actual body) before any copy, so an untrusted length can never force an
/// out-of-bounds read or an unbounded allocation.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Require that the whole body was consumed — rejects trailing bytes inside
    /// the declared frame length, which keeps the encoding canonical.
    fn finish(&self) -> Result<(), ProtocolError> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err(ProtocolError::ShortFrame)
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        let end = self.pos.checked_add(n).ok_or(ProtocolError::ShortFrame)?;
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or(ProtocolError::ShortFrame)?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ProtocolError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, ProtocolError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, ProtocolError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Read a `u32`-length-prefixed byte blob, borrowed from the body.
    fn bytes(&mut self) -> Result<&'a [u8], ProtocolError> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    /// Read a fixed 32-byte array (the hash digest).
    fn array32(&mut self) -> Result<[u8; 32], ProtocolError> {
        let b = self.take(32)?;
        let mut out = [0u8; 32];
        out.copy_from_slice(b);
        Ok(out)
    }
}

fn read_array32(r: &mut Reader) -> Result<[u8; 32], ProtocolError> {
    r.array32()
}
