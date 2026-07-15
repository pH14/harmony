// SPDX-License-Identifier: AGPL-3.0-or-later
#![no_std]
#![doc = "Deterministic guest/host hypercall wire protocol framing, guest client helpers, and host dispatch support for the deterministic VMM."]
//!
//! # Services and opcodes (the wire ABI)
//!
//! Every request names a [`ServiceId`] and a service-specific `opcode`. The
//! registered services and their opcodes (mirrored in `docs/INTEGRATION.md` §1):
//!
//! | Service | id | opcode(s) |
//! |---------|----|-----------|
//! | [`Console`](ServiceId::Console) | 1 | `1` = write bytes |
//! | [`Entropy`](ServiceId::Entropy) | 2 | `1` = fill from the seeded stream |
//! | [`Block`](ServiceId::Block)     | 3 | `1` = capacity, `2` = read sectors |
//! | [`Event`](ServiceId::Event)     | 4 | `1` = emit `(event_id, bytes)` (fire-and-forget) |
//! | [`Net`](ServiceId::Net)         | 5 | `1` = `net_decide` (round-trips a per-flow policy answer) |
//! | [`Sdk`](ServiceId::Sdk)         | 6 | `1` = `buggify_decide` (round-trips a one-byte fire / no-fire answer) |
//! | [`Pvclock`](ServiceId::Pvclock) | 7 | `1` = `pvclock_register` (publishes the guest clock-page GPA) |
//!
//! Id **5** is the task-61 `Net` vertical (the first guest-plane fault path); the
//! task-73 SDK control service ([`Sdk`](ServiceId::Sdk)) takes id **6**; the
//! task-110 paravirt work-derived clock registration ([`Pvclock`](ServiceId::Pvclock))
//! takes id **7**. An
//! unregistered service id or an opcode a service does not implement is a
//! [`Status::UnknownService`] / [`Status::UnknownOpcode`], never a silent drop.
//!
//! ## `Net` — the per-flow decision service (task 61)
//!
//! `net_decide` (op `1`) round-trips one **per-flow** decision: the guest flow
//! agent asks "what should I do with this flow?" once per flow/connection (never
//! per frame — the host is on the control path only). The request payload is a
//! fixed **18-byte little-endian** `NetFlow` decision point:
//!
//! | offset | field   | type  |
//! |--------|---------|-------|
//! | 0      | `src`   | `u32` |
//! | 4      | `dst`   | `u32` |
//! | 8      | `conn`  | `u64` |
//! | 16     | `event` | `u16` |
//!
//! The response payload is the **opaque, environment-encoded flow-policy answer**
//! (the guest decodes it against its own catalog — a `Nominal` deliver-normally, or
//! a `NetLatency`/`NetLoss`/`NetThrottle`/`NetReset` policy it enforces on the
//! intra-guest CNI). This crate is `consonance` substrate and deliberately does
//! **not** depend on the `dissonance/environment` catalog: it frames the request
//! fields and ferries the answer bytes verbatim, bounding their length but never
//! interpreting them. The production host ([`consonance/vmm-core`]) decodes the
//! request into an `environment::DecisionPoint::NetFlow`, resolves it through its
//! `Environment::decide` seam, records the answer at the surfacing `Moment`, and
//! writes back `Answer::encode()` — exactly as the `Sdk` service wires
//! `buggify_decide`, one wire shape either way. [`NetDecider`] is the deterministic
//! **reference** answerer used by loopback tests (a scripted per-flow table).

#[cfg(feature = "host")]
extern crate std;

#[cfg(feature = "host")]
use std::{boxed::Box, collections::BTreeMap, vec::Vec};

use core::fmt;

/// Maximum bytes in a hypercall frame, including the header.
pub const MAX_FRAME: usize = 4096;
/// Size of the fixed wire header in bytes.
pub const HEADER_LEN: usize = 24;
/// Maximum bytes in a hypercall frame payload.
pub const MAX_PAYLOAD: usize = MAX_FRAME - HEADER_LEN;
const MAGIC: u32 = 0x3150_4348;
const KIND_REQUEST: u16 = 1;
const KIND_RESPONSE: u16 = 2;
const SECTOR_SIZE: usize = 512;
const BLOCK_READ_MAX_SECTORS: usize = 7;

/// Wire length of a [`ServiceId::Net`] `net_decide` request payload: the fixed
/// 18-byte little-endian `NetFlow { src:u32, dst:u32, conn:u64, event:u16 }`
/// decision point (see the crate-level `Net` service docs).
pub const NET_REQUEST_LEN: usize = 18;
#[cfg(feature = "host")]
const ENTROPY_FALLBACK_SEED: u64 = 0x9E37_79B9_7F4A_7C15;
#[cfg(feature = "host")]
const ENTROPY_MUL: u64 = 0x2545_F491_4F6C_DD1D;

/// Hypercall service identifiers used on the wire.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ServiceId {
    /// Console output service.
    Console = 1,
    /// Deterministic entropy service.
    Entropy = 2,
    /// Read-only block device service.
    Block = 3,
    /// Test/coverage event service.
    Event = 4,
    /// Network per-flow decision service (task 61): the guest flow agent asks the
    /// host what to do with a flow (op 1, `net_decide`). One request carries an
    /// 18-byte little-endian `NetFlow { src:u32, dst:u32, conn:u64, event:u16 }`
    /// decision point; the response carries the **opaque** environment-encoded
    /// flow-policy answer the guest enforces on the intra-guest CNI. The host
    /// resolves it through its `Environment::decide` seam and records it at the
    /// surfacing `Moment`. One decision per flow/connection, never per frame.
    Net = 5,
    /// SDK control service (task 73): the guest asks the host to resolve a
    /// buggify decision (op 1, `buggify_decide`). Service id **5** is the task-61
    /// `Net` vertical, so the SDK takes **6**. Unlike the fire-and-forget
    /// [`Event`](ServiceId::Event) service, this one round-trips a one-byte answer
    /// (fire / don't fire); the host resolves it through its `Environment::decide`
    /// seam and records it at the surfacing `Moment`.
    Sdk = 6,
    /// Paravirt work-derived clock registration (task 110,
    /// `docs/PARAVIRT-CLOCK.md` §3.1): the guest publishes the guest-physical
    /// address of its 4 KiB clock page (op 1, `pvclock_register` — an 8-byte
    /// little-endian GPA). The host validates the GPA (page-aligned, inside
    /// guest RAM, clear of the doorbell frame pages and of any device-MMIO
    /// hole), **records it as pending**, and answers with the 4-byte
    /// little-endian page-layout ABI version (`HARMONY_PVCLOCK_ABI = 1`).
    ///
    /// **Registration is a two-step handshake — the page is NOT stamped by the
    /// response.** The doorbell `OUT` is a plain PIO exit, not a V-time
    /// intercept, so the host lays down the first page stamp and arms its
    /// staleness refresh only at the guest's **required** post-response counter
    /// read (an `RDTSC`/`RDTSCP` — a genuine skid-free intercept). A conforming
    /// guest MUST execute that read before reading the page: reading the page
    /// immediately after the response would observe stale bytes (ABI version
    /// zero / no `MATERIALIZED` flag). A guest that omits the handshake is out of
    /// contract — its page is never stamped and never refreshed.
    ///
    /// A host not composed with the clock page — or one whose backend has no
    /// deterministic work counter to derive the stamps from — answers
    /// [`Status::UnknownService`], and the guest keeps its trap-backstopped time
    /// paths (the page is pure opt-in on both sides).
    Pvclock = 7,
}

impl ServiceId {
    fn as_u16(self) -> u16 {
        self as u16
    }
}

/// Hypercall response status codes.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    /// Request completed successfully.
    Ok = 0,
    /// Request frame or payload was malformed.
    BadRequest = 1,
    /// No service was registered for the requested service id.
    UnknownService = 2,
    /// The service does not implement the requested opcode.
    UnknownOpcode = 3,
    /// The request addressed data outside the service's valid range.
    OutOfRange = 4,
    /// The service or dispatcher encountered an internal failure.
    Internal = 5,
}

impl Status {
    fn from_u16(value: u16) -> Option<Self> {
        match value {
            0 => Some(Self::Ok),
            1 => Some(Self::BadRequest),
            2 => Some(Self::UnknownService),
            3 => Some(Self::UnknownOpcode),
            4 => Some(Self::OutOfRange),
            5 => Some(Self::Internal),
            _ => None,
        }
    }

    fn as_u16(self) -> u16 {
        self as u16
    }
}

/// Decoded hypercall frame header fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameHeader {
    /// Wire magic value, always `0x31504348` for decoded frames.
    pub magic: u32,
    /// Frame kind: `1` request or `2` response.
    pub kind: u16,
    /// Raw service identifier.
    pub service: u16,
    /// Service-specific opcode.
    pub opcode: u16,
    /// Raw status code; requests use zero.
    pub status: u16,
    /// Request sequence number, echoed by responses.
    pub seq: u32,
    /// Payload length in bytes.
    pub payload_len: u32,
    /// Reserved field, always zero for decoded frames.
    pub reserved: u32,
}

// Host-only (a dispatcher concern): the guest is a client, never routes frames,
// so gating this keeps the `no_std` guest build — hence the SDK-demo binary and
// its hashed memory image — byte-identical.
#[cfg(feature = "host")]
impl FrameHeader {
    /// Whether this header is a **structurally valid request** — every
    /// request-header invariant [`decode`] does NOT already enforce, checked in
    /// one step so a dispatcher servicing guest bytes validates the whole header
    /// before routing (not one field per bug report):
    ///
    /// - `kind == 1` (request): [`decode`] accepts BOTH request and response
    ///   frames (the guest client decodes responses), so a response-typed frame
    ///   is not a valid request.
    /// - `status == 0`: `status` is a **response-only** field; a request carrying
    ///   a non-zero status is malformed and must not be serviced.
    /// - `reserved == 0`: defense in depth — [`decode`] already rejects a
    ///   non-zero reserved, but re-checking makes this a total request predicate
    ///   independent of that guarantee.
    ///
    /// `magic` and `reserved`/`payload_len` bounds are validated by [`decode`]
    /// itself, and `seq` is an arbitrary caller value (no invariant). **Service /
    /// opcode validity is deliberately NOT here** — an unknown service or opcode
    /// is a routing outcome with its own correlatable status
    /// ([`Status::UnknownService`] / [`Status::UnknownOpcode`]), not a `BadRequest`.
    pub fn is_request(&self) -> bool {
        self.kind == KIND_REQUEST && self.status == 0 && self.reserved == 0
    }
}

/// Protocol errors produced by frame, client, and snapshot handling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "host", derive(thiserror::Error))]
pub enum ProtoError {
    /// A caller-provided output buffer is too small.
    #[cfg_attr(feature = "host", error("buffer too small"))]
    BufferTooSmall,
    /// Payload is larger than the single-page wire format permits.
    #[cfg_attr(feature = "host", error("payload too large"))]
    PayloadTooLarge,
    /// Input ended before a complete frame or field was available.
    #[cfg_attr(feature = "host", error("truncated frame"))]
    Truncated,
    /// The frame magic is not `HCP1`.
    #[cfg_attr(feature = "host", error("bad magic"))]
    BadMagic,
    /// A header field has an invalid value.
    #[cfg_attr(feature = "host", error("invalid header"))]
    InvalidHeader,
    /// A service payload is malformed.
    #[cfg_attr(feature = "host", error("bad payload"))]
    BadPayload,
    /// A saved-state blob is malformed or does not match registration.
    #[cfg_attr(feature = "host", error("bad state"))]
    BadState,
}

#[cfg(not(feature = "host"))]
impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::BufferTooSmall => "buffer too small",
            Self::PayloadTooLarge => "payload too large",
            Self::Truncated => "truncated frame",
            Self::BadMagic => "bad magic",
            Self::InvalidHeader => "invalid header",
            Self::BadPayload => "bad payload",
            Self::BadState => "bad state",
        };
        f.write_str(text)
    }
}

/// Encode a request frame into `buf`, returning the total frame length.
pub fn encode_request(
    service: ServiceId,
    opcode: u16,
    seq: u32,
    payload: &[u8],
    buf: &mut [u8],
) -> Result<usize, ProtoError> {
    encode_frame(KIND_REQUEST, service.as_u16(), opcode, 0, seq, payload, buf)
}

/// Encode a response frame into `buf`, returning the total frame length.
pub fn encode_response(
    service: ServiceId,
    opcode: u16,
    seq: u32,
    status: Status,
    payload: &[u8],
    buf: &mut [u8],
) -> Result<usize, ProtoError> {
    encode_response_raw(service.as_u16(), opcode, seq, status, payload, buf)
}

fn encode_response_raw(
    service: u16,
    opcode: u16,
    seq: u32,
    status: Status,
    payload: &[u8],
    buf: &mut [u8],
) -> Result<usize, ProtoError> {
    encode_frame(
        KIND_RESPONSE,
        service,
        opcode,
        status.as_u16(),
        seq,
        payload,
        buf,
    )
}

/// Encode an **empty error response** echoing a **raw** `service`/`opcode` (task
/// 73). A doorbell dispatcher answers an unrecognized `service` id — which no
/// [`ServiceId`] variant represents, so [`encode_response`] cannot express it —
/// with a clean [`Status::UnknownService`] frame that echoes the request's raw
/// `service`/`opcode`/`seq`. The guest transport validates that echo (service +
/// opcode + seq must match its request), so echoing the raw fields lets it
/// correlate the frame and surface `ClientError::Status(UnknownService)` instead
/// of hanging on a missing reply — honoring the module contract ("never a silent
/// drop"). Mirrors the reference server's dispatch. Returns `0` if `buf` is too
/// small (the doorbell reads that as "no reply written").
pub fn encode_error(service: u16, opcode: u16, seq: u32, status: Status, buf: &mut [u8]) -> usize {
    encode_response_raw(service, opcode, seq, status, &[], buf).unwrap_or(0)
}

fn encode_frame(
    kind: u16,
    service: u16,
    opcode: u16,
    status: u16,
    seq: u32,
    payload: &[u8],
    buf: &mut [u8],
) -> Result<usize, ProtoError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(ProtoError::PayloadTooLarge);
    }
    let total = HEADER_LEN + payload.len();
    if buf.len() < total {
        return Err(ProtoError::BufferTooSmall);
    }
    write_header(
        buf,
        kind,
        service,
        opcode,
        status,
        seq,
        payload.len() as u32,
    );
    buf[HEADER_LEN..total].copy_from_slice(payload);
    Ok(total)
}

/// Write the 24-byte wire header into `buf[..HEADER_LEN]`; callers guarantee capacity.
fn write_header(
    buf: &mut [u8],
    kind: u16,
    service: u16,
    opcode: u16,
    status: u16,
    seq: u32,
    payload_len: u32,
) {
    put_u32(&mut buf[0..4], MAGIC);
    put_u16(&mut buf[4..6], kind);
    put_u16(&mut buf[6..8], service);
    put_u16(&mut buf[8..10], opcode);
    put_u16(&mut buf[10..12], status);
    put_u32(&mut buf[12..16], seq);
    put_u32(&mut buf[16..20], payload_len);
    put_u32(&mut buf[20..24], 0);
}

/// Decode and validate a frame, returning its header and a payload slice borrowed from `buf`.
pub fn decode(buf: &[u8]) -> Result<(FrameHeader, &[u8]), ProtoError> {
    if buf.len() < HEADER_LEN {
        return Err(ProtoError::Truncated);
    }
    let magic = read_u32(buf, 0)?;
    if magic != MAGIC {
        return Err(ProtoError::BadMagic);
    }
    let kind = read_u16(buf, 4)?;
    if kind != KIND_REQUEST && kind != KIND_RESPONSE {
        return Err(ProtoError::InvalidHeader);
    }
    let service = read_u16(buf, 6)?;
    let opcode = read_u16(buf, 8)?;
    let status = read_u16(buf, 10)?;
    let seq = read_u32(buf, 12)?;
    let payload_len = read_u32(buf, 16)?;
    let reserved = read_u32(buf, 20)?;
    if reserved != 0 || payload_len as usize > MAX_PAYLOAD {
        return Err(ProtoError::InvalidHeader);
    }
    let end = HEADER_LEN
        .checked_add(payload_len as usize)
        .ok_or(ProtoError::InvalidHeader)?;
    if buf.len() < end {
        return Err(ProtoError::Truncated);
    }
    let header = FrameHeader {
        magic,
        kind,
        service,
        opcode,
        status,
        seq,
        payload_len,
        reserved,
    };
    Ok((header, &buf[HEADER_LEN..end]))
}

#[cfg(feature = "host")]
fn raw_header_fields(buf: &[u8]) -> Option<(u16, u16, u32)> {
    if buf.len() < HEADER_LEN {
        return None;
    }
    Some((
        read_u16(buf, 6).ok()?,
        read_u16(buf, 8).ok()?,
        read_u32(buf, 12).ok()?,
    ))
}

fn put_u16(dst: &mut [u8], value: u16) {
    dst.copy_from_slice(&value.to_le_bytes());
}

fn put_u32(dst: &mut [u8], value: u32) {
    dst.copy_from_slice(&value.to_le_bytes());
}

fn put_u64(dst: &mut [u8], value: u64) {
    dst.copy_from_slice(&value.to_le_bytes());
}

fn read_u16(buf: &[u8], offset: usize) -> Result<u16, ProtoError> {
    let bytes = buf.get(offset..offset + 2).ok_or(ProtoError::Truncated)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(buf: &[u8], offset: usize) -> Result<u32, ProtoError> {
    let bytes = buf.get(offset..offset + 4).ok_or(ProtoError::Truncated)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u64(buf: &[u8], offset: usize) -> Result<u64, ProtoError> {
    let bytes = buf.get(offset..offset + 8).ok_or(ProtoError::Truncated)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

#[cfg(feature = "guest")]
mod guest {
    use super::*;

    /// Transport implemented by the guest VMCALL shim.
    pub trait Transport {
        /// Transport-specific error type.
        type Error;
        /// Submit a request frame and write the response frame into `resp`.
        fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Self::Error>;
    }

    /// Errors returned by the guest client.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum ClientError<E> {
        /// The underlying transport failed.
        Transport(E),
        /// Frame encoding or decoding failed.
        Protocol(ProtoError),
        /// The response sequence did not match the request.
        SeqMismatch,
        /// The response was not an `Ok` status.
        Status(Status),
        /// The caller supplied an invalid length.
        InvalidLength,
    }

    impl<E: fmt::Debug> fmt::Display for ClientError<E> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Transport(_) => f.write_str("transport error"),
                Self::Protocol(e) => write!(f, "protocol error: {e}"),
                Self::SeqMismatch => f.write_str("sequence mismatch"),
                Self::Status(s) => write!(f, "non-ok status: {s:?}"),
                Self::InvalidLength => f.write_str("invalid length"),
            }
        }
    }

    /// Guest-side client for deterministic hypercall services.
    pub struct Client<T: Transport> {
        transport: T,
        seq: u32,
    }

    impl<T: Transport> Client<T> {
        /// Create a client with sequence counter starting at one.
        pub fn new(transport: T) -> Self {
            Self { transport, seq: 1 }
        }

        /// Write bytes to the console service.
        pub fn console_write(&mut self, bytes: &[u8]) -> Result<(), ClientError<T::Error>> {
            let mut offset = 0;
            while offset < bytes.len() || (bytes.is_empty() && offset == 0) {
                let end = core::cmp::min(offset + MAX_PAYLOAD, bytes.len());
                self.call_expect_empty(ServiceId::Console, 1, &bytes[offset..end])?;
                if bytes.is_empty() {
                    break;
                }
                offset = end;
            }
            Ok(())
        }

        /// Fill `out` with deterministic entropy bytes.
        pub fn entropy_fill(&mut self, out: &mut [u8]) -> Result<(), ClientError<T::Error>> {
            let mut offset = 0;
            while offset < out.len() {
                let n = core::cmp::min(MAX_PAYLOAD, out.len() - offset);
                let mut payload = [0_u8; 4];
                put_u32(&mut payload, n as u32);
                let copied = self.call_copy(
                    ServiceId::Entropy,
                    1,
                    &payload,
                    &mut out[offset..offset + n],
                )?;
                if copied != n {
                    return Err(ClientError::Protocol(ProtoError::BadPayload));
                }
                offset += n;
            }
            Ok(())
        }

        /// Return the block device capacity in 512-byte sectors.
        pub fn block_capacity(&mut self) -> Result<u64, ClientError<T::Error>> {
            let mut out = [0_u8; 8];
            let copied = self.call_copy(ServiceId::Block, 1, &[], &mut out)?;
            if copied != 8 {
                return Err(ClientError::Protocol(ProtoError::BadPayload));
            }
            read_u64(&out, 0).map_err(ClientError::Protocol)
        }

        /// Read sectors beginning at `lba` into `out`.
        pub fn block_read(
            &mut self,
            mut lba: u64,
            out: &mut [u8],
        ) -> Result<(), ClientError<T::Error>> {
            if !out.len().is_multiple_of(SECTOR_SIZE) {
                return Err(ClientError::InvalidLength);
            }
            let mut offset = 0;
            while offset < out.len() {
                let remaining = (out.len() - offset) / SECTOR_SIZE;
                let sectors = core::cmp::min(BLOCK_READ_MAX_SECTORS, remaining);
                let byte_len = sectors * SECTOR_SIZE;
                let mut payload = [0_u8; 12];
                put_u64(&mut payload[0..8], lba);
                put_u32(&mut payload[8..12], sectors as u32);
                let copied = self.call_copy(
                    ServiceId::Block,
                    2,
                    &payload,
                    &mut out[offset..offset + byte_len],
                )?;
                if copied != byte_len {
                    return Err(ClientError::Protocol(ProtoError::BadPayload));
                }
                lba = lba.wrapping_add(sectors as u64);
                offset += byte_len;
            }
            Ok(())
        }

        /// Emit a deterministic test/coverage event.
        ///
        /// One emit is exactly one Emit request: `data` longer than
        /// `MAX_PAYLOAD - 4` is rejected, never fragmented into multiple
        /// events (the host counts each frame as a distinct event).
        pub fn event_emit(&mut self, id: u32, data: &[u8]) -> Result<(), ClientError<T::Error>> {
            if data.len() > MAX_PAYLOAD - 4 {
                return Err(ClientError::InvalidLength);
            }
            let mut payload = [0_u8; MAX_PAYLOAD];
            put_u32(&mut payload[..4], id);
            payload[4..4 + data.len()].copy_from_slice(data);
            self.call_expect_empty(ServiceId::Event, 1, &payload[..4 + data.len()])
        }

        /// Ask the host to resolve a **buggify** decision for `point` (task 73's
        /// SDK control service, [`ServiceId::Sdk`], op 1). Returns whether the
        /// host decided to **fire** the deliberate perturbation. One request
        /// carries the 4-byte little-endian `point`; the response is exactly one
        /// byte (`0` = don't fire, non-zero = fire) — any other length is a
        /// protocol error, never trusted.
        pub fn buggify_decide(&mut self, point: u32) -> Result<bool, ClientError<T::Error>> {
            let mut payload = [0_u8; 4];
            put_u32(&mut payload, point);
            let mut out = [0_u8; 1];
            let copied = self.call_copy(ServiceId::Sdk, 1, &payload, &mut out)?;
            if copied != 1 {
                return Err(ClientError::Protocol(ProtoError::BadPayload));
            }
            Ok(out[0] != 0)
        }

        /// Ask the host what to do with a flow (task 61's `Net` service,
        /// [`ServiceId::Net`], op 1). Sends the [`NET_REQUEST_LEN`]-byte
        /// `NetFlow { src, dst, conn, event }` decision point and copies the
        /// host's **opaque** flow-policy answer bytes into `out`, returning their
        /// length. One ask per flow/connection (never per frame): the host is on
        /// the control path only. The answer is the environment-encoded policy the
        /// caller decodes against its own catalog — this transport neither
        /// interprets nor bounds it beyond `out`'s capacity (a longer answer is a
        /// [`ProtoError::BufferTooSmall`]). An empty answer (`0` bytes copied) is a
        /// protocol error: the host always answers at least a one-byte `Nominal`.
        pub fn net_decide(
            &mut self,
            src: u32,
            dst: u32,
            conn: u64,
            event: u16,
            out: &mut [u8],
        ) -> Result<usize, ClientError<T::Error>> {
            let mut payload = [0_u8; NET_REQUEST_LEN];
            put_u32(&mut payload[0..4], src);
            put_u32(&mut payload[4..8], dst);
            put_u64(&mut payload[8..16], conn);
            put_u16(&mut payload[16..18], event);
            let copied = self.call_copy(ServiceId::Net, 1, &payload, out)?;
            if copied == 0 {
                return Err(ClientError::Protocol(ProtoError::BadPayload));
            }
            Ok(copied)
        }

        /// Publish the guest's paravirt clock-page GPA to the host (task 110's
        /// [`ServiceId::Pvclock`], op 1) and return the host's page-layout ABI
        /// version (`HARMONY_PVCLOCK_ABI`). One request carries the 8-byte
        /// little-endian page-aligned `gpa`; the response is exactly the 4-byte
        /// little-endian ABI version — any other length is a protocol error,
        /// never trusted. The caller must treat any error (including the
        /// [`Status::UnknownService`] a clock-page-less host answers) as "no
        /// page offered" and keep its trap-backstopped time paths.
        ///
        /// **The response only records a pending registration — it does NOT
        /// stamp the page (see [`ServiceId::Pvclock`]).** After a successful
        /// response the caller MUST perform the registration handshake — a
        /// single `RDTSC`/`RDTSCP` — before reading the page; that counter read
        /// is the intercept at which the host writes the first stamp. Reading the
        /// page between the response and the handshake observes stale bytes.
        pub fn pvclock_register(&mut self, gpa: u64) -> Result<u32, ClientError<T::Error>> {
            let mut payload = [0_u8; 8];
            put_u64(&mut payload, gpa);
            let mut out = [0_u8; 4];
            let copied = self.call_copy(ServiceId::Pvclock, 1, &payload, &mut out)?;
            if copied != 4 {
                return Err(ClientError::Protocol(ProtoError::BadPayload));
            }
            read_u32(&out, 0).map_err(ClientError::Protocol)
        }

        fn call_expect_empty(
            &mut self,
            service: ServiceId,
            opcode: u16,
            payload: &[u8],
        ) -> Result<(), ClientError<T::Error>> {
            let mut req = [0_u8; MAX_FRAME];
            let len = encode_request(service, opcode, self.next_seq(), payload, &mut req)
                .map_err(ClientError::Protocol)?;
            self.exchange_empty(service, opcode, len, &req)
        }

        fn call_copy(
            &mut self,
            service: ServiceId,
            opcode: u16,
            payload: &[u8],
            out: &mut [u8],
        ) -> Result<usize, ClientError<T::Error>> {
            let mut req = [0_u8; MAX_FRAME];
            let len = encode_request(service, opcode, self.next_seq(), payload, &mut req)
                .map_err(ClientError::Protocol)?;
            self.exchange_copy(service, opcode, len, &req, out)
        }

        fn exchange_empty(
            &mut self,
            service: ServiceId,
            opcode: u16,
            req_len: usize,
            req: &[u8; MAX_FRAME],
        ) -> Result<(), ClientError<T::Error>> {
            let mut scratch = [];
            let copied = self.exchange_copy(service, opcode, req_len, req, &mut scratch)?;
            if copied != 0 {
                return Err(ClientError::Protocol(ProtoError::BadPayload));
            }
            Ok(())
        }

        fn exchange_copy(
            &mut self,
            service: ServiceId,
            opcode: u16,
            req_len: usize,
            req: &[u8; MAX_FRAME],
            out: &mut [u8],
        ) -> Result<usize, ClientError<T::Error>> {
            let mut resp = [0_u8; MAX_FRAME];
            let seq = read_u32(req, 12).map_err(ClientError::Protocol)?;
            let len = self
                .transport
                .exchange(&req[..req_len], &mut resp)
                .map_err(ClientError::Transport)?;
            // `len` ultimately comes from the host (RAX); never trust it to be in bounds.
            let frame = resp
                .get(..len)
                .ok_or(ClientError::Protocol(ProtoError::Truncated))?;
            let (header, payload) = decode(frame).map_err(ClientError::Protocol)?;
            if header.seq != seq
                || header.service != service.as_u16()
                || header.opcode != opcode
                || header.kind != KIND_RESPONSE
            {
                return Err(ClientError::SeqMismatch);
            }
            let status = Status::from_u16(header.status)
                .ok_or(ClientError::Protocol(ProtoError::InvalidHeader))?;
            if status != Status::Ok {
                return Err(ClientError::Status(status));
            }
            if payload.len() > out.len() {
                return Err(ClientError::Protocol(ProtoError::BufferTooSmall));
            }
            out[..payload.len()].copy_from_slice(payload);
            Ok(payload.len())
        }

        fn next_seq(&mut self) -> u32 {
            let seq = self.seq;
            self.seq = self.seq.wrapping_add(1);
            if self.seq == 0 {
                self.seq = 1;
            }
            seq
        }
    }
}

#[cfg(feature = "guest")]
pub use guest::{Client, ClientError, Transport};

#[cfg(feature = "host")]
mod host {
    use super::*;

    /// Host-side implementation of one service.
    pub trait Service {
        /// Handle one request payload and write the response payload into `resp_payload`.
        fn handle(
            &mut self,
            opcode: u16,
            payload: &[u8],
            resp_payload: &mut [u8],
        ) -> (Status, usize);
        /// Serialize all state that influences future responses.
        fn save_state(&self) -> Vec<u8>;
        /// Restore state produced by `save_state`.
        fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError>;
    }

    /// Host dispatcher that routes request frames to registered services.
    pub struct Dispatcher {
        services: BTreeMap<u16, Box<dyn Service>>,
    }

    impl Default for Dispatcher {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Dispatcher {
        /// Create an empty dispatcher.
        pub fn new() -> Self {
            Self {
                services: BTreeMap::new(),
            }
        }

        /// Register or replace a service implementation for `id`.
        pub fn register(&mut self, id: ServiceId, svc: Box<dyn Service>) {
            let _old = self.services.insert(id.as_u16(), svc);
        }

        /// Decode, route, and encode exactly one response frame.
        pub fn dispatch(&mut self, req_buf: &[u8], resp_buf: &mut [u8]) -> usize {
            if resp_buf.len() < HEADER_LEN {
                return 0;
            }
            let decoded = decode(req_buf);
            let (header, payload) = match decoded {
                Ok(value) => value,
                Err(ProtoError::BadMagic) => {
                    return encode_error(0, 0, 0, Status::BadRequest, resp_buf);
                }
                // Any other failure (truncated payload, bad reserved/len/kind) means the
                // 24-byte header itself was readable, so its raw fields must be echoed;
                // only a header shorter than 24 bytes (None) takes the all-zeros path.
                Err(_) => {
                    let (service, opcode, seq) = raw_header_fields(req_buf).unwrap_or((0, 0, 0));
                    return encode_error(service, opcode, seq, Status::BadRequest, resp_buf);
                }
            };

            if header.kind != KIND_REQUEST || header.status != 0 {
                return encode_error(
                    header.service,
                    header.opcode,
                    header.seq,
                    Status::BadRequest,
                    resp_buf,
                );
            }

            let Some(service) = self.services.get_mut(&header.service) else {
                return encode_error(
                    header.service,
                    header.opcode,
                    header.seq,
                    Status::UnknownService,
                    resp_buf,
                );
            };

            let avail = resp_buf.len().saturating_sub(HEADER_LEN);
            let (status, payload_len) =
                service.handle(header.opcode, payload, &mut resp_buf[HEADER_LEN..]);
            if payload_len > avail || payload_len > MAX_PAYLOAD {
                return encode_error(
                    header.service,
                    header.opcode,
                    header.seq,
                    Status::Internal,
                    resp_buf,
                );
            }
            // The service already wrote its payload at resp_buf[HEADER_LEN..]; finish
            // the frame by writing the header in front of it.
            write_header(
                resp_buf,
                KIND_RESPONSE,
                header.service,
                header.opcode,
                status.as_u16(),
                header.seq,
                payload_len as u32,
            );
            HEADER_LEN + payload_len
        }

        /// Snapshot registered services in ascending service-id order.
        pub fn save_state(&self) -> Vec<u8> {
            let mut out = Vec::new();
            for (id, service) in &self.services {
                let state = service.save_state();
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&(state.len() as u32).to_le_bytes());
                out.extend_from_slice(&state);
            }
            out
        }

        /// Restore a snapshot into an identically registered dispatcher.
        ///
        /// On error the dispatcher is left in the state it had on entry; a failed
        /// restore never leaves services partially overwritten.
        pub fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError> {
            let backup = self.save_state();
            self.try_restore(state).inspect_err(|_| {
                // Rolling back replays each service's own save_state output, which
                // the Service contract obliges restore_state to accept.
                let _ = self.try_restore(&backup);
            })
        }

        fn try_restore(&mut self, state: &[u8]) -> Result<(), ProtoError> {
            let mut offset = 0;
            for (id, service) in &mut self.services {
                let id_bytes = state.get(offset..offset + 2).ok_or(ProtoError::BadState)?;
                let found = u16::from_le_bytes([id_bytes[0], id_bytes[1]]);
                offset += 2;
                if found != *id {
                    return Err(ProtoError::BadState);
                }
                let len_bytes = state.get(offset..offset + 4).ok_or(ProtoError::BadState)?;
                let len =
                    u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]])
                        as usize;
                offset += 4;
                let bytes = state
                    .get(offset..offset + len)
                    .ok_or(ProtoError::BadState)?;
                service.restore_state(bytes)?;
                offset += len;
            }
            if offset != state.len() {
                return Err(ProtoError::BadState);
            }
            Ok(())
        }
    }

    fn encode_error(
        service: u16,
        opcode: u16,
        seq: u32,
        status: Status,
        resp_buf: &mut [u8],
    ) -> usize {
        encode_response_raw(service, opcode, seq, status, &[], resp_buf).unwrap_or_default()
    }

    /// Reference console service that collects all written bytes.
    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub struct ConsoleSink {
        bytes: Vec<u8>,
    }

    impl ConsoleSink {
        /// Create an empty console sink.
        pub fn new() -> Self {
            Self { bytes: Vec::new() }
        }

        /// Return all bytes written so far.
        pub fn bytes(&self) -> &[u8] {
            &self.bytes
        }
    }

    impl Service for ConsoleSink {
        fn handle(
            &mut self,
            opcode: u16,
            payload: &[u8],
            _resp_payload: &mut [u8],
        ) -> (Status, usize) {
            if opcode != 1 {
                return (Status::UnknownOpcode, 0);
            }
            self.bytes.extend_from_slice(payload);
            (Status::Ok, 0)
        }

        fn save_state(&self) -> Vec<u8> {
            self.bytes.clone()
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError> {
            self.bytes.clear();
            self.bytes.extend_from_slice(state);
            Ok(())
        }
    }

    /// Reference deterministic entropy service using the specified xorshift64* stream.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct SeededEntropy {
        state: u64,
    }

    impl SeededEntropy {
        /// Create a deterministic entropy stream from `seed`.
        pub fn new(seed: u64) -> Self {
            Self {
                state: normalize_seed(seed),
            }
        }

        fn next(&mut self) -> u64 {
            self.state ^= self.state >> 12;
            self.state ^= self.state << 25;
            self.state ^= self.state >> 27;
            self.state.wrapping_mul(ENTROPY_MUL)
        }
    }

    impl Service for SeededEntropy {
        fn handle(
            &mut self,
            opcode: u16,
            payload: &[u8],
            resp_payload: &mut [u8],
        ) -> (Status, usize) {
            if opcode != 1 {
                return (Status::UnknownOpcode, 0);
            }
            if payload.len() != 4 {
                return (Status::BadRequest, 0);
            }
            let n = match read_u32(payload, 0) {
                Ok(value) if value >= 1 && value as usize <= MAX_PAYLOAD => value as usize,
                _ => return (Status::BadRequest, 0),
            };
            if resp_payload.len() < n {
                return (Status::Internal, 0);
            }
            let mut offset = 0;
            while offset < n {
                let word = self.next().to_le_bytes();
                let take = core::cmp::min(8, n - offset);
                resp_payload[offset..offset + take].copy_from_slice(&word[..take]);
                offset += take;
            }
            (Status::Ok, n)
        }

        fn save_state(&self) -> Vec<u8> {
            self.state.to_le_bytes().to_vec()
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError> {
            if state.len() != 8 {
                return Err(ProtoError::BadState);
            }
            let value = read_u64(state, 0)?;
            // save_state can never produce 0 (seed 0 is remapped and xorshift64 is a
            // bijection on nonzero states); accepting it would pin the stream at zero.
            if value == 0 {
                return Err(ProtoError::BadState);
            }
            self.state = value;
            Ok(())
        }
    }

    fn normalize_seed(seed: u64) -> u64 {
        if seed == 0 {
            ENTROPY_FALLBACK_SEED
        } else {
            seed
        }
    }

    /// Reference read-only in-memory block device with 512-byte sectors.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct MemBlockDevice {
        data: Vec<u8>,
    }

    impl MemBlockDevice {
        /// Create a block device; `data.len()` must be a multiple of 512.
        pub fn new(data: Vec<u8>) -> Result<Self, ProtoError> {
            if !data.len().is_multiple_of(SECTOR_SIZE) {
                return Err(ProtoError::BadPayload);
            }
            Ok(Self { data })
        }

        /// Return capacity in 512-byte sectors.
        pub fn sector_count(&self) -> u64 {
            (self.data.len() / SECTOR_SIZE) as u64
        }
    }

    impl Service for MemBlockDevice {
        fn handle(
            &mut self,
            opcode: u16,
            payload: &[u8],
            resp_payload: &mut [u8],
        ) -> (Status, usize) {
            match opcode {
                1 => {
                    if !payload.is_empty() {
                        return (Status::BadRequest, 0);
                    }
                    if resp_payload.len() < 8 {
                        return (Status::Internal, 0);
                    }
                    put_u64(&mut resp_payload[..8], self.sector_count());
                    (Status::Ok, 8)
                }
                2 => {
                    if payload.len() != 12 {
                        return (Status::BadRequest, 0);
                    }
                    let lba = match read_u64(payload, 0) {
                        Ok(value) => value,
                        Err(_) => return (Status::BadRequest, 0),
                    };
                    let sectors = match read_u32(payload, 8) {
                        Ok(value) if (1..=BLOCK_READ_MAX_SECTORS as u32).contains(&value) => {
                            value as usize
                        }
                        _ => return (Status::BadRequest, 0),
                    };
                    let start_sector = match usize::try_from(lba) {
                        Ok(value) => value,
                        Err(_) => return (Status::OutOfRange, 0),
                    };
                    let start = match start_sector.checked_mul(SECTOR_SIZE) {
                        Some(value) => value,
                        None => return (Status::OutOfRange, 0),
                    };
                    let len = sectors * SECTOR_SIZE;
                    let end = match start.checked_add(len) {
                        Some(value) => value,
                        None => return (Status::OutOfRange, 0),
                    };
                    if end > self.data.len() {
                        return (Status::OutOfRange, 0);
                    }
                    if resp_payload.len() < len {
                        return (Status::Internal, 0);
                    }
                    resp_payload[..len].copy_from_slice(&self.data[start..end]);
                    (Status::Ok, len)
                }
                _ => (Status::UnknownOpcode, 0),
            }
        }

        fn save_state(&self) -> Vec<u8> {
            Vec::new()
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError> {
            if state.is_empty() {
                Ok(())
            } else {
                Err(ProtoError::BadState)
            }
        }
    }

    /// Reference event service that records emitted events.
    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub struct EventSink {
        events: Vec<(u32, Vec<u8>)>,
    }

    impl EventSink {
        /// Create an empty event sink.
        pub fn new() -> Self {
            Self { events: Vec::new() }
        }

        /// Return recorded events in arrival order.
        pub fn events(&self) -> &[(u32, Vec<u8>)] {
            &self.events
        }
    }

    impl Service for EventSink {
        fn handle(
            &mut self,
            opcode: u16,
            payload: &[u8],
            _resp_payload: &mut [u8],
        ) -> (Status, usize) {
            if opcode != 1 {
                return (Status::UnknownOpcode, 0);
            }
            if payload.len() < 4 {
                return (Status::BadRequest, 0);
            }
            let id = match read_u32(payload, 0) {
                Ok(value) => value,
                Err(_) => return (Status::BadRequest, 0),
            };
            self.events.push((id, payload[4..].to_vec()));
            (Status::Ok, 0)
        }

        fn save_state(&self) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(&(self.events.len() as u32).to_le_bytes());
            for (id, data) in &self.events {
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&(data.len() as u32).to_le_bytes());
                out.extend_from_slice(data);
            }
            out
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError> {
            let count_bytes = state.get(0..4).ok_or(ProtoError::BadState)?;
            let count = u32::from_le_bytes([
                count_bytes[0],
                count_bytes[1],
                count_bytes[2],
                count_bytes[3],
            ]) as usize;
            let mut offset = 4;
            let mut events = Vec::new();
            for _ in 0..count {
                let id_bytes = state.get(offset..offset + 4).ok_or(ProtoError::BadState)?;
                let id = u32::from_le_bytes([id_bytes[0], id_bytes[1], id_bytes[2], id_bytes[3]]);
                offset += 4;
                let len_bytes = state.get(offset..offset + 4).ok_or(ProtoError::BadState)?;
                let len =
                    u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]])
                        as usize;
                offset += 4;
                let data = state
                    .get(offset..offset + len)
                    .ok_or(ProtoError::BadState)?;
                events.push((id, data.to_vec()));
                offset += len;
            }
            if offset != state.len() {
                return Err(ProtoError::BadState);
            }
            self.events = events;
            Ok(())
        }
    }

    /// Reference SDK control service (task 73, [`ServiceId::Sdk`]): resolves a
    /// guest `buggify_decide(point)` (op 1) to a one-byte fire/don't-fire answer.
    ///
    /// This is the deterministic **reference** answerer used by loopback tests —
    /// it maps a point to a fixed decision from a per-point table plus a default,
    /// mirroring the host's per-point biasing at a table level (no PRNG). The
    /// production host wires this opcode to its `Environment::decide` seam
    /// instead; the wire shape is identical either way. Every ask is recorded, so
    /// a test can assert which points the guest actually reached.
    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub struct SdkBuggify {
        default_fire: bool,
        decisions: BTreeMap<u32, bool>,
        asked: Vec<u32>,
    }

    impl SdkBuggify {
        /// A service that answers every point with `default_fire` unless a
        /// per-point decision is set.
        pub fn new(default_fire: bool) -> Self {
            Self {
                default_fire,
                decisions: BTreeMap::new(),
                asked: Vec::new(),
            }
        }

        /// Pin the fire/don't-fire answer for a specific `point`.
        pub fn set_point(&mut self, point: u32, fire: bool) {
            self.decisions.insert(point, fire);
        }

        /// The points the guest has asked about, in call order.
        pub fn asked(&self) -> &[u32] {
            &self.asked
        }

        /// The decision in force for `point` (its override, else the default).
        fn decide(&self, point: u32) -> bool {
            self.decisions
                .get(&point)
                .copied()
                .unwrap_or(self.default_fire)
        }
    }

    impl Service for SdkBuggify {
        fn handle(
            &mut self,
            opcode: u16,
            payload: &[u8],
            resp_payload: &mut [u8],
        ) -> (Status, usize) {
            if opcode != 1 {
                return (Status::UnknownOpcode, 0);
            }
            if payload.len() != 4 {
                return (Status::BadRequest, 0);
            }
            let point = match read_u32(payload, 0) {
                Ok(value) => value,
                Err(_) => return (Status::BadRequest, 0),
            };
            if resp_payload.is_empty() {
                return (Status::Internal, 0);
            }
            self.asked.push(point);
            resp_payload[0] = u8::from(self.decide(point));
            (Status::Ok, 1)
        }

        fn save_state(&self) -> Vec<u8> {
            let mut out = Vec::new();
            out.push(u8::from(self.default_fire));
            out.extend_from_slice(&(self.decisions.len() as u32).to_le_bytes());
            for (point, fire) in &self.decisions {
                out.extend_from_slice(&point.to_le_bytes());
                out.push(u8::from(*fire));
            }
            out.extend_from_slice(&(self.asked.len() as u32).to_le_bytes());
            for point in &self.asked {
                out.extend_from_slice(&point.to_le_bytes());
            }
            out
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError> {
            let mut offset = 0;
            let default_fire = *state.get(offset).ok_or(ProtoError::BadState)? != 0;
            offset += 1;
            let dec_count = {
                let b = state.get(offset..offset + 4).ok_or(ProtoError::BadState)?;
                offset += 4;
                u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize
            };
            let mut decisions = BTreeMap::new();
            for _ in 0..dec_count {
                let b = state.get(offset..offset + 4).ok_or(ProtoError::BadState)?;
                let point = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                offset += 4;
                let fire = *state.get(offset).ok_or(ProtoError::BadState)? != 0;
                offset += 1;
                decisions.insert(point, fire);
            }
            let ask_count = {
                let b = state.get(offset..offset + 4).ok_or(ProtoError::BadState)?;
                offset += 4;
                u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize
            };
            let mut asked = Vec::new();
            for _ in 0..ask_count {
                let b = state.get(offset..offset + 4).ok_or(ProtoError::BadState)?;
                asked.push(u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                offset += 4;
            }
            if offset != state.len() {
                return Err(ProtoError::BadState);
            }
            self.default_fire = default_fire;
            self.decisions = decisions;
            self.asked = asked;
            Ok(())
        }
    }

    /// A decoded [`ServiceId::Net`] `net_decide` request — the `NetFlow` decision
    /// point the guest flow agent asks about. Carried in the fixed
    /// [`NET_REQUEST_LEN`]-byte little-endian wire form; part of the *live*
    /// decision a service reads, never of a serialized blob.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct NetFlowPoint {
        /// Source node of the flow.
        pub src: u32,
        /// Destination node of the flow.
        pub dst: u32,
        /// Connection identity (for fault targeting).
        pub conn: u64,
        /// What surfaced this flow decision (today, always `0` = flow open).
        pub event: u16,
    }

    impl NetFlowPoint {
        /// Decode the fixed [`NET_REQUEST_LEN`]-byte little-endian request payload,
        /// rejecting any other length.
        pub fn decode(payload: &[u8]) -> Option<Self> {
            if payload.len() != NET_REQUEST_LEN {
                return None;
            }
            Some(Self {
                src: read_u32(payload, 0).ok()?,
                dst: read_u32(payload, 4).ok()?,
                conn: read_u64(payload, 8).ok()?,
                event: read_u16(payload, 16).ok()?,
            })
        }
    }

    /// Reference network per-flow answerer (task 61, [`ServiceId::Net`]): resolves
    /// a guest `net_decide(point)` (op 1) to an **opaque** flow-policy answer.
    ///
    /// This is the deterministic **reference** answerer used by loopback tests — it
    /// maps a flow's `conn` to a fixed answer from a per-connection table plus a
    /// default, mirroring the host's per-flow policy at a table level (no PRNG). The
    /// production host wires this opcode to its `Environment::decide` seam instead,
    /// encoding the resolved `Answer`; the wire shape is identical either way. The
    /// answer bytes are opaque to this crate (it is `consonance` substrate and does
    /// not depend on the `environment` catalog): callers supply and decode them.
    /// Every ask is recorded, so a test can assert which flows the guest reached.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct NetDecider {
        default_answer: Vec<u8>,
        answers: BTreeMap<u64, Vec<u8>>,
        asked: Vec<NetFlowPoint>,
    }

    impl NetDecider {
        /// A service that answers every flow with `default_answer` (the encoded
        /// policy bytes) unless a per-connection answer is set. `default_answer`
        /// must be non-empty — the host always answers at least a one-byte
        /// `Nominal`, and an empty answer is a guest-side protocol error.
        pub fn new(default_answer: Vec<u8>) -> Self {
            Self {
                default_answer,
                answers: BTreeMap::new(),
                asked: Vec::new(),
            }
        }

        /// Pin the opaque answer bytes for a specific flow `conn`.
        pub fn set_flow(&mut self, conn: u64, answer: Vec<u8>) {
            let _old = self.answers.insert(conn, answer);
        }

        /// The flows the guest has asked about, in call order.
        pub fn asked(&self) -> &[NetFlowPoint] {
            &self.asked
        }

        /// The answer bytes in force for `conn` (its override, else the default).
        fn answer_for(&self, conn: u64) -> &[u8] {
            self.answers
                .get(&conn)
                .map_or(self.default_answer.as_slice(), Vec::as_slice)
        }
    }

    impl Service for NetDecider {
        fn handle(
            &mut self,
            opcode: u16,
            payload: &[u8],
            resp_payload: &mut [u8],
        ) -> (Status, usize) {
            if opcode != 1 {
                return (Status::UnknownOpcode, 0);
            }
            let Some(point) = NetFlowPoint::decode(payload) else {
                return (Status::BadRequest, 0);
            };
            let answer = self.answer_for(point.conn);
            if answer.len() > resp_payload.len() || answer.len() > MAX_PAYLOAD {
                return (Status::Internal, 0);
            }
            // Record the ask only once the response is known to fit, so a rejected
            // (too-large) answer does not leave a phantom decision in the log.
            resp_payload[..answer.len()].copy_from_slice(answer);
            let n = answer.len();
            self.asked.push(point);
            (Status::Ok, n)
        }

        fn save_state(&self) -> Vec<u8> {
            let mut out = Vec::new();
            put_len_prefixed(&mut out, &self.default_answer);
            out.extend_from_slice(&(self.answers.len() as u32).to_le_bytes());
            for (conn, answer) in &self.answers {
                out.extend_from_slice(&conn.to_le_bytes());
                put_len_prefixed(&mut out, answer);
            }
            out.extend_from_slice(&(self.asked.len() as u32).to_le_bytes());
            for point in &self.asked {
                out.extend_from_slice(&point.src.to_le_bytes());
                out.extend_from_slice(&point.dst.to_le_bytes());
                out.extend_from_slice(&point.conn.to_le_bytes());
                out.extend_from_slice(&point.event.to_le_bytes());
            }
            out
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError> {
            let mut offset = 0;
            let default_answer = take_len_prefixed(state, &mut offset)?.to_vec();
            let ans_count = take_u32(state, &mut offset)? as usize;
            let mut answers = BTreeMap::new();
            for _ in 0..ans_count {
                let conn = take_u64(state, &mut offset)?;
                let answer = take_len_prefixed(state, &mut offset)?.to_vec();
                answers.insert(conn, answer);
            }
            let ask_count = take_u32(state, &mut offset)? as usize;
            let mut asked = Vec::new();
            for _ in 0..ask_count {
                let src = take_u32(state, &mut offset)?;
                let dst = take_u32(state, &mut offset)?;
                let conn = take_u64(state, &mut offset)?;
                let event = take_u16(state, &mut offset)?;
                asked.push(NetFlowPoint {
                    src,
                    dst,
                    conn,
                    event,
                });
            }
            if offset != state.len() {
                return Err(ProtoError::BadState);
            }
            self.default_answer = default_answer;
            self.answers = answers;
            self.asked = asked;
            Ok(())
        }
    }

    /// Deterministic **reference** paravirt-clock registrar for loopback tests
    /// (task 110, [`ServiceId::Pvclock`]): validates the 8-byte little-endian
    /// GPA payload of a `pvclock_register` (op 1) against a fixed guest-RAM
    /// size and page alignment, records it, and answers the 4-byte ABI
    /// version. The production host is `vmm-core`'s doorbell dispatch, which
    /// additionally stamps the page and gates on its V-time wiring; this
    /// reference exists so the guest [`Client::pvclock_register`] verb and the
    /// frame shape are loopback-testable with no VM.
    pub struct PvclockRegistrar {
        ram_len: u64,
        abi_version: u32,
        registered: Option<u64>,
    }

    impl PvclockRegistrar {
        /// A registrar validating GPAs against `ram_len` bytes of guest RAM
        /// and answering `abi_version`.
        pub fn new(ram_len: u64, abi_version: u32) -> Self {
            Self {
                ram_len,
                abi_version,
                registered: None,
            }
        }

        /// The registered page GPA, if the guest has published one.
        pub fn registered(&self) -> Option<u64> {
            self.registered
        }
    }

    impl Service for PvclockRegistrar {
        fn handle(
            &mut self,
            opcode: u16,
            payload: &[u8],
            resp_payload: &mut [u8],
        ) -> (Status, usize) {
            if opcode != 1 {
                return (Status::UnknownOpcode, 0);
            }
            let Ok(gpa) = read_u64(payload, 0) else {
                return (Status::BadRequest, 0);
            };
            if payload.len() != 8 {
                return (Status::BadRequest, 0);
            }
            // One-shot (the frozen ABI, mirroring the production host): the
            // first accepted registration pins the target for the machine's
            // life; ANY second register — same GPA or not — is a guest fault,
            // rejected before the range check exactly as production orders it,
            // so loopback tests exercise the semantics real guests will hit.
            if self.registered.is_some() {
                return (Status::BadRequest, 0);
            }
            // Page-aligned and wholly inside guest RAM, else OutOfRange.
            if gpa % 4096 != 0 || gpa.checked_add(4096).is_none_or(|end| end > self.ram_len) {
                return (Status::OutOfRange, 0);
            }
            if resp_payload.len() < 4 {
                return (Status::Internal, 0);
            }
            self.registered = Some(gpa);
            resp_payload[..4].copy_from_slice(&self.abi_version.to_le_bytes());
            (Status::Ok, 4)
        }

        fn save_state(&self) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(&self.ram_len.to_le_bytes());
            out.extend_from_slice(&self.abi_version.to_le_bytes());
            match self.registered {
                Some(gpa) => {
                    out.push(1);
                    out.extend_from_slice(&gpa.to_le_bytes());
                }
                None => out.push(0),
            }
            out
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError> {
            let mut offset = 0;
            let ram_len = take_u64(state, &mut offset)?;
            let abi_version = take_u32(state, &mut offset)?;
            let tag = take_u8(state, &mut offset)?;
            let registered = match tag {
                0 => None,
                1 => Some(take_u64(state, &mut offset)?),
                _ => return Err(ProtoError::BadState),
            };
            if offset != state.len() {
                return Err(ProtoError::BadState);
            }
            self.ram_len = ram_len;
            self.abi_version = abi_version;
            self.registered = registered;
            Ok(())
        }
    }

    fn put_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }

    fn take_u8(state: &[u8], offset: &mut usize) -> Result<u8, ProtoError> {
        let v = *state.get(*offset).ok_or(ProtoError::BadState)?;
        *offset += 1;
        Ok(v)
    }

    fn take_u16(state: &[u8], offset: &mut usize) -> Result<u16, ProtoError> {
        let v = read_u16(state, *offset).map_err(|_| ProtoError::BadState)?;
        *offset += 2;
        Ok(v)
    }

    fn take_u32(state: &[u8], offset: &mut usize) -> Result<u32, ProtoError> {
        let v = read_u32(state, *offset).map_err(|_| ProtoError::BadState)?;
        *offset += 4;
        Ok(v)
    }

    fn take_u64(state: &[u8], offset: &mut usize) -> Result<u64, ProtoError> {
        let v = read_u64(state, *offset).map_err(|_| ProtoError::BadState)?;
        *offset += 8;
        Ok(v)
    }

    fn take_len_prefixed<'a>(state: &'a [u8], offset: &mut usize) -> Result<&'a [u8], ProtoError> {
        let len = take_u32(state, offset)? as usize;
        let bytes = state
            .get(*offset..*offset + len)
            .ok_or(ProtoError::BadState)?;
        *offset += len;
        Ok(bytes)
    }
}

#[cfg(feature = "host")]
pub use host::{
    ConsoleSink, Dispatcher, EventSink, MemBlockDevice, NetDecider, NetFlowPoint, PvclockRegistrar,
    SdkBuggify, SeededEntropy, Service,
};
