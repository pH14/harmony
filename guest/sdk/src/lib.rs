// SPDX-License-Identifier: AGPL-3.0-or-later
#![no_std]
#![doc = "The harmony guest SDK (task 73): assertions, IJON state registers, buggify decisions, and lifecycle points a cooperating in-guest workload emits over the deterministic hypercall channel."]
//!
//! # The thin-SDK ruling (load-bearing)
//!
//! The SDK is **hooks + transport only** (Paul, 2026-07-01): it contributes
//! *identity and observation* — named points, their firings, numeric state — and
//! the **host owns every interpretation**. There are no checkers and no policy in
//! the guest; Elle/history checkers live at the evaluator layer (task 75). So:
//!
//! - **`assert_always`** emits only on **violation**; the host turns the
//!   violation into `StopReason::Assertion`.
//! - **`assert_sometimes`** emits on **every hit** — features are a timestamped
//!   stream (task 64), not a terminal set.
//! - **`assert_reachable`** / **`assert_unreachable`** are the reached/must-not-
//!   reach duals: a reached `unreachable` is a violation.
//! - **`state_set` / `state_max`** are the IJON numeric registers (S&P 2020): the
//!   guest reports the raw `(reg, op, value)`; the host interprets max-novelty.
//! - **`buggify(point)`** asks the host to resolve a deliberate perturbation
//!   ([FoundationDB BUGGIFY], minus the anonymity — the point is a *named,
//!   steerable, auditable* coordinate) and records the result on the event stream.
//! - **`setup_complete`** is the lifecycle hook the host turns into
//!   `StopReason::SnapshotPoint`.
//!
//! The SDK never times anything — the **host** stamps each emission at the
//! `Moment` it surfaces. Guest randomness is **not** an SDK primitive: the
//! Entropy hypercall (`Client::entropy_fill`, host `SeededEntropy`) is the single
//! seeded source, re-exported as [`Sdk::entropy_fill`].
//!
//! # Form
//!
//! A `no_std`, `alloc`-free crate generic over `hypercall_proto::Transport`, so
//! `Sdk<Client-over-VmcallTransport>` composes with the purpose-built guest
//! doorbell shim with zero new transport code. Every emission rides the existing
//! Event service (`ServiceId::Event`, op 1) under the byte-deterministic,
//! versioned payload convention in [`wire`]; the round-trip `buggify` verb rides
//! the SDK control service (`ServiceId::Sdk`, op 1). Task 74's OTel bridge reuses
//! these same transport conventions (a reserved event-id namespace).
//!
//! [FoundationDB BUGGIFY]: https://www.youtube.com/watch?v=4fFDFbi3toc

pub mod wire;

use hypercall_proto::{Client, ClientError, MAX_PAYLOAD, Transport};

use core::fmt;

/// The scratch buffer the one-shot catalog declaration is marshalled into. Sized
/// to the largest payload one Event frame carries; a catalog that overflows it is
/// reported as [`SdkError::CatalogTooLarge`], never truncated. Only
/// [`Sdk::init`] uses a buffer this large, and it runs once at guest startup.
const CATALOG_BUF: usize = MAX_PAYLOAD - 4;

/// The declared kind of an SDK [`Point`] — its role in the catalog. The kind
/// selects the runtime event-id namespace ([`wire`]) and lets the host-side
/// never-fired report be sliced by role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointKind {
    /// An `assert_always`: it must hold on every pass; a violation is a bug.
    AssertAlways,
    /// An `assert_sometimes`: at least one satisfied hit is expected across the
    /// campaign; a declared-but-never-hit point is a coverage gap.
    AssertSometimes,
    /// An `assert_reachable`: this point should be reached at least once.
    AssertReachable,
    /// An `assert_unreachable`: reaching this point is a bug.
    AssertUnreachable,
    /// An IJON numeric state register.
    StateReg,
    /// A buggify site.
    Buggify,
}

impl PointKind {
    /// The catalog wire byte for this kind.
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
}

/// One declared point: a **stable id**, a human name, and a [`PointKind`]. The id
/// is the guest-owned identity that fires at runtime; the name is the stable key
/// the host-side catalog keys its never-fired report on. Ids must fit
/// [`wire::LOCAL_MAX`] (24 bits) and be unique within their kind's namespace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Point {
    /// The stable site id (unique within the kind's namespace, `<= LOCAL_MAX`).
    pub id: u32,
    /// The human-readable name — the catalog's stable report key.
    pub name: &'static str,
    /// The declared kind.
    pub kind: PointKind,
}

impl Point {
    /// An `assert_always` point.
    pub const fn always(id: u32, name: &'static str) -> Self {
        Self {
            id,
            name,
            kind: PointKind::AssertAlways,
        }
    }
    /// An `assert_sometimes` point.
    pub const fn sometimes(id: u32, name: &'static str) -> Self {
        Self {
            id,
            name,
            kind: PointKind::AssertSometimes,
        }
    }
    /// An `assert_reachable` point.
    pub const fn reachable(id: u32, name: &'static str) -> Self {
        Self {
            id,
            name,
            kind: PointKind::AssertReachable,
        }
    }
    /// An `assert_unreachable` point.
    pub const fn unreachable(id: u32, name: &'static str) -> Self {
        Self {
            id,
            name,
            kind: PointKind::AssertUnreachable,
        }
    }
    /// An IJON state register.
    pub const fn state(id: u32, name: &'static str) -> Self {
        Self {
            id,
            name,
            kind: PointKind::StateReg,
        }
    }
    /// A buggify site.
    pub const fn buggify(id: u32, name: &'static str) -> Self {
        Self {
            id,
            name,
            kind: PointKind::Buggify,
        }
    }
}

/// An SDK error. Wraps the underlying [`ClientError`] and adds the SDK-local
/// framing failures. Total and panic-free: a too-large catalog or an out-of-range
/// id is a typed error, never a truncation or a panic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SdkError<E> {
    /// The underlying hypercall client failed.
    Client(ClientError<E>),
    /// The declared catalog does not fit one Event frame.
    CatalogTooLarge,
    /// A point/register id exceeds [`wire::LOCAL_MAX`] (the 24-bit local space).
    PointIdTooLarge,
}

impl<E: fmt::Debug> fmt::Display for SdkError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SdkError::Client(e) => write!(f, "sdk client error: {e}"),
            SdkError::CatalogTooLarge => f.write_str("declared catalog exceeds one event frame"),
            SdkError::PointIdTooLarge => f.write_str("point id exceeds the 24-bit local space"),
        }
    }
}

impl<E> From<ClientError<E>> for SdkError<E> {
    fn from(e: ClientError<E>) -> Self {
        SdkError::Client(e)
    }
}

/// A forward-only, panic-free byte writer over a fixed buffer. A write past the
/// end sets the overflow flag and is dropped; [`finish`](Cursor::finish) then
/// returns `None`. All integers are little-endian ([`wire`] convention).
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

/// The guest SDK handle: a thin wrapper over a `hypercall_proto::Client` that
/// speaks the [`wire`] convention. Construct it with [`init`](Sdk::init), which
/// declares the point catalog in one Event, then call the verbs.
pub struct Sdk<T: Transport> {
    client: Client<T>,
}

impl<T: Transport> Sdk<T> {
    /// Build the SDK over `transport` and **register the declared point set** in
    /// one catalog-declaration Event (each point = stable id + name + kind). The
    /// host folds this declaration into its catalog so a never-hit point is
    /// detectable (the never-fired report).
    ///
    /// Fails with [`SdkError::CatalogTooLarge`] if the catalog does not fit one
    /// frame, or [`SdkError::PointIdTooLarge`] if any id exceeds the 24-bit local
    /// space.
    pub fn init(transport: T, catalog: &[Point]) -> Result<Self, SdkError<T::Error>> {
        let mut sdk = Self {
            client: Client::new(transport),
        };
        sdk.declare(catalog)?;
        Ok(sdk)
    }

    /// Marshal and emit the catalog-declaration Event.
    fn declare(&mut self, catalog: &[Point]) -> Result<(), SdkError<T::Error>> {
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

    /// `assert_always(cond, point)`: emit **only on violation** (`!cond`). The
    /// host surfaces a violation as `StopReason::Assertion`.
    pub fn assert_always(&mut self, cond: bool, point: u32) -> Result<(), SdkError<T::Error>> {
        if cond {
            return Ok(());
        }
        self.emit_assert(point, wire::DISP_VIOLATION)
    }

    /// `assert_sometimes(cond, point)`: emit a **hit on every satisfied pass**
    /// (`cond`). Each hit is a timestamped feature (task 64); the never-fired
    /// report flags a `sometimes` point that never hit.
    pub fn assert_sometimes(&mut self, cond: bool, point: u32) -> Result<(), SdkError<T::Error>> {
        if !cond {
            return Ok(());
        }
        self.emit_assert(point, wire::DISP_HIT)
    }

    /// `assert_reachable(point)`: emit a **hit** — this point was reached (a
    /// positive signal; a never-reached `reachable` point is a coverage gap).
    pub fn assert_reachable(&mut self, point: u32) -> Result<(), SdkError<T::Error>> {
        self.emit_assert(point, wire::DISP_HIT)
    }

    /// `assert_unreachable(point)`: emit a **violation** — reaching this point is
    /// a bug, so the host surfaces `StopReason::Assertion`.
    pub fn assert_unreachable(&mut self, point: u32) -> Result<(), SdkError<T::Error>> {
        self.emit_assert(point, wire::DISP_VIOLATION)
    }

    /// `state_set(reg, v)`: report the IJON register `reg` was assigned `v`. The
    /// guest reports the raw value + op; the host interprets novelty.
    pub fn state_set(&mut self, reg: u32, v: u64) -> Result<(), SdkError<T::Error>> {
        self.emit_state(reg, wire::STATE_SET, v)
    }

    /// `state_max(reg, v)`: report `v` as a candidate maximum for register `reg`.
    /// The host tracks the running max and the novelty of a new one; the guest
    /// stays thin (no max tracking, per the thin-SDK ruling).
    pub fn state_max(&mut self, reg: u32, v: u64) -> Result<(), SdkError<T::Error>> {
        self.emit_state(reg, wire::STATE_MAX, v)
    }

    /// `setup_complete()`: the lifecycle hook. The host surfaces
    /// `StopReason::SnapshotPoint` here so the campaign can seal the boot/setup
    /// prefix and fork from it.
    pub fn setup_complete(&mut self) -> Result<(), SdkError<T::Error>> {
        self.emit(wire::SETUP_COMPLETE_EVENT_ID, &[])
    }

    /// `buggify(point) -> bool`: ask the host whether to fire the deliberate
    /// perturbation at `point`, then **record the result** on the event stream
    /// (so the link tier observes reached-and-fired vs reached-and-nominal, and
    /// the catalog can flag a never-reached buggify point). Returns whether the
    /// host decided to fire.
    pub fn buggify(&mut self, point: u32) -> Result<bool, SdkError<T::Error>> {
        if point > wire::LOCAL_MAX {
            return Err(SdkError::PointIdTooLarge);
        }
        let fired = self.client.buggify_decide(point)?;
        self.emit(wire::event_id(wire::NS_BUGGIFY, point), &[u8::from(fired)])?;
        Ok(fired)
    }

    /// Fill `out` with deterministic guest entropy. **Not a new primitive** — it
    /// forwards to `Client::entropy_fill` (the host `SeededEntropy` stream), the
    /// project's single guest-random source, cited here so a workload holding
    /// only an [`Sdk`] handle still has determinized randomness.
    pub fn entropy_fill(&mut self, out: &mut [u8]) -> Result<(), SdkError<T::Error>> {
        self.client.entropy_fill(out).map_err(SdkError::Client)
    }

    /// Mutable access to the underlying hypercall client — the escape hatch for a
    /// workload that also needs the console/block/entropy services directly.
    pub fn client_mut(&mut self) -> &mut Client<T> {
        &mut self.client
    }

    /// Emit one assertion event `[disposition, detail_len=0, ...]` for `point`.
    fn emit_assert(&mut self, point: u32, disposition: u8) -> Result<(), SdkError<T::Error>> {
        if point > wire::LOCAL_MAX {
            return Err(SdkError::PointIdTooLarge);
        }
        // `[disposition u8][detail_len u16 = 0]` — detail is reserved for a future
        // message-carrying variant; the point id is the assertion identity today.
        let buf = [disposition, 0, 0];
        self.emit(wire::event_id(wire::NS_ASSERT, point), &buf)
    }

    /// Emit one state event `[op, value]` for register `reg`.
    fn emit_state(&mut self, reg: u32, op: u8, value: u64) -> Result<(), SdkError<T::Error>> {
        if reg > wire::LOCAL_MAX {
            return Err(SdkError::PointIdTooLarge);
        }
        let mut buf = [0_u8; 9];
        buf[0] = op;
        buf[1..9].copy_from_slice(&value.to_le_bytes());
        self.emit(wire::event_id(wire::NS_STATE, reg), &buf)
    }

    /// One Event emission through the client.
    fn emit(&mut self, id: u32, data: &[u8]) -> Result<(), SdkError<T::Error>> {
        self.client.event_emit(id, data).map_err(SdkError::Client)
    }
}
