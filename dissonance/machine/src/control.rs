// SPDX-License-Identifier: AGPL-3.0-or-later

//! Control-protocol implementation of [`crate::Machine`].
//!
//! [`SocketMachine`] is deliberately a thin, synchronous adapter: every trait
//! call emits exactly one request and consumes exactly one sequence-matched
//! reply. The consonance types remain authoritative on the wire; this module
//! converts them to the searcher's local mirror only at the boundary.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::{Read, Write},
};

use control_proto::{CoverageGeometry, HashScope, Reply, Request};

use crate::{
    Answer, CrashInfo, CrashKind, DecisionId, EventRef, Machine, MachineError, Moment, Reproducer,
    SnapId, StopConditions, StopReason, class_bit,
};

/// SDK lifecycle `frame_complete` event id, mirrored from
/// `harmony-linux/sdk/src/wire.rs`. Its payload is one little-endian `u64`
/// cumulative emulated-frame count.
const FRAME_COMPLETE_EVENT_ID: u32 = (4_u32 << 24) | 1;
/// TetaNES guest state-register event ids, mirrored from the guest agent. A
/// state-set payload is `[op=0, value_le_u64]`.
const WRAM_GPA_EVENT_ID: u32 = (2_u32 << 24) | 1;
const WRAM_LEN_EVENT_ID: u32 = (2_u32 << 24) | 2;
const STATE_SET: u8 = 0;

/// The capabilities a search client requires from the M2 control server.
#[must_use]
pub fn client_caps() -> control_proto::Caps {
    control_proto::Caps {
        protocol_version: control_proto::APP_PROTOCOL_VERSION,
        env_version_min: environment::EnvSpec::BLOB_VERSION,
        env_version_max: environment::EnvSpec::BLOB_VERSION,
        coverage: CoverageGeometry {
            map_bytes: 0,
            producer: 0,
        },
        flags: control_proto::CapFlags::GUEST_HAS_SDK,
    }
}

/// The evidence cut returned atomically with one snapshot handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotCut {
    /// Exact V-time of the sealed state.
    pub at: Moment,
    /// Guest-reported cumulative emulated-frame count at the seal.
    pub frame: Moment,
    /// SDK-event prefix length included in the seal.
    pub sdk_events: u64,
    /// Whether the timeline is off-record because of improvisation.
    pub tainted: bool,
}

/// Restore accounting used by M2's causal-load-bearing report.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RestoreCounters {
    /// Successful branches/replays from a marked gameplay-genesis snapshot.
    pub genesis: u64,
    /// Successful branches/replays from a non-genesis continuation snapshot.
    pub continuation: u64,
}

struct Connection<S> {
    stream: S,
    sequence: u32,
    input: Vec<u8>,
}

impl<S: Read + Write> Connection<S> {
    fn request(&mut self, request: &Request) -> Result<Reply, MachineError> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| MachineError::Backend("control request sequence exhausted".into()))?;
        let sequence = self.sequence;
        let mut output = Vec::new();
        control_proto::encode_request(sequence, request, &mut output)
            .map_err(|error| MachineError::Backend(error.to_string()))?;
        self.stream
            .write_all(&output)
            .and_then(|()| self.stream.flush())
            .map_err(|error| MachineError::Backend(error.to_string()))?;

        let mut chunk = [0_u8; 4096];
        loop {
            if let Some((reply_sequence, reply, consumed)) =
                control_proto::decode_reply(&self.input)
                    .map_err(|error| MachineError::Backend(error.to_string()))?
            {
                if reply_sequence != sequence {
                    return Err(MachineError::Backend(format!(
                        "control reply sequence mismatch: expected {sequence}, got {reply_sequence}"
                    )));
                }
                self.input.drain(..consumed);
                return reply.map_err(map_control_error);
            }
            let read = self
                .stream
                .read(&mut chunk)
                .map_err(|error| MachineError::Backend(error.to_string()))?;
            if read == 0 {
                return Err(MachineError::Backend(
                    "control server closed before a complete reply".into(),
                ));
            }
            self.input.extend_from_slice(&chunk[..read]);
            // `decode_reply` rejects an advertised over-cap body as soon as its
            // header is present. This independent cap also bounds a peer that
            // never supplies enough header bytes to become decodable.
            if self.input.len() > control_proto::MAX_FRAME_LEN + 4096 {
                return Err(MachineError::Backend(
                    "control reply exceeded the protocol frame cap".into(),
                ));
            }
        }
    }
}

/// A synchronous control-protocol machine over any byte stream.
///
/// The stream lives behind a `RefCell` solely because [`Machine::read`] is a
/// logically pure observation and therefore takes `&self`, while a socket read
/// necessarily advances transport buffers. No VM state is hidden in the cell.
pub struct SocketMachine<S> {
    connection: RefCell<Connection<S>>,
    cuts: BTreeMap<u64, SnapshotCut>,
    genesis: Option<SnapId>,
    restores: RestoreCounters,
    sdk_cursor: u32,
    logical_frame: Moment,
    wram_gpa: Option<u64>,
    wram_len: Option<u32>,
}

impl<S: Read + Write> SocketMachine<S> {
    /// Negotiate a control session over an already-connected stream.
    ///
    /// # Errors
    ///
    /// Returns a loud backend error for transport/framing failure, a rejected
    /// hello, or a server capability set other than the exact required set.
    pub fn from_stream(stream: S) -> Result<Self, MachineError> {
        let mut machine = Self {
            connection: RefCell::new(Connection {
                stream,
                sequence: 0,
                input: Vec::new(),
            }),
            cuts: BTreeMap::new(),
            genesis: None,
            restores: RestoreCounters::default(),
            sdk_cursor: 0,
            logical_frame: Moment(0),
            wram_gpa: None,
            wram_len: None,
        };
        let expected = client_caps();
        match machine.request(&Request::Hello(expected))? {
            Reply::Hello(actual) if actual == expected => Ok(machine),
            Reply::Hello(actual) => Err(MachineError::Backend(format!(
                "control capability mismatch: expected {expected:?}, got {actual:?}"
            ))),
            reply => Err(unexpected("Hello", &reply)),
        }
    }

    fn request(&mut self, request: &Request) -> Result<Reply, MachineError> {
        self.connection.get_mut().request(request)
    }

    fn observe(&self, request: &Request) -> Result<Reply, MachineError> {
        self.connection.borrow_mut().request(request)
    }

    /// Mark the snapshot that represents gameplay genesis. Successful restores
    /// from it are counted separately from continuation restores.
    pub fn mark_genesis(&mut self, snap: SnapId) -> Result<(), MachineError> {
        if !self.cuts.contains_key(&snap.0) {
            return Err(MachineError::UnknownSnapshot);
        }
        self.genesis = Some(snap);
        Ok(())
    }

    /// Restore counters accumulated by this session.
    #[must_use]
    pub fn restore_counters(&self) -> RestoreCounters {
        self.restores
    }

    /// The guest's last validated cumulative emulated-frame report.
    #[must_use]
    pub fn logical_frame(&self) -> Moment {
        self.logical_frame
    }

    /// The exact host-readable WRAM window published by the guest, once both
    /// state registers have been observed.
    #[must_use]
    pub fn wram_window(&self) -> Option<(u64, u32)> {
        self.wram_gpa.zip(self.wram_len)
    }

    /// The atomic evidence cut carried with `snap`, when it is still held.
    #[must_use]
    pub fn snapshot_cut(&self, snap: SnapId) -> Option<SnapshotCut> {
        self.cuts.get(&snap.0).copied()
    }

    /// Register an out-of-band imported snapshot and its source-authenticated
    /// evidence cut. Portable artifacts are loaded by the control-server
    /// composition root before this client connects, so the ordinary wire
    /// protocol has no snapshot reply from which to learn the cut. Registration
    /// restores exactly that missing session-local bookkeeping; `replay` then
    /// uses the normal control verb and validation path.
    pub fn register_imported_snapshot(
        &mut self,
        snap: SnapId,
        cut: SnapshotCut,
    ) -> Result<(), MachineError> {
        u32::try_from(cut.sdk_events).map_err(|_| {
            MachineError::Backend("snapshot SDK event cut exceeds protocol cursor".into())
        })?;
        if self.cuts.insert(snap.0, cut).is_some() {
            return Err(MachineError::Backend(
                "imported snapshot handle is already registered".into(),
            ));
        }
        Ok(())
    }

    /// Read the server's canonical whole-state hash.
    ///
    /// # Errors
    ///
    /// Returns an error for a transport/control failure or a wrong reply shape.
    pub fn state_hash(&self) -> Result<[u8; 32], MachineError> {
        match self.observe(&Request::Hash {
            scope: HashScope::Whole,
        })? {
            Reply::Hash(hash) => Ok(hash),
            reply => Err(unexpected("Hash", &reply)),
        }
    }

    /// Page the SDK capture from `offset` until the server returns an empty
    /// page. The returned vector retains server order.
    ///
    /// # Errors
    ///
    /// Returns an error on transport/control failure, a wrong reply shape, or
    /// an event count that cannot be represented by the protocol's `u32` cursor.
    pub fn sdk_events_from(
        &self,
        mut offset: u32,
    ) -> Result<Vec<(u64, u32, Vec<u8>)>, MachineError> {
        let mut events = Vec::new();
        loop {
            let page = match self.observe(&Request::SdkEvents { offset })? {
                Reply::SdkEvents(page) => page,
                reply => return Err(unexpected("SdkEvents", &reply)),
            };
            if page.is_empty() {
                return Ok(events);
            }
            let added = u32::try_from(page.len())
                .map_err(|_| MachineError::Backend("SDK event page is too large".into()))?;
            offset = offset
                .checked_add(added)
                .ok_or_else(|| MachineError::Backend("SDK event cursor overflow".into()))?;
            events.extend(page);
        }
    }

    /// Drain the SDK capture once, validate every frame-clock event, and advance
    /// this adapter's logical clock. The VMM validates the same width at the
    /// doorbell; doing it independently here catches a malformed or dishonest
    /// control peer instead of allowing host V-time to leak into game time.
    fn sync_frame_clock(&mut self) -> Result<(), MachineError> {
        let events = self.sdk_events_from(self.sdk_cursor)?;
        let added = u32::try_from(events.len())
            .map_err(|_| MachineError::Backend("SDK event page is too large".into()))?;
        for (_, id, data) in &events {
            match *id {
                FRAME_COMPLETE_EVENT_ID => {
                    let bytes: [u8; 8] = data.as_slice().try_into().map_err(|_| {
                        MachineError::Backend(
                            "control peer returned malformed frame_complete payload".into(),
                        )
                    })?;
                    let frame = Moment(u64::from_le_bytes(bytes));
                    if frame <= self.logical_frame {
                        return Err(MachineError::Backend(format!(
                            "non-increasing guest frame clock: previous {}, next {}",
                            self.logical_frame.0, frame.0
                        )));
                    }
                    self.logical_frame = frame;
                }
                WRAM_GPA_EVENT_ID => {
                    let value = decode_state_set("WRAM GPA", data)?;
                    set_once("WRAM GPA", &mut self.wram_gpa, value)?;
                }
                WRAM_LEN_EVENT_ID => {
                    let value = decode_state_set("WRAM length", data)?;
                    let value = u32::try_from(value).map_err(|_| {
                        MachineError::Backend("guest WRAM length exceeds u32".into())
                    })?;
                    set_once("WRAM length", &mut self.wram_len, value)?;
                }
                _ => {}
            }
        }
        self.sdk_cursor = self
            .sdk_cursor
            .checked_add(added)
            .ok_or_else(|| MachineError::Backend("SDK event cursor overflow".into()))?;
        Ok(())
    }

    fn restore_cut_clock(&mut self, snap: SnapId) -> Result<(), MachineError> {
        let cut = self
            .cuts
            .get(&snap.0)
            .copied()
            .ok_or(MachineError::UnknownSnapshot)?;
        self.sdk_cursor = u32::try_from(cut.sdk_events).map_err(|_| {
            MachineError::Backend("snapshot SDK event cut exceeds protocol cursor".into())
        })?;
        self.logical_frame = cut.frame;
        Ok(())
    }

    fn count_restore(&mut self, snap: SnapId) {
        if self.genesis == Some(snap) {
            self.restores.genesis = self.restores.genesis.saturating_add(1);
        } else {
            self.restores.continuation = self.restores.continuation.saturating_add(1);
        }
    }
}

fn decode_state_set(name: &str, data: &[u8]) -> Result<u64, MachineError> {
    let Some((&op, value)) = data.split_first() else {
        return Err(MachineError::Backend(format!(
            "guest {name} publication is empty"
        )));
    };
    let bytes: [u8; 8] = value.try_into().map_err(|_| {
        MachineError::Backend(format!("guest {name} publication has malformed width"))
    })?;
    if op != STATE_SET {
        return Err(MachineError::Backend(format!(
            "guest {name} publication is not state_set"
        )));
    }
    Ok(u64::from_le_bytes(bytes))
}

fn set_once<T: Copy + Eq + std::fmt::Display>(
    name: &str,
    slot: &mut Option<T>,
    value: T,
) -> Result<(), MachineError> {
    match *slot {
        None => {
            *slot = Some(value);
            Ok(())
        }
        Some(previous) if previous == value => Ok(()),
        Some(previous) => Err(MachineError::Backend(format!(
            "guest republished {name}: previous {previous}, next {value}"
        ))),
    }
}

#[cfg(unix)]
impl SocketMachine<std::os::unix::net::UnixStream> {
    /// Connect to a consonance Unix-domain control socket and negotiate hello.
    ///
    /// # Errors
    ///
    /// Returns a transport or negotiation failure.
    pub fn connect(path: impl AsRef<std::path::Path>) -> Result<Self, MachineError> {
        let stream = std::os::unix::net::UnixStream::connect(path)
            .map_err(|error| MachineError::Backend(error.to_string()))?;
        Self::from_stream(stream)
    }
}

impl<S: Read + Write> Machine for SocketMachine<S> {
    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        self.sync_frame_clock()?;
        match self.request(&Request::Snapshot)? {
            Reply::Snapshot {
                id,
                at,
                sdk_events,
                tainted,
            } => {
                if sdk_events != u64::from(self.sdk_cursor) {
                    return Err(MachineError::Backend(format!(
                        "snapshot SDK cut mismatch: drained {}, sealed {sdk_events}",
                        self.sdk_cursor
                    )));
                }
                let id = SnapId(id.0);
                self.cuts.insert(
                    id.0,
                    SnapshotCut {
                        at: Moment(at.0),
                        frame: self.logical_frame,
                        sdk_events,
                        tainted,
                    },
                );
                Ok(id)
            }
            reply => Err(unexpected("Snapshot", &reply)),
        }
    }

    fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
        match self.request(&Request::Drop(control_proto::SnapId(snap.0)))? {
            Reply::Unit => {
                self.cuts.remove(&snap.0);
                if self.genesis == Some(snap) {
                    self.genesis = None;
                }
                Ok(())
            }
            reply => Err(unexpected("Drop", &reply)),
        }
    }

    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
        let request = Request::Branch {
            snap: control_proto::SnapId(snap.0),
            env: control_proto::Reproducer {
                blob_version: env.blob_version,
                bytes: env.bytes.clone(),
            },
        };
        match self.request(&request)? {
            Reply::Unit => {
                self.restore_cut_clock(snap)?;
                self.count_restore(snap);
                Ok(())
            }
            reply => Err(unexpected("Branch", &reply)),
        }
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        match self.request(&Request::Replay(control_proto::SnapId(snap.0)))? {
            Reply::Unit => {
                self.restore_cut_clock(snap)?;
                self.count_restore(snap);
                Ok(())
            }
            reply => Err(unexpected("Replay", &reply)),
        }
    }

    fn run(
        &mut self,
        until: StopConditions,
        resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        if resolve.is_none()
            && let Some(deadline) = until.deadline
            && self.logical_frame >= deadline
        {
            return Ok(StopReason::Deadline {
                vtime: self.logical_frame,
            });
        }

        let caller_armed_snapshot = until.on.armed(class_bit::SNAPSHOT_POINT);
        let on = until.on.arm(class_bit::SNAPSHOT_POINT);
        let mut resolve = resolve.map(|answer| control_proto::Answer(answer.0.clone()));
        loop {
            // The server's deadline is V-time nanoseconds. M2 game deadlines are
            // guest-reported frames, so the adapter deliberately sends no wire
            // deadline and advances one lifecycle yield at a time.
            let request = Request::Run {
                until: control_proto::StopConditions {
                    deadline: None,
                    on: control_proto::StopMask(on.0),
                },
                resolve: resolve.take(),
            };
            let reason = match self.request(&request)? {
                Reply::Stop(reason) => reason,
                reply => return Err(unexpected("Run", &reply)),
            };
            match reason {
                control_proto::StopReason::SnapshotPoint { .. } => {
                    self.sync_frame_clock()?;
                    if until
                        .deadline
                        .is_some_and(|deadline| self.logical_frame >= deadline)
                    {
                        return Ok(StopReason::Deadline {
                            vtime: self.logical_frame,
                        });
                    }
                    if caller_armed_snapshot {
                        return Ok(StopReason::SnapshotPoint {
                            vtime: self.logical_frame,
                        });
                    }
                    // An unarmed caller does not observe lifecycle yields. Keep
                    // running until its logical deadline or a terminal stop.
                }
                other => {
                    self.sync_frame_clock()?;
                    return Ok(map_stop_at(other, self.logical_frame));
                }
            }
        }
    }

    fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
        match self.observe(&Request::Read { gpa: addr, len })? {
            Reply::Bytes(bytes) if bytes.len() == len as usize => Ok(bytes),
            Reply::Bytes(_) => Err(MachineError::Backend(
                "control server returned a short memory read".into(),
            )),
            reply => Err(unexpected("Read", &reply)),
        }
    }
}

fn unexpected(verb: &str, reply: &Reply) -> MachineError {
    MachineError::Backend(format!("unexpected {verb} reply: {reply:?}"))
}

fn map_control_error(error: control_proto::ControlError) -> MachineError {
    match error {
        control_proto::ControlError::UnknownSnapshot(_) => MachineError::UnknownSnapshot,
        control_proto::ControlError::BadEnvVersion(_) => MachineError::BadEnvVersion,
        control_proto::ControlError::MalformedEnvironment => MachineError::MalformedEnv,
        control_proto::ControlError::ResolveWithoutDecision => MachineError::ResolveWithoutDecision,
        control_proto::ControlError::ReadOutOfRange { .. }
        | control_proto::ControlError::ReadTooLarge { .. } => MachineError::ReadOutOfBounds,
        other => MachineError::Backend(other.to_string()),
    }
}

fn map_stop_at(reason: control_proto::StopReason, logical_frame: Moment) -> StopReason {
    match reason {
        control_proto::StopReason::Deadline { .. } => StopReason::Deadline {
            vtime: logical_frame,
        },
        control_proto::StopReason::Quiescent { .. } => StopReason::Quiescent {
            vtime: logical_frame,
        },
        control_proto::StopReason::Crash { info, .. } => StopReason::Crash {
            vtime: logical_frame,
            info: CrashInfo {
                kind: match info.kind {
                    control_proto::CrashKind::Panic => CrashKind::Panic,
                    control_proto::CrashKind::UnrecoverableFault => CrashKind::UnrecoverableFault,
                    control_proto::CrashKind::Shutdown => CrashKind::Shutdown,
                },
                detail: info.detail,
            },
        },
        control_proto::StopReason::Decision { id, ctx, .. } => StopReason::Decision {
            vtime: logical_frame,
            id: DecisionId(id.0),
            ctx,
        },
        control_proto::StopReason::SnapshotPoint { .. } => StopReason::SnapshotPoint {
            vtime: logical_frame,
        },
        control_proto::StopReason::Assertion { ev, .. } => StopReason::Assertion {
            vtime: logical_frame,
            ev: EventRef {
                id: ev.id,
                data: ev.data,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Write};

    use super::{
        FRAME_COMPLETE_EVENT_ID, RestoreCounters, SnapshotCut, SocketMachine, WRAM_GPA_EVENT_ID,
        WRAM_LEN_EVENT_ID, client_caps,
    };
    use crate::{Machine, Moment, Reproducer, SnapId, StopConditions, StopReason};

    struct ScriptedStream {
        replies: Cursor<Vec<u8>>,
        requests: Vec<u8>,
    }

    impl Read for ScriptedStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.replies.read(buf)
        }
    }

    impl Write for ScriptedStream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.requests.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn replies(values: &[Result<control_proto::Reply, control_proto::ControlError>]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (index, value) in values.iter().enumerate() {
            control_proto::encode_reply(u32::try_from(index + 1).unwrap(), value, &mut bytes)
                .unwrap();
        }
        bytes
    }

    #[test]
    fn socket_machine_maps_every_core_verb_and_counts_restores() {
        let stream = ScriptedStream {
            replies: Cursor::new(replies(&[
                Ok(control_proto::Reply::Hello(client_caps())),
                Ok(control_proto::Reply::SdkEvents(vec![])),
                Ok(control_proto::Reply::Snapshot {
                    id: control_proto::SnapId(9),
                    at: control_proto::Moment(40),
                    sdk_events: 0,
                    tainted: false,
                }),
                Ok(control_proto::Reply::Unit),
                Ok(control_proto::Reply::Stop(
                    control_proto::StopReason::Quiescent {
                        vtime: control_proto::Moment(44),
                    },
                )),
                Ok(control_proto::Reply::SdkEvents(vec![])),
                Ok(control_proto::Reply::Bytes(vec![1, 2])),
                Ok(control_proto::Reply::Hash([7; 32])),
                Ok(control_proto::Reply::Unit),
                Ok(control_proto::Reply::Unit),
            ])),
            requests: Vec::new(),
        };
        let mut machine = SocketMachine::from_stream(stream).unwrap();
        let snap = machine.snapshot().unwrap();
        assert_eq!(snap, SnapId(9));
        assert_eq!(
            machine.snapshot_cut(snap),
            Some(SnapshotCut {
                at: Moment(40),
                frame: Moment(0),
                sdk_events: 0,
                tainted: false,
            })
        );
        machine.mark_genesis(snap).unwrap();
        machine
            .branch(
                snap,
                &Reproducer {
                    blob_version: environment::EnvSpec::BLOB_VERSION,
                    bytes: vec![1, 2, 3],
                },
            )
            .unwrap();
        assert_eq!(
            machine.run(StopConditions::default(), None).unwrap(),
            StopReason::Quiescent { vtime: Moment(0) }
        );
        assert_eq!(machine.read(10, 2).unwrap(), vec![1, 2]);
        assert_eq!(machine.state_hash().unwrap(), [7; 32]);
        machine.replay(snap).unwrap();
        assert_eq!(
            machine.restore_counters(),
            RestoreCounters {
                genesis: 2,
                continuation: 0,
            }
        );
        machine.drop_snapshot(snap).unwrap();
        assert_eq!(machine.snapshot_cut(snap), None);

        let stream = machine.connection.into_inner().stream;
        let mut input = stream.requests.as_slice();
        let mut seen = Vec::new();
        while !input.is_empty() {
            let (sequence, request, consumed) = control_proto::decode_request(input)
                .unwrap()
                .expect("complete request");
            seen.push((sequence, request));
            input = &input[consumed..];
        }
        assert_eq!(seen.len(), 10);
        assert!(matches!(seen[0].1, control_proto::Request::Hello(_)));
        assert!(matches!(
            seen[1].1,
            control_proto::Request::SdkEvents { .. }
        ));
        assert!(matches!(seen[2].1, control_proto::Request::Snapshot));
        assert!(matches!(seen[3].1, control_proto::Request::Branch { .. }));
        assert!(matches!(seen[4].1, control_proto::Request::Run { .. }));
        assert!(matches!(
            seen[5].1,
            control_proto::Request::SdkEvents { .. }
        ));
        assert!(matches!(seen[6].1, control_proto::Request::Read { .. }));
        assert!(matches!(seen[7].1, control_proto::Request::Hash { .. }));
        assert!(matches!(seen[8].1, control_proto::Request::Replay(_)));
        assert!(matches!(seen[9].1, control_proto::Request::Drop(_)));
    }

    #[test]
    fn socket_machine_uses_guest_frames_for_deadlines_and_not_wire_vtime() {
        let frame_event =
            |at, frame: u64| (at, FRAME_COMPLETE_EVENT_ID, frame.to_le_bytes().to_vec());
        let stream = ScriptedStream {
            replies: Cursor::new(replies(&[
                Ok(control_proto::Reply::Hello(client_caps())),
                Ok(control_proto::Reply::Stop(
                    control_proto::StopReason::SnapshotPoint {
                        vtime: control_proto::Moment(9_000_000),
                    },
                )),
                Ok(control_proto::Reply::SdkEvents(vec![
                    (1, WRAM_GPA_EVENT_ID, state_set(0x4012_3000)),
                    (2, WRAM_LEN_EVENT_ID, state_set(2048)),
                    frame_event(90, 4),
                ])),
                Ok(control_proto::Reply::SdkEvents(vec![])),
                Ok(control_proto::Reply::Stop(
                    control_proto::StopReason::SnapshotPoint {
                        vtime: control_proto::Moment(99_000_000),
                    },
                )),
                Ok(control_proto::Reply::SdkEvents(vec![frame_event(99, 7)])),
                Ok(control_proto::Reply::SdkEvents(vec![])),
            ])),
            requests: Vec::new(),
        };
        let mut machine = SocketMachine::from_stream(stream).unwrap();
        assert_eq!(
            machine
                .run(
                    StopConditions {
                        deadline: Some(Moment(5)),
                        ..StopConditions::default()
                    },
                    None,
                )
                .unwrap(),
            StopReason::Deadline { vtime: Moment(7) },
            "deadline is the first chord-boundary report reaching frame 5"
        );
        assert_eq!(machine.logical_frame(), Moment(7));
        assert_eq!(machine.wram_window(), Some((0x4012_3000, 2048)));

        let stream = machine.connection.into_inner().stream;
        let mut input = stream.requests.as_slice();
        let mut run_count = 0;
        while !input.is_empty() {
            let (_, request, consumed) = control_proto::decode_request(input)
                .unwrap()
                .expect("complete request");
            if let control_proto::Request::Run { until, .. } = request {
                run_count += 1;
                assert_eq!(until.deadline, None, "frames never ride as wire V-time");
                assert!(
                    until.on.armed(control_proto::class_bit::SNAPSHOT_POINT),
                    "the adapter advances at lifecycle yields"
                );
            }
            input = &input[consumed..];
        }
        assert_eq!(run_count, 2, "the first report at frame 4 cannot pass");
    }

    #[test]
    fn socket_machine_rejects_malformed_or_non_increasing_frame_clock() {
        let run_once = |events| {
            let stream = ScriptedStream {
                replies: Cursor::new(replies(&[
                    Ok(control_proto::Reply::Hello(client_caps())),
                    Ok(control_proto::Reply::Stop(
                        control_proto::StopReason::SnapshotPoint {
                            vtime: control_proto::Moment(1),
                        },
                    )),
                    Ok(control_proto::Reply::SdkEvents(events)),
                    Ok(control_proto::Reply::SdkEvents(vec![])),
                ])),
                requests: Vec::new(),
            };
            let mut machine = SocketMachine::from_stream(stream).unwrap();
            machine
                .run(
                    StopConditions {
                        deadline: Some(Moment(1)),
                        ..StopConditions::default()
                    },
                    None,
                )
                .unwrap_err()
        };

        let malformed = run_once(vec![(1, FRAME_COMPLETE_EVENT_ID, vec![0; 7])]);
        assert!(malformed.to_string().contains("malformed frame_complete"));

        let decreasing = run_once(vec![
            (1, FRAME_COMPLETE_EVENT_ID, 5_u64.to_le_bytes().to_vec()),
            (2, FRAME_COMPLETE_EVENT_ID, 4_u64.to_le_bytes().to_vec()),
        ]);
        assert!(
            decreasing
                .to_string()
                .contains("non-increasing guest frame clock")
        );

        let malformed_wram = run_once(vec![(1, WRAM_GPA_EVENT_ID, vec![0; 8])]);
        assert!(malformed_wram.to_string().contains("WRAM GPA publication"));
    }

    fn state_set(value: u64) -> Vec<u8> {
        let mut data = vec![0];
        data.extend_from_slice(&value.to_le_bytes());
        data
    }

    #[test]
    fn socket_machine_rejects_wrong_reply_sequence() {
        let mut bytes = Vec::new();
        control_proto::encode_reply(
            2,
            &Ok(control_proto::Reply::Hello(client_caps())),
            &mut bytes,
        )
        .unwrap();
        let error = SocketMachine::from_stream(ScriptedStream {
            replies: Cursor::new(bytes),
            requests: Vec::new(),
        })
        .err()
        .expect("wrong sequence rejects");
        assert!(error.to_string().contains("sequence mismatch"));
    }
}
