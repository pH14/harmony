// SPDX-License-Identifier: AGPL-3.0-or-later
//! The versioned, **canonical** journal codec — [`encode`] / [`decode`].
//!
//! Modeled on `control-proto`'s codec discipline. A journal is
//! `magic(4) · format_version(2) · env_blob_version(2)` followed by a canonical
//! payload; every variable-length field is `u32`-length prefixed and every
//! integer little-endian. The encoding is **bit-deterministic**: each value has
//! exactly one byte form (fixed field order, `BTreeMap`s walked in sorted order,
//! no floats, no wall-clock — conventions rule 4), so equal traces encode to
//! equal bytes and `encode(decode(b)) == b` for any journal `decode` accepts.
//!
//! Decoding is **strict and total**: it bounds-checks every length and tag
//! against the *actual* buffer before use, rejects trailing bytes, and — the
//! load-bearing rule of task 65 §1 — fails loudly with [`TraceError::Version`]
//! on an unknown [`TRACE_FORMAT_VERSION`](crate::TRACE_FORMAT_VERSION) rather
//! than reinterpreting old bytes under a new schema. All five [`RunTrace`]
//! fields serialize from day one: `events` is present-and-empty until task 73
//! and `coverage` is `None` under task 58's zero-width geometry — neither is a
//! format bump when it lights up.

use explorer::{
    CoverageView, Environment, GuestEvent, Moment, Record, RunTrace, StopReason, StreamId, VTime,
    Value,
};

use crate::error::TraceError;

/// Journal magic: `b"TRC1"` read little-endian. Pins the on-disk byte order and
/// marks the bytes as a RunTrace journal.
const MAGIC: u32 = u32::from_le_bytes(*b"TRC1");

/// Fixed journal header: magic(4) + format_version(2) + env_blob_version(2).
const HEADER_LEN: usize = 8;

// ---- StopReason variant tags. Stable; the on-disk format depends on them. ----
const SR_DEADLINE: u8 = 1;
const SR_QUIESCENT: u8 = 2;
const SR_CRASH: u8 = 3;
const SR_DECISION: u8 = 4;
const SR_ASSERTION: u8 = 5;
const SR_SNAPSHOT_POINT: u8 = 6;

// ---- Value variant tags. ----
const V_BOOL: u8 = 0;
const V_INT: u8 = 1;
const V_UINT: u8 = 2;
const V_STR: u8 = 3;
const V_BYTES: u8 = 4;

// ---- Option present-flag. ----
const ABSENT: u8 = 0;
const PRESENT: u8 = 1;

// ---- Minimum on-wire size of one collection element, for bounding decode
// preallocation against the *actual* remaining bytes (never a raw count). A
// count claims `n` elements, but each element occupies at least this many bytes,
// so a malformed huge count cannot reserve more than the buffer could hold. ----
/// Smallest `(Moment, GuestEvent)`: `Moment`(8) + empty-kind len(4) + attr count(4).
const MIN_EVENT_WIRE_LEN: usize = 16;
/// Smallest `(Moment, Record)`: `Moment`(8) + `StreamId`(2) + empty-line len(4).
const MIN_RECORD_WIRE_LEN: usize = 14;

// ============================ public entry points ============================

/// Encode a [`RunTrace`] into a versioned, canonical journal.
///
/// There is no journal size cap (unlike a wire frame — an on-disk journal is as
/// large as the run's console), but every variable-length field is `u32`-length
/// prefixed, so a single field larger than `u32::MAX` (> 4 GiB — e.g. one
/// gigantic unterminated console line) cannot be represented. Rather than
/// saturate the prefix and emit a journal that can never decode, encoding
/// **fails loudly** with [`TraceError::Oversize`] (mirroring `control-proto`'s
/// `BadLength`). Validation runs *before* any bytes are written, so a caller
/// that ignores the error is not left with a half-built buffer.
pub fn encode(t: &RunTrace) -> Result<Vec<u8>, TraceError> {
    check_encodable(t)?;
    let mut w = Vec::new();
    w.extend_from_slice(&MAGIC.to_le_bytes());
    w.extend_from_slice(&crate::TRACE_FORMAT_VERSION.to_le_bytes());
    // The env blob version rides in the header (task 65 §1); the bytes ride in
    // the payload, so the `Environment` is reconstructed from the two on decode.
    w.extend_from_slice(&t.env.blob_version.to_le_bytes());

    write_stop_reason(&mut w, &t.terminal);
    put_bytes(&mut w, &t.env.bytes);
    write_opt_coverage(&mut w, &t.coverage);
    write_events(&mut w, &t.events);
    write_records(&mut w, &t.records);
    Ok(w)
}

/// Whether `len` fits the `u32` length prefix the format uses.
fn fits(len: usize) -> bool {
    u32::try_from(len).is_ok()
}

/// Reject a [`RunTrace`] any of whose length-prefixed fields (byte blobs or
/// collection counts) would overflow the `u32` prefix, **before** [`encode`]
/// writes anything. This is the one place the format's size limit is enforced;
/// after it passes, every `put_len`/`put_bytes` below is guaranteed in range.
fn check_encodable(t: &RunTrace) -> Result<(), TraceError> {
    let check = |what: &'static str, len: usize| -> Result<(), TraceError> {
        if fits(len) {
            Ok(())
        } else {
            Err(TraceError::Oversize { what, len })
        }
    };
    check("env.bytes", t.env.bytes.len())?;
    match &t.terminal {
        StopReason::Crash { info, .. } => check("terminal.info", info.len())?,
        StopReason::Decision { ctx, .. } => check("terminal.ctx", ctx.len())?,
        StopReason::Assertion { data, .. } => check("terminal.data", data.len())?,
        _ => {}
    }
    if let Some(cv) = &t.coverage {
        check("coverage.map", cv.map.len())?;
    }
    check("events.count", t.events.len())?;
    for (_, ev) in &t.events {
        check("event.kind", ev.kind.len())?;
        check("event.attrs.count", ev.attrs.len())?;
        for (k, v) in &ev.attrs {
            check("event.attr.key", k.len())?;
            match v {
                Value::Str(s) => check("event.attr.value", s.len())?,
                Value::Bytes(b) => check("event.attr.value", b.len())?,
                _ => {}
            }
        }
    }
    check("records.count", t.records.len())?;
    for (_, r) in &t.records {
        check("record.line", r.line.len())?;
    }
    Ok(())
}

/// Decode one [`RunTrace`] from a complete journal.
///
/// Fails loudly ([`TraceError::Version`]) on an unknown format version; returns
/// [`TraceError::Truncated`]/[`TraceError::Trailing`]/[`TraceError::Magic`] on a
/// malformed or non-canonical journal. Never panics on arbitrary bytes.
pub fn decode(buf: &[u8]) -> Result<RunTrace, TraceError> {
    if buf.len() < HEADER_LEN {
        return Err(TraceError::Truncated);
    }
    // Indexing is in bounds: at least HEADER_LEN bytes present.
    if u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) != MAGIC {
        return Err(TraceError::Magic);
    }
    let version = u16::from_le_bytes([buf[4], buf[5]]);
    if version != crate::TRACE_FORMAT_VERSION {
        return Err(TraceError::Version {
            found: version,
            supported: crate::TRACE_FORMAT_VERSION,
        });
    }
    let env_blob_version = u16::from_le_bytes([buf[6], buf[7]]);

    let mut r = Reader::new(&buf[HEADER_LEN..]);
    let terminal = read_stop_reason(&mut r)?;
    let env = Environment {
        blob_version: env_blob_version,
        bytes: r.bytes()?.to_vec(),
    };
    let coverage = read_opt_coverage(&mut r)?;
    let events = read_events(&mut r)?;
    let records = read_records(&mut r)?;
    r.finish()?;
    Ok(RunTrace {
        terminal,
        env,
        coverage,
        events,
        records,
    })
}

/// The **canonical env bytes** hashed for a [`TraceId`](crate::TraceId) and
/// stored in the store's env sidecar: `blob_version(2) · bytes`. Independent of
/// the journal envelope, so an env-only trace addresses identically to the same
/// run's full journal.
pub fn encode_env(env: &Environment) -> Vec<u8> {
    let mut w = Vec::with_capacity(2 + env.bytes.len());
    w.extend_from_slice(&env.blob_version.to_le_bytes());
    w.extend_from_slice(&env.bytes);
    w
}

/// Decode an env sidecar written by [`encode_env`].
pub fn decode_env(buf: &[u8]) -> Result<Environment, TraceError> {
    if buf.len() < 2 {
        return Err(TraceError::Truncated);
    }
    Ok(Environment {
        blob_version: u16::from_le_bytes([buf[0], buf[1]]),
        bytes: buf[2..].to_vec(),
    })
}

// ============================ component encoders =============================

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
            put_bytes(w, info);
        }
        StopReason::Decision { vtime, id, ctx } => {
            w.push(SR_DECISION);
            put_u64(w, vtime.0);
            put_u64(w, *id);
            put_bytes(w, ctx);
        }
        StopReason::Assertion { vtime, id, data } => {
            w.push(SR_ASSERTION);
            put_u64(w, vtime.0);
            put_u32(w, *id);
            put_bytes(w, data);
        }
        StopReason::SnapshotPoint { vtime } => {
            w.push(SR_SNAPSHOT_POINT);
            put_u64(w, vtime.0);
        }
    }
}

fn read_stop_reason(r: &mut Reader) -> Result<StopReason, TraceError> {
    Ok(match r.u8()? {
        SR_DEADLINE => StopReason::Deadline {
            vtime: VTime(r.u64()?),
        },
        SR_QUIESCENT => StopReason::Quiescent {
            vtime: VTime(r.u64()?),
        },
        SR_CRASH => StopReason::Crash {
            vtime: VTime(r.u64()?),
            info: r.bytes()?.to_vec(),
        },
        SR_DECISION => StopReason::Decision {
            vtime: VTime(r.u64()?),
            id: r.u64()?,
            ctx: r.bytes()?.to_vec(),
        },
        SR_ASSERTION => StopReason::Assertion {
            vtime: VTime(r.u64()?),
            id: r.u32()?,
            data: r.bytes()?.to_vec(),
        },
        SR_SNAPSHOT_POINT => StopReason::SnapshotPoint {
            vtime: VTime(r.u64()?),
        },
        _ => return Err(TraceError::Truncated),
    })
}

fn write_opt_coverage(w: &mut Vec<u8>, coverage: &Option<CoverageView>) {
    match coverage {
        None => w.push(ABSENT),
        Some(cv) => {
            w.push(PRESENT);
            put_bytes(w, &cv.map);
        }
    }
}

fn read_opt_coverage(r: &mut Reader) -> Result<Option<CoverageView>, TraceError> {
    Ok(match r.u8()? {
        ABSENT => None,
        PRESENT => Some(CoverageView {
            map: r.bytes()?.to_vec(),
        }),
        _ => return Err(TraceError::Truncated),
    })
}

fn write_events(w: &mut Vec<u8>, events: &[(Moment, GuestEvent)]) {
    put_len(w, events.len());
    for (at, ev) in events {
        put_u64(w, at.0);
        put_str(w, &ev.kind);
        put_len(w, ev.attrs.len());
        // `attrs` is a `BTreeMap`, so this iteration is already the canonical
        // sorted order — no iteration-order surface reaches the bytes (rule 4).
        for (k, v) in &ev.attrs {
            put_str(w, k);
            write_value(w, v);
        }
    }
}

fn read_events(r: &mut Reader) -> Result<Vec<(Moment, GuestEvent)>, TraceError> {
    let n = r.len_prefix()?;
    // Bound the preallocation by how many elements could *possibly* remain
    // (bytes / min element size), never the raw count — a malformed 100 MB input
    // claiming billions of elements must not reserve gigabytes before validation.
    let mut out = Vec::with_capacity(n.min(r.remaining() / MIN_EVENT_WIRE_LEN));
    for _ in 0..n {
        let at = Moment(r.u64()?);
        let kind = r.string()?;
        let attr_n = r.len_prefix()?;
        let mut attrs = std::collections::BTreeMap::new();
        // `attrs` is a `BTreeMap`, so its canonical byte form has keys in
        // strictly-increasing order (the encoder walks that order). Enforce it on
        // decode: a journal with unsorted or duplicate keys would otherwise
        // decode successfully but re-encode to *different* bytes (BTreeMap sorts
        // and last-wins-dedups), falsifying `encode(decode(b)) == b`. Reject it.
        let mut prev: Option<String> = None;
        for _ in 0..attr_n {
            let k = r.string()?;
            if let Some(p) = &prev
                && k <= *p
            {
                return Err(TraceError::NonCanonical);
            }
            let v = read_value(r)?;
            prev = Some(k.clone());
            attrs.insert(k, v);
        }
        out.push((at, GuestEvent { kind, attrs }));
    }
    Ok(out)
}

fn write_records(w: &mut Vec<u8>, records: &[(Moment, Record)]) {
    put_len(w, records.len());
    for (at, rec) in records {
        put_u64(w, at.0);
        put_u16(w, rec.stream.0);
        put_bytes(w, &rec.line);
    }
}

fn read_records(r: &mut Reader) -> Result<Vec<(Moment, Record)>, TraceError> {
    let n = r.len_prefix()?;
    // Bound the preallocation by bytes / min element size, not the raw count.
    let mut out = Vec::with_capacity(n.min(r.remaining() / MIN_RECORD_WIRE_LEN));
    for _ in 0..n {
        let at = Moment(r.u64()?);
        let stream = StreamId(r.u16()?);
        let line = r.bytes()?.to_vec();
        out.push((at, Record { stream, line }));
    }
    Ok(out)
}

fn write_value(w: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Bool(b) => {
            w.push(V_BOOL);
            w.push(u8::from(*b));
        }
        Value::Int(i) => {
            w.push(V_INT);
            w.extend_from_slice(&i.to_le_bytes());
        }
        Value::UInt(u) => {
            w.push(V_UINT);
            put_u64(w, *u);
        }
        Value::Str(s) => {
            w.push(V_STR);
            put_str(w, s);
        }
        Value::Bytes(b) => {
            w.push(V_BYTES);
            put_bytes(w, b);
        }
    }
}

fn read_value(r: &mut Reader) -> Result<Value, TraceError> {
    Ok(match r.u8()? {
        V_BOOL => Value::Bool(match r.u8()? {
            0 => false,
            1 => true,
            _ => return Err(TraceError::Truncated),
        }),
        V_INT => Value::Int(i64::from_le_bytes(r.array8()?)),
        V_UINT => Value::UInt(r.u64()?),
        V_STR => Value::Str(r.string()?),
        V_BYTES => Value::Bytes(r.bytes()?.to_vec()),
        _ => return Err(TraceError::Truncated),
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

/// A collection/blob length as `u32`. [`encode`]'s [`check_encodable`] pre-pass
/// guarantees every length reaching here fits `u32`, so the fallback is now
/// genuinely unreachable (kept only so this stays panic-free even if a future
/// caller bypasses the check).
fn put_len(w: &mut Vec<u8>, n: usize) {
    put_u32(w, u32::try_from(n).unwrap_or(u32::MAX));
}

/// A `u32`-length-prefixed byte blob (see [`put_len`] on the length domain).
fn put_bytes(w: &mut Vec<u8>, b: &[u8]) {
    put_len(w, b.len());
    w.extend_from_slice(b);
}

fn put_str(w: &mut Vec<u8>, s: &str) {
    put_bytes(w, s.as_bytes());
}

/// A forward-only cursor over the journal payload. Every read past the end is
/// [`TraceError::Truncated`]; byte blobs are sliced (bounds-checked against the
/// actual buffer) before any copy, so an untrusted length can never force an
/// out-of-bounds read or an unbounded allocation (control-proto discipline).
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    /// Bytes not yet consumed — an allocation upper bound for length-prefixed
    /// collections (never reserve more than could possibly remain).
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Require the whole payload was consumed — rejects a non-canonical journal
    /// with trailing bytes, keeping `encode(decode(b)) == b`.
    fn finish(&self) -> Result<(), TraceError> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err(TraceError::Trailing)
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], TraceError> {
        let end = self.pos.checked_add(n).ok_or(TraceError::Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(TraceError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, TraceError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, TraceError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, TraceError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, TraceError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn array8(&mut self) -> Result<[u8; 8], TraceError> {
        let b = self.take(8)?;
        let mut out = [0u8; 8];
        out.copy_from_slice(b);
        Ok(out)
    }

    /// A collection length prefix, returned as `usize`.
    fn len_prefix(&mut self) -> Result<usize, TraceError> {
        Ok(self.u32()? as usize)
    }

    /// A `u32`-length-prefixed byte blob, borrowed from the payload.
    fn bytes(&mut self) -> Result<&'a [u8], TraceError> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    /// A `u32`-length-prefixed UTF-8 string. Non-UTF-8 bytes are a loud
    /// [`TraceError::Utf8`], never a lossy substitution (string fields are
    /// always valid UTF-8 when encoded; a garbage journal is rejected).
    fn string(&mut self) -> Result<String, TraceError> {
        let b = self.bytes()?;
        String::from_utf8(b.to_vec()).map_err(|_| TraceError::Utf8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RunTrace {
        RunTrace {
            terminal: StopReason::Quiescent {
                vtime: VTime(4_200),
            },
            env: Environment {
                blob_version: 3,
                bytes: vec![7, 8, 9],
            },
            coverage: None,
            events: vec![],
            records: vec![(
                Moment(4_200),
                Record {
                    stream: StreamId(0),
                    line: b"ready\n".to_vec(),
                },
            )],
        }
    }

    #[test]
    fn journal_round_trips_and_carries_the_env_blob_version_in_the_header() {
        let t = sample();
        let bytes = encode(&t).expect("small trace encodes");
        // Header: magic(4) + format_version(2) + env_blob_version(2).
        assert_eq!(&bytes[0..4], b"TRC1");
        assert_eq!(
            u16::from_le_bytes([bytes[4], bytes[5]]),
            crate::TRACE_FORMAT_VERSION
        );
        assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), t.env.blob_version);
        assert_eq!(decode(&bytes).unwrap(), t);
    }

    #[test]
    fn env_sidecar_round_trips() {
        let env = Environment {
            blob_version: 42,
            bytes: vec![0, 1, 2, 3],
        };
        assert_eq!(decode_env(&encode_env(&env)).unwrap(), env);
        // The content address is a pure function of those canonical bytes.
        assert_eq!(
            crate::TraceId::of(&env),
            crate::TraceId(*blake3::hash(&encode_env(&env)).as_bytes())
        );
    }

    /// Hand-assemble a journal whose single event carries two attr keys, so a
    /// test can feed canonical and non-canonical key orders. Uses the crate's own
    /// private byte helpers, so it stays in lockstep with the real encoder.
    fn journal_with_two_attr_keys(k1: &str, k2: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&MAGIC.to_le_bytes());
        buf.extend_from_slice(&crate::TRACE_FORMAT_VERSION.to_le_bytes());
        buf.extend_from_slice(&3u16.to_le_bytes()); // env blob version
        buf.push(SR_QUIESCENT);
        put_u64(&mut buf, 1); // terminal
        put_bytes(&mut buf, &[]); // env bytes
        buf.push(ABSENT); // coverage
        put_len(&mut buf, 1); // one event
        put_u64(&mut buf, 10); // Moment
        put_str(&mut buf, "k"); // kind
        put_len(&mut buf, 2); // two attrs
        put_str(&mut buf, k1);
        buf.push(V_BOOL);
        buf.push(1);
        put_str(&mut buf, k2);
        buf.push(V_BOOL);
        buf.push(1);
        put_len(&mut buf, 0); // no records
        buf
    }

    #[test]
    fn read_events_rejects_noncanonical_attr_keys() {
        // Out-of-order keys ("b" then "a") are rejected loudly.
        assert!(matches!(
            decode(&journal_with_two_attr_keys("b", "a")),
            Err(TraceError::NonCanonical)
        ));
        // Duplicate keys ("a" then "a") are rejected too (BTreeMap would dedup
        // last-wins, breaking `encode(decode(b)) == b`).
        assert!(matches!(
            decode(&journal_with_two_attr_keys("a", "a")),
            Err(TraceError::NonCanonical)
        ));
        // The canonical order ("a" then "b") decodes, and re-encodes identically.
        let canonical = journal_with_two_attr_keys("a", "b");
        let t = decode(&canonical).expect("canonical journal decodes");
        assert_eq!(
            encode(&t).expect("encodes"),
            canonical,
            "accepted bytes are canonical"
        );
    }

    #[test]
    fn size_gate_boundary_is_the_u32_prefix() {
        // `encode` rejects any field whose length overflows the `u32` prefix
        // (`check_encodable` → `fits`). A real > 4 GiB blob cannot be allocated
        // in a test, so exercise the boundary directly; on our 64-bit targets
        // `usize` exceeds `u32::MAX`.
        assert!(fits(u32::MAX as usize), "u32::MAX fits the prefix");
        #[cfg(target_pointer_width = "64")]
        assert!(
            !fits(u32::MAX as usize + 1),
            "one past u32::MAX must not fit — encode returns TraceError::Oversize"
        );
    }
}
