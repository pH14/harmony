// SPDX-License-Identifier: AGPL-3.0-or-later
#![no_std]

#[cfg(feature = "host")]
extern crate std;

#[cfg(feature = "host")]
use std::{boxed::Box, collections::BTreeMap, vec::Vec};

#[cfg(any(feature = "guest", not(feature = "host")))]
use core::fmt;

pub const MAX_FRAME: usize = 4096;
pub const HEADER_LEN: usize = 24;
pub const MAX_PAYLOAD: usize = MAX_FRAME - HEADER_LEN;
const MAGIC: u32 = 0x3150_4348;
const KIND_REQUEST: u16 = 1;
const KIND_RESPONSE: u16 = 2;
const SECTOR_SIZE: usize = 512;
const BLOCK_READ_MAX_SECTORS: usize = 7;

pub const SDK_COVERAGE_REQUEST_LEN: usize = 16;
pub const SDK_COVERAGE_RESPONSE_LEN: usize = 12;
pub const SDK_COVERAGE_QUANTUM: u64 = 1;
#[cfg(feature = "host")]
const ENTROPY_FALLBACK_SEED: u64 = 0x9E37_79B9_7F4A_7C15;
#[cfg(feature = "host")]
const ENTROPY_MUL: u64 = 0x2545_F491_4F6C_DD1D;

#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ServiceId {
    Console = 1,
    Entropy = 2,
    Block = 3,
    Event = 4,
    Net = 5,
    Sdk = 6,
    Pvclock = 7,
    Payload = 8,
}

impl ServiceId {
    fn as_u16(self) -> u16 {
        self as u16
    }
}

#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Ok = 0,
    BadRequest = 1,
    UnknownService = 2,
    UnknownOpcode = 3,
    OutOfRange = 4,
    Internal = 5,
}

impl Status {
    #[cfg(feature = "guest")]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameHeader {
    pub magic: u32,
    pub kind: u16,
    pub service: u16,
    pub opcode: u16,
    pub status: u16,
    pub seq: u32,
    pub payload_len: u32,
    pub reserved: u32,
}

#[cfg(feature = "host")]
impl FrameHeader {
    pub fn is_request(&self) -> bool {
        self.kind == KIND_REQUEST && self.status == 0 && self.reserved == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "host", derive(thiserror::Error))]
pub enum ProtoError {
    #[cfg_attr(feature = "host", error("buffer too small"))]
    BufferTooSmall,
    #[cfg_attr(feature = "host", error("payload too large"))]
    PayloadTooLarge,
    #[cfg_attr(feature = "host", error("truncated frame"))]
    Truncated,
    #[cfg_attr(feature = "host", error("bad magic"))]
    BadMagic,
    #[cfg_attr(feature = "host", error("invalid header"))]
    InvalidHeader,
    #[cfg_attr(feature = "host", error("bad payload"))]
    BadPayload,
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

pub fn encode_request(
    service: ServiceId,
    opcode: u16,
    seq: u32,
    payload: &[u8],
    buf: &mut [u8],
) -> Result<usize, ProtoError> {
    encode_frame(KIND_REQUEST, service.as_u16(), opcode, 0, seq, payload, buf)
}

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
    let end = offset.checked_add(2).ok_or(ProtoError::Truncated)?;
    let bytes = buf.get(offset..end).ok_or(ProtoError::Truncated)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(buf: &[u8], offset: usize) -> Result<u32, ProtoError> {
    let end = offset.checked_add(4).ok_or(ProtoError::Truncated)?;
    let bytes = buf.get(offset..end).ok_or(ProtoError::Truncated)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u64(buf: &[u8], offset: usize) -> Result<u64, ProtoError> {
    let end = offset.checked_add(8).ok_or(ProtoError::Truncated)?;
    let bytes = buf.get(offset..end).ok_or(ProtoError::Truncated)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

#[cfg(feature = "guest")]
mod guest {
    use super::*;

    pub trait Transport {
        type Error;
        fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Self::Error>;
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum ClientError<E> {
        Transport(E),
        Protocol(ProtoError),
        SeqMismatch,
        Status(Status),
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

    pub struct Client<T: Transport> {
        transport: T,
        seq: u32,
    }

    impl<T: Transport> Client<T> {
        pub fn new(transport: T) -> Self {
            Self { transport, seq: 1 }
        }

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

        pub fn block_capacity(&mut self) -> Result<u64, ClientError<T::Error>> {
            let mut out = [0_u8; 8];
            let copied = self.call_copy(ServiceId::Block, 1, &[], &mut out)?;
            if copied != 8 {
                return Err(ClientError::Protocol(ProtoError::BadPayload));
            }
            read_u64(&out, 0).map_err(ClientError::Protocol)
        }

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

        pub fn event_emit(&mut self, id: u32, data: &[u8]) -> Result<(), ClientError<T::Error>> {
            if data.len() > MAX_PAYLOAD - 4 {
                return Err(ClientError::InvalidLength);
            }
            let mut payload = [0_u8; MAX_PAYLOAD];
            put_u32(&mut payload[..4], id);
            payload[4..4 + data.len()].copy_from_slice(data);
            self.call_expect_empty(ServiceId::Event, 1, &payload[..4 + data.len()])
        }

        pub fn payload_fetch(&mut self, out: &mut [u8]) -> Result<(), ClientError<T::Error>> {
            if out.is_empty() || out.len() > MAX_PAYLOAD {
                return Err(ClientError::InvalidLength);
            }
            let mut payload = [0_u8; 4];
            put_u32(&mut payload, out.len() as u32);
            let copied = self.call_copy(ServiceId::Payload, 1, &payload, out)?;
            if copied != out.len() {
                return Err(ClientError::Protocol(ProtoError::BadPayload));
            }
            Ok(())
        }

        pub fn coverage_yield(
            &mut self,
            thread: u32,
            observed: u64,
            ready: u32,
        ) -> Result<(u64, u32), ClientError<T::Error>> {
            if ready == 0 || observed == 0 {
                return Err(ClientError::InvalidLength);
            }
            let mut payload = [0_u8; SDK_COVERAGE_REQUEST_LEN];
            put_u32(&mut payload[0..4], thread);
            put_u64(&mut payload[4..12], observed);
            put_u32(&mut payload[12..16], ready);
            let mut out = [0_u8; SDK_COVERAGE_RESPONSE_LEN];
            let copied = self.call_copy(ServiceId::Sdk, 2, &payload, &mut out)?;
            if copied != out.len() {
                return Err(ClientError::Protocol(ProtoError::BadPayload));
            }
            let next = read_u64(&out, 0).map_err(ClientError::Protocol)?;
            let selected = read_u32(&out, 8).map_err(ClientError::Protocol)?;
            if next <= observed || selected >= ready {
                return Err(ClientError::Protocol(ProtoError::BadPayload));
            }
            Ok((next, selected))
        }

        pub fn service_request(
            &mut self,
            namespace: u16,
            request_id: u64,
            request: &[u8],
            out: &mut [u8],
        ) -> Result<Option<usize>, ClientError<T::Error>> {
            if namespace <= 3 || request.len() > MAX_PAYLOAD - 10 {
                return Err(ClientError::InvalidLength);
            }
            let mut payload = [0_u8; MAX_PAYLOAD];
            put_u16(&mut payload[..2], namespace);
            put_u64(&mut payload[2..10], request_id);
            payload[10..10 + request.len()].copy_from_slice(request);
            let mut response = [0_u8; MAX_PAYLOAD];
            let len = self.call_copy(
                ServiceId::Sdk,
                3,
                &payload[..10 + request.len()],
                &mut response,
            )?;
            match response.get(..len) {
                Some([0]) => Ok(None),
                Some([1, bytes @ ..]) if bytes.len() <= out.len() => {
                    out[..bytes.len()].copy_from_slice(bytes);
                    Ok(Some(bytes.len()))
                }
                Some([1, ..]) => Err(ClientError::Protocol(ProtoError::BufferTooSmall)),
                _ => Err(ClientError::Protocol(ProtoError::BadPayload)),
            }
        }

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

    pub trait Service {
        fn handle(
            &mut self,
            opcode: u16,
            payload: &[u8],
            resp_payload: &mut [u8],
        ) -> (Status, usize);
        fn save_state(&self) -> Vec<u8>;
        fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError>;
    }

    pub struct Dispatcher {
        services: BTreeMap<u16, Box<dyn Service>>,
    }

    impl Default for Dispatcher {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Dispatcher {
        pub fn new() -> Self {
            Self {
                services: BTreeMap::new(),
            }
        }

        pub fn register(&mut self, id: ServiceId, svc: Box<dyn Service>) {
            let _old = self.services.insert(id.as_u16(), svc);
        }

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

        pub fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError> {
            let backup = self.save_state();
            self.try_restore(state).inspect_err(|_| {
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

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub struct ConsoleSink {
        bytes: Vec<u8>,
    }

    impl ConsoleSink {
        pub fn new() -> Self {
            Self { bytes: Vec::new() }
        }

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

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct SeededEntropy {
        state: u64,
    }

    impl SeededEntropy {
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

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct MemBlockDevice {
        data: Vec<u8>,
    }

    impl MemBlockDevice {
        pub fn new(data: Vec<u8>) -> Result<Self, ProtoError> {
            if !data.len().is_multiple_of(SECTOR_SIZE) {
                return Err(ProtoError::BadPayload);
            }
            Ok(Self { data })
        }

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

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub struct EventSink {
        events: Vec<(u32, Vec<u8>)>,
    }

    impl EventSink {
        pub fn new() -> Self {
            Self { events: Vec::new() }
        }

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

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub struct CoverageService {
        coverage_thresholds: BTreeMap<u32, u64>,
        coverage_asked: Vec<(u32, u64, u32, u32)>,
    }

    impl CoverageService {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn asked(&self) -> &[(u32, u64, u32, u32)] {
            &self.coverage_asked
        }

        fn handle_coverage(&mut self, payload: &[u8], resp_payload: &mut [u8]) -> (Status, usize) {
            if payload.len() != SDK_COVERAGE_REQUEST_LEN
                || resp_payload.len() < SDK_COVERAGE_RESPONSE_LEN
            {
                return (Status::BadRequest, 0);
            }
            let Ok(thread) = read_u32(payload, 0) else {
                return (Status::BadRequest, 0);
            };
            let Ok(observed) = read_u64(payload, 4) else {
                return (Status::BadRequest, 0);
            };
            let Ok(ready) = read_u32(payload, 12) else {
                return (Status::BadRequest, 0);
            };
            let expected = self
                .coverage_thresholds
                .get(&thread)
                .copied()
                .unwrap_or(SDK_COVERAGE_QUANTUM);
            let Some(next) = observed.checked_add(SDK_COVERAGE_QUANTUM) else {
                return (Status::OutOfRange, 0);
            };
            if ready == 0 || observed != expected {
                return (Status::BadRequest, 0);
            }
            let selected = ((u64::from(thread) ^ observed) % u64::from(ready)) as u32;
            self.coverage_thresholds.insert(thread, next);
            self.coverage_asked
                .push((thread, observed, ready, selected));
            put_u64(&mut resp_payload[0..8], next);
            put_u32(&mut resp_payload[8..12], selected);
            (Status::Ok, SDK_COVERAGE_RESPONSE_LEN)
        }
    }

    impl Service for CoverageService {
        fn handle(
            &mut self,
            opcode: u16,
            payload: &[u8],
            resp_payload: &mut [u8],
        ) -> (Status, usize) {
            if opcode == 2 {
                self.handle_coverage(payload, resp_payload)
            } else {
                (Status::UnknownOpcode, 0)
            }
        }

        fn save_state(&self) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(&(self.coverage_thresholds.len() as u32).to_le_bytes());
            for (thread, threshold) in &self.coverage_thresholds {
                out.extend_from_slice(&thread.to_le_bytes());
                out.extend_from_slice(&threshold.to_le_bytes());
            }
            out.extend_from_slice(&(self.coverage_asked.len() as u32).to_le_bytes());
            for (thread, observed, ready, selected) in &self.coverage_asked {
                out.extend_from_slice(&thread.to_le_bytes());
                out.extend_from_slice(&observed.to_le_bytes());
                out.extend_from_slice(&ready.to_le_bytes());
                out.extend_from_slice(&selected.to_le_bytes());
            }
            out
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError> {
            let mut offset = 0;
            let count = take_u32(state, &mut offset)? as usize;
            let mut thresholds = BTreeMap::new();
            for _ in 0..count {
                let thread = take_u32(state, &mut offset)?;
                let threshold = take_u64(state, &mut offset)?;
                if threshold == 0 || thresholds.insert(thread, threshold).is_some() {
                    return Err(ProtoError::BadState);
                }
            }
            let count = take_u32(state, &mut offset)? as usize;
            let mut asked = Vec::new();
            for _ in 0..count {
                let thread = take_u32(state, &mut offset)?;
                let observed = take_u64(state, &mut offset)?;
                let ready = take_u32(state, &mut offset)?;
                let selected = take_u32(state, &mut offset)?;
                if ready == 0 || selected >= ready {
                    return Err(ProtoError::BadState);
                }
                asked.push((thread, observed, ready, selected));
            }
            if offset != state.len() {
                return Err(ProtoError::BadState);
            }
            self.coverage_thresholds = thresholds;
            self.coverage_asked = asked;
            Ok(())
        }
    }

    pub struct PvclockRegistrar {
        ram_len: u64,
        abi_version: u32,
        registered: Option<u64>,
    }

    impl PvclockRegistrar {
        pub fn new(ram_len: u64, abi_version: u32) -> Self {
            Self {
                ram_len,
                abi_version,
                registered: None,
            }
        }

        pub fn registered(&self) -> Option<u64> {
            self.registered
        }

        fn gpa_fits(gpa: u64, ram_len: u64) -> bool {
            gpa.is_multiple_of(4096) && gpa.checked_add(4096).is_some_and(|end| end <= ram_len)
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
            if self.registered.is_some() {
                return (Status::BadRequest, 0);
            }
            if !Self::gpa_fits(gpa, self.ram_len) {
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
                1 => {
                    let gpa = take_u64(state, &mut offset)?;
                    if !Self::gpa_fits(gpa, ram_len) {
                        return Err(ProtoError::BadState);
                    }
                    Some(gpa)
                }
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

    fn take_u8(state: &[u8], offset: &mut usize) -> Result<u8, ProtoError> {
        let v = *state.get(*offset).ok_or(ProtoError::BadState)?;
        *offset += 1;
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
}

#[cfg(feature = "host")]
pub use host::{
    ConsoleSink, CoverageService, Dispatcher, EventSink, MemBlockDevice, PvclockRegistrar,
    SeededEntropy, Service,
};
