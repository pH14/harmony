// SPDX-License-Identifier: AGPL-3.0-or-later
#![no_std]
#![doc = "Deterministic guest/host hypercall wire protocol framing, guest client helpers, and host dispatch support for the Hypervizor VMM."]
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
//! | `Net` (reserved — task 61)      | 5 | — |
//! | [`Sdk`](ServiceId::Sdk)         | 6 | `1` = `buggify_decide` (round-trips a one-byte fire / no-fire answer) |
//!
//! Id **5** is reserved for task 61's `Net` vertical, so the task-73 SDK control
//! service ([`Sdk`](ServiceId::Sdk)) takes id **6**. An unregistered service id or
//! an opcode a service does not implement is a [`Status::UnknownService`] /
//! [`Status::UnknownOpcode`], never a silent drop.

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
    /// SDK control service (task 73): the guest asks the host to resolve a
    /// buggify decision (op 1, `buggify_decide`). Service id **5** is reserved
    /// for task 61's `Net` vertical, so the SDK takes **6**. Unlike the fire-and-
    /// forget [`Event`](ServiceId::Event) service, this one round-trips a
    /// one-byte answer (fire / don't fire); the host resolves it through its
    /// `Environment::decide` seam and records it at the surfacing `Moment`.
    Sdk = 6,
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
}

#[cfg(feature = "host")]
pub use host::{
    ConsoleSink, Dispatcher, EventSink, MemBlockDevice, SdkBuggify, SeededEntropy, Service,
};
