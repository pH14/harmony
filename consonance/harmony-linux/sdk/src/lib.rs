// SPDX-License-Identifier: AGPL-3.0-or-later
#![no_std]
#![doc = "The harmony guest SDK: assertions, IJON state registers, and lifecycle points a cooperating in-guest workload emits over the deterministic hypercall channel."]
pub mod wire;

use hypercall_proto::{Client, ClientError, MAX_PAYLOAD, Transport};

use core::fmt;

const CATALOG_BUF: usize = MAX_PAYLOAD - 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointKind {
    AssertAlways,
    AssertSometimes,
    AssertReachable,
    AssertUnreachable,
    StateReg,
    Buggify,
}

impl PointKind {
    const fn byte(self) -> u8 {
        match self {
            PointKind::AssertAlways => wire::KIND_ALWAYS,
            PointKind::AssertSometimes => wire::KIND_SOMETIMES,
            PointKind::AssertReachable => wire::KIND_REACHABLE,
            PointKind::AssertUnreachable => wire::KIND_UNREACHABLE,
            PointKind::StateReg => wire::KIND_STATE,
            PointKind::Buggify => wire::KIND_BUGGIFY,
        }
    }

    const fn namespace(self) -> u8 {
        match self {
            PointKind::AssertAlways
            | PointKind::AssertSometimes
            | PointKind::AssertReachable
            | PointKind::AssertUnreachable => wire::NS_ASSERT,
            PointKind::StateReg => wire::NS_STATE,
            PointKind::Buggify => wire::NS_BUGGIFY,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Point {
    pub id: u32,
    pub name: &'static str,
    pub kind: PointKind,
}

impl Point {
    pub const fn always(id: u32, name: &'static str) -> Self {
        Self {
            id,
            name,
            kind: PointKind::AssertAlways,
        }
    }
    pub const fn sometimes(id: u32, name: &'static str) -> Self {
        Self {
            id,
            name,
            kind: PointKind::AssertSometimes,
        }
    }
    pub const fn reachable(id: u32, name: &'static str) -> Self {
        Self {
            id,
            name,
            kind: PointKind::AssertReachable,
        }
    }
    pub const fn unreachable(id: u32, name: &'static str) -> Self {
        Self {
            id,
            name,
            kind: PointKind::AssertUnreachable,
        }
    }
    pub const fn state(id: u32, name: &'static str) -> Self {
        Self {
            id,
            name,
            kind: PointKind::StateReg,
        }
    }
    pub const fn buggify(id: u32, name: &'static str) -> Self {
        Self {
            id,
            name,
            kind: PointKind::Buggify,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SdkError<E> {
    Client(ClientError<E>),
    CatalogTooLarge,
    PointIdTooLarge,
    DuplicateCoordinate,
    DuplicateName,
}

impl<E: fmt::Debug> fmt::Display for SdkError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SdkError::Client(e) => write!(f, "sdk client error: {e}"),
            SdkError::CatalogTooLarge => f.write_str("declared catalog exceeds one event frame"),
            SdkError::PointIdTooLarge => f.write_str("point id exceeds the 24-bit local space"),
            SdkError::DuplicateCoordinate => {
                f.write_str("two declared points share a (namespace, id) coordinate")
            }
            SdkError::DuplicateName => f.write_str("two declared points share a name"),
        }
    }
}

impl<E> From<ClientError<E>> for SdkError<E> {
    fn from(e: ClientError<E>) -> Self {
        SdkError::Client(e)
    }
}

struct Cursor<'a> {
    buf: &'a mut [u8],
    pos: usize,
    overflow: bool,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            pos: 0,
            overflow: false,
        }
    }

    fn bytes(&mut self, b: &[u8]) {
        match self.buf.get_mut(self.pos..self.pos + b.len()) {
            Some(dst) => {
                dst.copy_from_slice(b);
                self.pos += b.len();
            }
            None => self.overflow = true,
        }
    }

    fn u8(&mut self, v: u8) {
        self.bytes(&[v]);
    }

    fn u16(&mut self, v: u16) {
        self.bytes(&v.to_le_bytes());
    }

    fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }

    fn finish(self) -> Option<usize> {
        if self.overflow { None } else { Some(self.pos) }
    }
}

pub struct Sdk<T: Transport> {
    client: Client<T>,
}

impl<T: Transport> Sdk<T> {
    pub fn init(transport: T, catalog: &[Point]) -> Result<Self, SdkError<T::Error>> {
        let mut sdk = Self {
            client: Client::new(transport),
        };
        sdk.declare(catalog)?;
        Ok(sdk)
    }

    fn declare(&mut self, catalog: &[Point]) -> Result<(), SdkError<T::Error>> {
        for (i, p) in catalog.iter().enumerate() {
            for q in &catalog[i + 1..] {
                if p.id == q.id && p.kind.namespace() == q.kind.namespace() {
                    return Err(SdkError::DuplicateCoordinate);
                }
                if p.name == q.name {
                    return Err(SdkError::DuplicateName);
                }
            }
        }
        let mut buf = [0_u8; CATALOG_BUF];
        let mut c = Cursor::new(&mut buf);
        c.u32(wire::CATALOG_MAGIC);
        c.u8(wire::SDK_WIRE_VERSION);
        c.u32(catalog.len() as u32);
        for p in catalog {
            if p.id > wire::LOCAL_MAX {
                return Err(SdkError::PointIdTooLarge);
            }
            let name = p.name.as_bytes();
            if name.len() > u16::MAX as usize {
                return Err(SdkError::CatalogTooLarge);
            }
            c.u8(p.kind.byte());
            c.u32(p.id);
            c.u16(name.len() as u16);
            c.bytes(name);
        }
        let len = c.finish().ok_or(SdkError::CatalogTooLarge)?;
        self.emit(wire::CATALOG_EVENT_ID, &buf[..len])
    }

    pub fn assert_always(&mut self, cond: bool, point: u32) -> Result<(), SdkError<T::Error>> {
        if cond {
            return Ok(());
        }
        self.emit_assert(point, wire::DISP_VIOLATION)
    }

    pub fn assert_sometimes(&mut self, cond: bool, point: u32) -> Result<(), SdkError<T::Error>> {
        if !cond {
            return Ok(());
        }
        self.emit_assert(point, wire::DISP_HIT)
    }

    pub fn assert_reachable(&mut self, point: u32) -> Result<(), SdkError<T::Error>> {
        self.emit_assert(point, wire::DISP_HIT)
    }

    pub fn assert_unreachable(&mut self, point: u32) -> Result<(), SdkError<T::Error>> {
        self.emit_assert(point, wire::DISP_VIOLATION)
    }

    pub fn state_set(&mut self, reg: u32, v: u64) -> Result<(), SdkError<T::Error>> {
        self.emit_state(reg, wire::STATE_SET, v)
    }

    pub fn state_max(&mut self, reg: u32, v: u64) -> Result<(), SdkError<T::Error>> {
        self.emit_state(reg, wire::STATE_MAX, v)
    }

    pub fn setup_complete(&mut self) -> Result<(), SdkError<T::Error>> {
        self.emit(wire::SETUP_COMPLETE_EVENT_ID, &[])
    }

    pub fn frame_complete(&mut self, frame_count: u64) -> Result<(), SdkError<T::Error>> {
        self.emit(wire::FRAME_COMPLETE_EVENT_ID, &frame_count.to_le_bytes())
    }

    pub fn coverage_yield(
        &mut self,
        thread: u32,
        observed: u64,
        ready: u32,
    ) -> Result<(u64, u32), SdkError<T::Error>> {
        self.client
            .coverage_yield(thread, observed, ready)
            .map_err(SdkError::Client)
    }

    pub fn entropy_fill(&mut self, out: &mut [u8]) -> Result<(), SdkError<T::Error>> {
        self.client.entropy_fill(out).map_err(SdkError::Client)
    }

    pub fn client_mut(&mut self) -> &mut Client<T> {
        &mut self.client
    }

    fn emit_assert(&mut self, point: u32, disposition: u8) -> Result<(), SdkError<T::Error>> {
        if point > wire::LOCAL_MAX {
            return Err(SdkError::PointIdTooLarge);
        }
        let buf = [disposition, 0, 0];
        self.emit(wire::event_id(wire::NS_ASSERT, point), &buf)
    }

    fn emit_state(&mut self, reg: u32, op: u8, value: u64) -> Result<(), SdkError<T::Error>> {
        if reg > wire::LOCAL_MAX {
            return Err(SdkError::PointIdTooLarge);
        }
        let mut buf = [0_u8; 9];
        buf[0] = op;
        buf[1..9].copy_from_slice(&value.to_le_bytes());
        self.emit(wire::event_id(wire::NS_STATE, reg), &buf)
    }

    fn emit(&mut self, id: u32, data: &[u8]) -> Result<(), SdkError<T::Error>> {
        self.client.event_emit(id, data).map_err(SdkError::Client)
    }
}
