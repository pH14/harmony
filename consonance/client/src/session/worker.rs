// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    error::Error,
    ffi::OsString,
    io::{self, Write},
    net::Shutdown,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    ptr::NonNull,
    time::{Duration, Instant},
};

#[cfg(any(not(target_os = "macos"), test))]
use std::io::Read;

use control_proto::{
    Answer, DecisionId, Moment, Reply, Resolution, SnapId, StopConditions, StopMask, StopReason,
    decode_reply, encode_reply,
};
use environment::{
    channel::{CoverageSchedule, Effect},
    input_spec::{InputSpec, ServiceConfig, ServiceFactory},
};

use super::live::{checked_cached_attachment, vmm_target_commitment};
use super::{CachedSnapshotReceipt, SnapshotAttachment};
use super::{SdkEvent, Session, SessionConfig, SessionError, service_branch_spec};
use crate::ancillary::parse_control_rights;
use crate::cache::segments::SharedRange;
use crate::cache::{ANCHOR_DEPTH, CacheIndex, CommittedExtent, Lease, Namespace};

pub const WORKER_FD_ENV: &str = "HARMONY_SESSION_WORKER_FD";
const HEADER: usize = 16;
const MAX_FDS: usize = 2 * ANCHOR_DEPTH as usize;
#[cfg(target_os = "macos")]
const DARWIN_CONTROL_CAPACITY: usize = 2048;
const EXIT_POLL: Duration = Duration::from_millis(5);
const EXIT_POLLS: u32 = 400;
const IMAGE_HELLO: &[u8] = b"harmony-session-worker-image-v2";
const IMAGE_HELLO_LIMIT: usize = 512;
const IMAGE_HELLO_TIMEOUT: Duration = Duration::from_secs(15);

const INIT: u8 = 1;
const STATE_HASH: u8 = 2;
const EVENT_HISTORY: u8 = 17;
const CONSOLE_TAIL: u8 = 3;
const TELEMETRY: u8 = 4;
const OWNED_PAGES: u8 = 5;
const EXPORT: u8 = 6;
const NEED_EXTENT: u8 = 7;
const EXTENT_READY: u8 = 18;
const EXTENT_DENIED: u8 = 19;
const EXPORT_FRAME_LIMIT: usize = 4096;
const IMPORT: u8 = 8;
const SNAPSHOT: u8 = 9;
const DROP_SNAPSHOT: u8 = 10;
const REPLAY: u8 = 11;
const BRANCH: u8 = 12;
const RUN_UNTIL: u8 = 13;
const SDK_EVENTS: u8 = 14;
const STORE_BYTES: u8 = 15;
const RUN_RESOLVING: u8 = 16;

const OK: u8 = 0;
const FAILED: u8 = 1;
const OTHER: u8 = 0;
const HUNG: u8 = 1;
const ABANDONED: u8 = 2;
const INFRASTRUCTURE: u8 = 3;

pub trait SearchSession: std::fmt::Debug {
    fn setup_handle(&self) -> (SnapId, u64);
    fn state_hash(&mut self) -> Result<[u8; 32], Box<dyn Error>>;
    fn event_history(&mut self) -> Result<control_proto::EventHistory, Box<dyn Error>> {
        Err("ordered event history is unavailable for this backend".into())
    }
    fn cache_identity(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
        self.state_hash()
    }
    fn root_identity(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
        self.cache_identity()
    }
    fn publish_root(
        &self,
        index: &dyn CacheIndex,
        namespace: Namespace,
        target: SnapId,
    ) -> Result<Lease, Box<dyn Error>> {
        self.publish_snapshot(index, namespace, b"root", None, target, 1)
    }
    fn import_root(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
    ) -> Result<(SnapId, u64), Box<dyn Error>> {
        self.import_cached(index, lease, self.setup_handle().0)
    }
    fn console_tail(&mut self) -> Result<Vec<u8>, Box<dyn Error>>;
    fn telemetry_counters(&self) -> Vec<(String, u64)>;
    fn snapshot_owned_pages(&self, snapshot: SnapId) -> Option<u64>;
    fn store_bytes(&self) -> Option<u64>;
    fn publish_snapshot(
        &self,
        index: &dyn CacheIndex,
        namespace: Namespace,
        key: &[u8],
        parent: Option<(SnapId, &Lease)>,
        target: SnapId,
        cost: u64,
    ) -> Result<Lease, Box<dyn Error>>;
    fn import_cached(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
        near: SnapId,
    ) -> Result<(SnapId, u64), Box<dyn Error>>;
    #[expect(clippy::too_many_arguments)]
    fn publish_snapshot_attached(
        &self,
        index: &dyn CacheIndex,
        namespace: Namespace,
        key: &[u8],
        parent: Option<(SnapId, &Lease)>,
        target: SnapId,
        attachment: &SnapshotAttachment,
        cost: u64,
    ) -> Result<Lease, Box<dyn Error>> {
        if attachment != &SnapshotAttachment::default() {
            return Err("session does not support snapshot attachments".into());
        }
        self.publish_snapshot(index, namespace, key, parent, target, cost)
    }
    fn import_cached_receipt(
        &mut self,
        _index: &dyn CacheIndex,
        _lease: &Lease,
        _near: SnapId,
    ) -> Result<CachedSnapshotReceipt, Box<dyn Error>> {
        Err("session does not support snapshot attachment receipts".into())
    }
    fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>>;
    fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>>;
    fn branch_with_service(
        &mut self,
        snapshot: SnapId,
        config: ServiceConfig,
        payloads: Vec<Vec<u8>>,
        effects: Vec<(u64, Effect)>,
        coverage_schedule: Option<CoverageSchedule>,
    ) -> Result<(), Box<dyn Error>>;
    fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>>;
    fn run_resolving(
        &mut self,
        _deadline: u64,
        _resolution: Resolution,
    ) -> Result<StopReason, Box<dyn Error>> {
        Err("this session cannot resolve a service decision".into())
    }
    fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>>;
    fn sdk_events(&mut self) -> Result<Vec<SdkEvent>, Box<dyn Error>>;
    fn sdk_events_since(&mut self, offset: u32) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
        let offset = usize::try_from(offset)?;
        let mut events = self.sdk_events()?;
        u32::try_from(events.len()).map_err(|_| "SDK event offset overflow")?;
        if offset >= events.len() {
            return Ok(Vec::new());
        }
        Ok(events.split_off(offset))
    }
    fn abandoned(&self) -> bool;
}

trait ServedSession: SearchSession {
    fn export_delta_into(
        &self,
        parent: Option<SnapId>,
        target: SnapId,
        attachment: &SnapshotAttachment,
        allocate: &mut dyn FnMut(usize) -> Result<Mapping, Box<dyn Error>>,
    ) -> Result<(), Box<dyn Error>>;
    fn import_extents(
        &mut self,
        extents: &[&[u8]],
        near: SnapId,
    ) -> Result<(SnapId, u64), Box<dyn Error>>;
    fn branch_input(&mut self, snapshot: SnapId, spec: &InputSpec) -> Result<(), Box<dyn Error>>;
}

impl SearchSession for Session {
    fn setup_handle(&self) -> (SnapId, u64) {
        Session::setup_handle(self)
    }

    fn state_hash(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
        Session::state_hash(self)
    }

    fn event_history(&mut self) -> Result<control_proto::EventHistory, Box<dyn Error>> {
        Session::event_history(self)
    }

    fn console_tail(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
        Session::console_tail(self)
    }

    fn telemetry_counters(&self) -> Vec<(String, u64)> {
        Session::telemetry_counters(self)
    }

    fn snapshot_owned_pages(&self, snapshot: SnapId) -> Option<u64> {
        Session::snapshot_owned_pages(self, snapshot)
    }

    fn store_bytes(&self) -> Option<u64> {
        Some(Session::store_bytes(self))
    }

    fn publish_snapshot(
        &self,
        index: &dyn CacheIndex,
        namespace: Namespace,
        key: &[u8],
        parent: Option<(SnapId, &Lease)>,
        target: SnapId,
        cost: u64,
    ) -> Result<Lease, Box<dyn Error>> {
        Session::publish_snapshot(self, index, namespace, key, parent, target, cost)
    }

    fn import_cached(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
        near: SnapId,
    ) -> Result<(SnapId, u64), Box<dyn Error>> {
        Session::import_cached(self, index, lease, near)
    }

    fn publish_snapshot_attached(
        &self,
        index: &dyn CacheIndex,
        namespace: Namespace,
        key: &[u8],
        parent: Option<(SnapId, &Lease)>,
        target: SnapId,
        attachment: &SnapshotAttachment,
        cost: u64,
    ) -> Result<Lease, Box<dyn Error>> {
        Session::publish_snapshot_attached(
            self, index, namespace, key, parent, target, attachment, cost,
        )
    }
    fn import_cached_receipt(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
        near: SnapId,
    ) -> Result<CachedSnapshotReceipt, Box<dyn Error>> {
        Session::import_cached_receipt(self, index, lease, near)
    }
    fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        Session::replay_snapshot(self, snapshot)
    }

    fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        Session::drop_snapshot(self, snapshot)
    }

    fn branch_with_service(
        &mut self,
        snapshot: SnapId,
        config: ServiceConfig,
        payloads: Vec<Vec<u8>>,
        effects: Vec<(u64, Effect)>,
        coverage_schedule: Option<CoverageSchedule>,
    ) -> Result<(), Box<dyn Error>> {
        Session::branch_with_service(self, snapshot, config, payloads, effects, coverage_schedule)
    }

    fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
        Session::run_until(self, deadline)
    }

    fn run_resolving(
        &mut self,
        deadline: u64,
        resolution: Resolution,
    ) -> Result<StopReason, Box<dyn Error>> {
        Session::run(
            self,
            StopConditions {
                deadline: Some(Moment(deadline)),
                on: StopMask::NONE.arm(control_proto::class_bit::ASSERTION),
            },
            Some(resolution),
        )
    }

    fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
        Session::snapshot(self)
    }

    fn sdk_events(&mut self) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
        Session::sdk_events(self)
    }

    fn sdk_events_since(&mut self, offset: u32) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
        Session::sdk_events_since(self, offset)
    }

    fn abandoned(&self) -> bool {
        Session::abandoned(self)
    }
}

#[cfg(all(test, target_arch = "aarch64"))]
pub(super) fn export_destination_for_test(
    session: &Session,
    index: &dyn CacheIndex,
    parent: Option<SnapId>,
    target: SnapId,
    attachment: &SnapshotAttachment,
) -> (crate::cache::WritableExtent, Vec<(usize, usize)>) {
    let mut extent = None;
    let mut allocations = Vec::new();
    session
        .export_delta_into(parent, target, attachment, &mut |len| {
            let destination = index.extent(len)?;
            let SharedRange { fd, offset, .. } = destination.shared_range();
            let mut mapping = Mapping::new(fd, offset, len, true)?;
            allocations.push((mapping.bytes_mut().as_ptr() as usize, len));
            extent = Some(destination);
            Ok(mapping)
        })
        .unwrap();
    (extent.unwrap(), allocations)
}

impl ServedSession for Session {
    fn export_delta_into(
        &self,
        parent: Option<SnapId>,
        target: SnapId,
        attachment: &SnapshotAttachment,
        allocate: &mut dyn FnMut(usize) -> Result<Mapping, Box<dyn Error>>,
    ) -> Result<(), Box<dyn Error>> {
        let mapping =
            self.write_delta_attached(parent, target, attachment, allocate, Mapping::bytes_mut)?;
        drop(mapping);
        Ok(())
    }
    fn import_extents(
        &mut self,
        extents: &[&[u8]],
        near: SnapId,
    ) -> Result<(SnapId, u64), Box<dyn Error>> {
        Session::import_extents(self, extents, near)
    }

    fn branch_input(&mut self, snapshot: SnapId, spec: &InputSpec) -> Result<(), Box<dyn Error>> {
        Session::branch_input(self, snapshot, spec)
    }
}

fn malformed(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("session worker frame: {what}"),
    )
}

fn transport_error(error: io::Error) -> SessionError {
    if matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
    ) {
        SessionError::Infrastructure(format!("session worker disconnected: {error}"))
    } else {
        SessionError::Control(format!("session worker: {error}"))
    }
}

#[derive(Debug, Default)]
struct Out(Vec<u8>);

impl Out {
    fn op(op: u8) -> Self {
        Self(vec![op])
    }

    fn u8(&mut self, value: u8) -> &mut Self {
        self.0.push(value);
        self
    }

    fn u64(&mut self, value: u64) -> &mut Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn len(&mut self, value: usize) -> &mut Self {
        self.u64(value as u64)
    }

    fn bytes(&mut self, value: &[u8]) -> &mut Self {
        self.len(value.len());
        self.0.extend_from_slice(value);
        self
    }

    fn option(&mut self, value: Option<u64>) -> &mut Self {
        match value {
            Some(value) => self.u8(1).u64(value),
            None => self.u8(0),
        }
    }
}

struct In<'a>(&'a [u8]);

impl<'a> In<'a> {
    fn take(&mut self, len: usize) -> io::Result<&'a [u8]> {
        if self.0.len() < len {
            return Err(malformed("truncated"));
        }
        let (head, rest) = self.0.split_at(len);
        self.0 = rest;
        Ok(head)
    }

    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }

    fn len(&mut self) -> io::Result<usize> {
        usize::try_from(self.u64()?).map_err(|_| malformed("length overflows"))
    }

    fn bytes(&mut self) -> io::Result<&'a [u8]> {
        let len = self.len()?;
        self.take(len)
    }

    fn string(&mut self) -> io::Result<String> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| malformed("text is not UTF-8"))
    }

    fn option(&mut self) -> io::Result<Option<u64>> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.u64()?)),
            _ => Err(malformed("option tag")),
        }
    }

    fn snap(&mut self) -> io::Result<(SnapId, u64)> {
        Ok((SnapId(self.u64()?), self.u64()?))
    }

    fn finish(self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(malformed("trailing bytes"))
        }
    }
}

#[cfg(target_os = "linux")]
const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;
#[cfg(not(target_os = "linux"))]
const SEND_FLAGS: libc::c_int = 0;
#[cfg(target_os = "linux")]
const RECV_FLAGS: libc::c_int = libc::MSG_CMSG_CLOEXEC;
#[cfg(not(target_os = "linux"))]
const RECV_FLAGS: libc::c_int = 0;

fn control_space(fds: usize) -> usize {
    let data = u32::try_from(fds * size_of::<RawFd>()).expect("descriptor table fits u32");
    // SAFETY: CMSG_SPACE is pure arithmetic on its argument.
    unsafe { libc::CMSG_SPACE(data) as usize }
}

fn send_with_fds(stream: &UnixStream, bytes: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<usize> {
    let raw: Vec<RawFd> = fds.iter().map(AsRawFd::as_raw_fd).collect();
    let data = size_of_val(raw.as_slice());
    let space = if raw.is_empty() {
        0
    } else {
        control_space(raw.len())
    };
    let mut control = vec![0_u64; space.div_ceil(size_of::<u64>())];
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    // SAFETY: msghdr is a plain C struct for which all-zero bytes are a valid empty message.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    if !raw.is_empty() {
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = space as _;
        // SAFETY: msg_control points at `space` zeroed, u64-aligned bytes, which CMSG_SPACE
        // sized for one header carrying `data` bytes, so the first header and its data fit.
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&raw const message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(data as u32) as _;
            std::ptr::copy_nonoverlapping(raw.as_ptr().cast::<u8>(), libc::CMSG_DATA(header), data);
        }
    }
    loop {
        // SAFETY: message, its iovec and its control buffer stay alive and unmoved for the call.
        let sent = unsafe { libc::sendmsg(stream.as_raw_fd(), &raw const message, SEND_FLAGS) };
        if let Ok(sent) = usize::try_from(sent) {
            return Ok(sent);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[allow(clippy::disallowed_methods)]
fn recv_with_fds(
    stream: &UnixStream,
    buffer: &mut [u8],
    deadline: Option<Instant>,
) -> io::Result<(usize, Vec<OwnedFd>)> {
    #[cfg(target_os = "macos")]
    let space = DARWIN_CONTROL_CAPACITY;
    #[cfg(not(target_os = "macos"))]
    let space = control_space(MAX_FDS);
    recv_with_fds_control_space(stream, buffer, deadline, space)
}

#[allow(clippy::disallowed_methods)]
fn recv_with_fds_control_space(
    stream: &UnixStream,
    buffer: &mut [u8],
    deadline: Option<Instant>,
    space: usize,
) -> io::Result<(usize, Vec<OwnedFd>)> {
    let mut control = vec![0_u64; space.div_ceil(size_of::<u64>())];
    let mut iov = libc::iovec {
        iov_base: buffer.as_mut_ptr().cast(),
        iov_len: buffer.len(),
    };
    // SAFETY: msghdr is a plain C struct for which all-zero bytes are a valid empty message.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = space as _;
    let received = loop {
        if let Some(deadline) = deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            stream.set_read_timeout(Some(remaining))?;
        }
        // SAFETY: message, its iovec and its control buffer stay alive and unmoved for the call.
        let received = unsafe { libc::recvmsg(stream.as_raw_fd(), &raw mut message, RECV_FLAGS) };
        if let Ok(received) = usize::try_from(received) {
            break received;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    };
    let mut fds = Vec::new();
    // SAFETY: control owns initialized u64 storage of at least space bytes, which was the
    // capacity supplied to recvmsg; the byte view cannot exceed that allocation.
    let control_bytes = unsafe { std::slice::from_raw_parts(control.as_ptr().cast::<u8>(), space) };
    let parsed = parse_control_rights(control_bytes, message.msg_controllen as usize, |raw| {
        // SAFETY: only complete SCM_RIGHTS integers within both the supplied allocation and
        // recvmsg's returned extent reach this callback; these are fresh descriptors installed
        // by this recvmsg, and each is immediately assigned exactly one owning handle.
        fds.push(unsafe { OwnedFd::from_raw_fd(raw) });
    });
    if message.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(malformed("descriptors were truncated"));
    }
    parsed?;
    if fds.len() > MAX_FDS {
        return Err(malformed("too many descriptors"));
    }
    #[cfg(not(target_os = "linux"))]
    for fd in &fds {
        set_cloexec(fd.as_raw_fd(), true)?;
    }
    Ok((received, fds))
}

fn set_cloexec(fd: RawFd, on: bool) -> io::Result<()> {
    let flags = if on { libc::FD_CLOEXEC } else { 0 };
    // SAFETY: F_SETFD only changes the descriptor flags of an open descriptor.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn send(stream: &UnixStream, body: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    if fds.len() > MAX_FDS {
        return Err(malformed("too many descriptors"));
    }
    let mut header = [0_u8; HEADER];
    header[..8].copy_from_slice(&(body.len() as u64).to_le_bytes());
    header[8..].copy_from_slice(&(fds.len() as u64).to_le_bytes());
    let sent = send_with_fds(stream, &header, fds)?;
    let mut writer = stream;
    writer.write_all(&header[sent..])?;
    writer.write_all(body)
}

#[cfg(not(target_os = "macos"))]
#[allow(clippy::disallowed_methods)]
fn read_exact_until(
    stream: &UnixStream,
    mut buffer: &mut [u8],
    deadline: Instant,
) -> io::Result<()> {
    while !buffer.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        stream.set_read_timeout(Some(remaining))?;
        let size = (&*stream).read(buffer)?;
        if size == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buffer = &mut buffer[size..];
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn read_frame_exact(
    stream: &UnixStream,
    mut buffer: &mut [u8],
    deadline: Option<Instant>,
) -> io::Result<()> {
    while !buffer.is_empty() {
        let (size, rights) = recv_with_fds(stream, buffer, deadline)?;
        if !rights.is_empty() {
            return Err(malformed("unexpected descriptors after frame header"));
        }
        if size == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buffer = &mut buffer[size..];
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn read_frame_exact(
    stream: &UnixStream,
    buffer: &mut [u8],
    deadline: Option<Instant>,
) -> io::Result<()> {
    if let Some(deadline) = deadline {
        read_exact_until(stream, buffer, deadline)
    } else {
        (&*stream).read_exact(buffer)
    }
}

fn recv_with_limit(
    stream: &UnixStream,
    limit: usize,
    deadline: Option<Instant>,
) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut header = [0_u8; HEADER];
    let (received, fds) = recv_with_fds(stream, &mut header, deadline)?;
    if received == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    read_frame_exact(stream, &mut header[received..], deadline)?;
    let len = usize::try_from(u64::from_le_bytes(
        header[..8].try_into().expect("eight bytes"),
    ))
    .map_err(|_| malformed("length overflows"))?;
    if len > limit {
        return Err(malformed("frame length exceeds limit"));
    }
    if u64::from_le_bytes(header[8..].try_into().expect("eight bytes")) != fds.len() as u64 {
        return Err(malformed("descriptor count"));
    }
    let mut body = vec![0; len];
    read_frame_exact(stream, &mut body, deadline)?;
    Ok((body, fds))
}

fn recv(stream: &UnixStream) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    recv_with_limit(stream, usize::MAX, None)
}

#[cfg(test)]
thread_local! { static MAPPING_ATTEMPTS: Cell<usize> = const { Cell::new(0) }; }

struct Mapping {
    base: NonNull<u8>,
    skip: usize,
    len: usize,
}

impl Mapping {
    fn new(fd: BorrowedFd<'_>, offset: usize, len: usize, writable: bool) -> io::Result<Self> {
        #[cfg(test)]
        MAPPING_ATTEMPTS.with(|attempts| attempts.set(attempts.get() + 1));
        // SAFETY: sysconf reads a constant system parameter.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .map_err(|_| io::Error::last_os_error())?;
        let (start, skip, mapped_len) = crate::cache::segments::checked_mapping_range(
            offset,
            len,
            crate::cache::segments::backing_len(fd)?,
            page,
        )?;
        let start = libc::off_t::try_from(start).map_err(|_| malformed("offset"))?;
        let protection = if writable {
            libc::PROT_READ | libc::PROT_WRITE
        } else {
            libc::PROT_READ
        };
        // SAFETY: a fresh shared mapping of a descriptor the caller holds open aliases no Rust
        // object. The checked range fits the current backing; the owning coordinator keeps its
        // allocation alive without truncating it for the entire exchange.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mapped_len,
                protection,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                start,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let base = NonNull::new(base.cast::<u8>()).ok_or_else(io::Error::last_os_error)?;
        Ok(Self { base, skip, len })
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the mapping covers skip + len bytes; the coordinator holds a lease on the
        // committed extent for the whole request, and a committed extent is never written again.
        unsafe { std::slice::from_raw_parts(self.base.as_ptr().add(self.skip), self.len) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: the mapping covers skip + len bytes; the coordinator holds the only writable
        // extent for this range and does not touch it until this request is answered.
        unsafe { std::slice::from_raw_parts_mut(self.base.as_ptr().add(self.skip), self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: base and skip + len describe the mapping created in new, and no slice into it
        // outlives self.
        unsafe { libc::munmap(self.base.as_ptr().cast(), self.skip + self.len) };
    }
}

#[derive(Clone, Debug)]
pub struct WorkerLauncher {
    program: PathBuf,
    args: Vec<OsString>,
    expected_image: String,
    expected_main: String,
    loaded_image_guard: crate::image_identity::LoadedImageGuard,
}

impl WorkerLauncher {
    pub fn current_exe(args: Vec<OsString>) -> io::Result<Self> {
        let loaded_image_guard = crate::image_identity::LoadedImageGuard::capture()?;
        #[cfg(target_os = "linux")]
        let program = PathBuf::from("/proc/self/exe");
        #[cfg(not(target_os = "linux"))]
        let program = std::env::current_exe()?;
        let launcher = Self {
            program,
            args,
            expected_image: loaded_image_guard.current_identity()?,
            expected_main: crate::image_identity::loaded_executable_identity_for_child(
                std::process::id(),
            )?,
            loaded_image_guard: loaded_image_guard.clone(),
        };
        loaded_image_guard.check_at(crate::image_identity::GuardSite::Admission)?;
        Ok(launcher)
    }

    pub fn telemetry_counters(&self) -> Vec<(String, u64)> {
        self.loaded_image_guard.counters("guard.launcher")
    }

    #[allow(clippy::disallowed_methods)]
    #[cfg(test)]
    fn spawn(&self) -> io::Result<(Child, UnixStream)> {
        self.spawn_guarded(&self.loaded_image_guard)
    }

    #[allow(clippy::disallowed_methods)]
    fn spawn_guarded(
        &self,
        loaded_image_guard: &crate::image_identity::LoadedImageGuard,
    ) -> io::Result<(Child, UnixStream)> {
        loaded_image_guard.check_at(crate::image_identity::GuardSite::LauncherBefore)?;
        crate::image_identity::reject_loader_overrides()?;
        if loaded_image_guard.current_identity()? != self.expected_image {
            return Err(io::Error::other(
                "launcher loaded image changed before worker spawn",
            ));
        }
        let (parent, child) = UnixStream::pair()?;
        let raw = child.as_raw_fd();
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .env(WORKER_FD_ENV, raw.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        // SAFETY: the hook runs in the forked child before exec and only calls fcntl, which is
        // async-signal-safe; clearing close-on-exec there leaves every other process untouched.
        unsafe {
            command.pre_exec(move || set_cloexec(raw, false));
        }
        let mut process = command.spawn()?;
        let actual = crate::image_identity::loaded_executable_identity_for_child(process.id());
        let check = match actual {
            Ok(actual) if actual == self.expected_main => Ok(()),
            Ok(_) => Err(io::Error::other(
                "session worker executable differs from launcher",
            )),
            Err(error) => Err(io::Error::new(
                error.kind(),
                format!("cannot identify session worker executable: {error}"),
            )),
        };
        drop(child);
        let deadline = Instant::now() + IMAGE_HELLO_TIMEOUT;
        let check = check.and_then(|()| {
            let (body, fds) = recv_with_limit(&parent, IMAGE_HELLO_LIMIT, Some(deadline))?;
            if !fds.is_empty() {
                return Err(malformed("worker image hello carries descriptors"));
            }
            let mut input = In(&body);
            if input.bytes()? != IMAGE_HELLO {
                return Err(malformed("worker image hello has an unknown version"));
            }
            let actual = input.string()?;
            input.finish()?;
            if actual != self.expected_image {
                return Err(io::Error::other(
                    "session worker loaded image differs from launcher",
                ));
            }
            Ok(())
        });
        let check = check.and_then(|()| {
            loaded_image_guard.check_at(crate::image_identity::GuardSite::LauncherAfter)
        });
        let check = check.and(parent.set_read_timeout(None));
        if let Err(error) = check {
            let _ = process.kill();
            let _ = process.wait();
            return Err(error);
        }
        Ok((process, parent))
    }
}

fn put_config(out: &mut Out, config: &SessionConfig) {
    out.u8(match config.guest_contract {
        super::GuestContract::Ordinary => 0,
        super::GuestContract::NestedHost => 1,
    })
    .len(config.ram_bytes)
    .u64(config.seed)
    .u64(config.run_budget)
    .bytes(config.cmdline.as_bytes())
    .bytes(config.identity_tag.as_bytes())
    .option(
        config
            .wall_limit
            .map(|limit| u64::try_from(limit.as_nanos()).unwrap_or(u64::MAX)),
    )
    .u8(u8::from(config.defer_virtual_time_checkpoint_hashes));
}

fn take_config(input: &mut In<'_>) -> io::Result<SessionConfig> {
    Ok(SessionConfig {
        guest_contract: match input.u8()? {
            0 => super::GuestContract::Ordinary,
            1 => super::GuestContract::NestedHost,
            _ => return Err(malformed("unknown guest contract")),
        },
        ram_bytes: input.len()?,
        seed: input.u64()?,
        run_budget: input.u64()?,
        cmdline: input.string()?,
        identity_tag: input.string()?,
        wall_limit: input.option()?.map(Duration::from_nanos),
        defer_virtual_time_checkpoint_hashes: input.u8()? != 0,
    })
}

struct WorkerConnection {
    stream: UnixStream,
    process: Option<Child>,
    loaded_image_guard: crate::image_identity::LoadedImageGuard,
}

#[derive(Debug)]
pub struct WorkerSession {
    stream: UnixStream,
    process: Option<Child>,
    loaded_image_guard: crate::image_identity::LoadedImageGuard,
    setup: (SnapId, u64),
    seed: u64,
    failed: Cell<bool>,
    counters: RefCell<Vec<(String, u64)>>,
    telemetry_queries: Cell<u64>,
    telemetry_query_failures: Cell<u64>,
    exchanging: Cell<bool>,
}

struct Exchange<'a>(&'a Cell<bool>);
impl Drop for Exchange<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl WorkerSession {
    pub fn spawn(
        launcher: &WorkerLauncher,
        kernel: &[u8],
        initramfs: &[u8],
        config: &SessionConfig,
        setup_payloads: &[Vec<u8>],
        service: &str,
    ) -> Result<Self, Box<dyn Error>> {
        let loaded_image_guard = launcher.loaded_image_guard.fork();
        let (process, stream) = launcher
            .spawn_guarded(&loaded_image_guard)
            .map_err(|error| {
                SessionError::Control(format!(
                    "start session worker {}: {error}",
                    launcher.program.display()
                ))
            })?;
        Self::connect(
            WorkerConnection {
                stream,
                process: Some(process),
                loaded_image_guard,
            },
            kernel,
            initramfs,
            config,
            setup_payloads,
            service,
        )
    }

    fn connect(
        connection: WorkerConnection,
        kernel: &[u8],
        initramfs: &[u8],
        config: &SessionConfig,
        setup_payloads: &[Vec<u8>],
        service: &str,
    ) -> Result<Self, Box<dyn Error>> {
        let WorkerConnection {
            stream,
            process,
            loaded_image_guard,
        } = connection;
        loaded_image_guard.check_at(crate::image_identity::GuardSite::Admission)?;
        let mut session = Self {
            stream,
            process,
            loaded_image_guard,
            setup: (SnapId(0), 0),
            seed: config.seed,
            failed: Cell::new(false),
            counters: RefCell::default(),
            exchanging: Cell::new(false),
            telemetry_queries: Cell::new(0),
            telemetry_query_failures: Cell::new(0),
        };
        let mut request = Out::op(INIT);
        put_config(&mut request, config);
        request
            .bytes(kernel)
            .bytes(initramfs)
            .len(setup_payloads.len());
        for payload in setup_payloads {
            request.bytes(payload);
        }
        request.bytes(service.as_bytes());
        let reply = session.call(&request, &[])?;
        let mut input = In(&reply);
        session.setup = input.snap()?;
        input.finish()?;
        session.check_loaded_image_at(crate::image_identity::GuardSite::Admission)?;
        Ok(session)
    }

    #[must_use]
    pub fn process_id(&self) -> Option<u32> {
        self.process.as_ref().map(Child::id)
    }

    fn check_loaded_image_at(
        &self,
        site: crate::image_identity::GuardSite,
    ) -> Result<(), Box<dyn Error>> {
        if self.failed.get() {
            return Err(SessionError::Abandoned.into());
        }
        if let Err(error) = self.loaded_image_guard.check_at(site) {
            self.failed.set(true);
            return Err(error.into());
        }
        Ok(())
    }

    fn begin_exchange(&self) -> Result<Exchange<'_>, Box<dyn Error>> {
        if self.exchanging.replace(true) {
            return Err("session worker exchange is already active".into());
        }
        Ok(Exchange(&self.exchanging))
    }

    fn call(&self, request: &Out, fds: &[BorrowedFd<'_>]) -> Result<Vec<u8>, Box<dyn Error>> {
        let _exchange = self.begin_exchange()?;
        self.check_loaded_image_at(crate::image_identity::GuardSite::ParentRpcBefore)?;
        let reply = send(&self.stream, &request.0, fds).and_then(|()| recv(&self.stream));
        let body = match reply {
            Ok((body, _)) => body,
            Err(error) => {
                self.failed.set(true);
                return Err(transport_error(error).into());
            }
        };
        self.check_loaded_image_at(crate::image_identity::GuardSite::ParentRpcAfter)?;
        self.reply_status(&body)
    }

    fn reply_status(&self, body: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut input = In(body);
        match input.u8()? {
            OK => Ok(input.0.to_vec()),
            FAILED => {
                let kind = input.u8()?;
                let error = match kind {
                    HUNG => SessionError::Hung(Duration::from_nanos(input.u64()?)),
                    ABANDONED => SessionError::Abandoned,
                    INFRASTRUCTURE => SessionError::Infrastructure(input.string()?),
                    OTHER => SessionError::Control(input.string()?),
                    _ => return Err(malformed("reply error kind").into()),
                };
                input.finish()?;
                if kind != OTHER {
                    self.failed.set(true);
                }
                Err(error.into())
            }
            _ => Err(malformed("reply status").into()),
        }
    }

    fn export_extent(
        &self,
        index: &dyn CacheIndex,
        parent: Option<SnapId>,
        target: SnapId,
        attachment: &SnapshotAttachment,
    ) -> Result<crate::cache::WritableExtent, Box<dyn Error>> {
        let _exchange = self.begin_exchange()?;
        self.check_loaded_image_at(crate::image_identity::GuardSite::ParentExportBefore)?;
        let mut denied = None;
        let mut rejected = None;
        let result: Result<Option<crate::cache::WritableExtent>, Box<dyn Error>> = (|| {
            let mut request = Out::op(EXPORT);
            request
                .option(parent.map(|snap| snap.0))
                .u64(target.0)
                .bytes(&attachment.domain)
                .bytes(&attachment.bytes);
            send(&self.stream, &request.0, &[])?;
            let (body, fds) = recv_with_limit(&self.stream, EXPORT_FRAME_LIMIT, None)?;
            self.check_loaded_image_at(crate::image_identity::GuardSite::ParentExtentRequested)?;
            if !fds.is_empty() {
                return Err(malformed("allocation request descriptors").into());
            }
            let mut input = In(&body);
            if input.u8()? != NEED_EXTENT {
                if body.first() != Some(&FAILED) {
                    return Err(malformed("missing allocation request").into());
                }
                let mut failure = In(&body[1..]);
                match failure.u8()? {
                    HUNG => {
                        failure.u64()?;
                    }
                    ABANDONED => {}
                    OTHER | INFRASTRUCTURE => {
                        failure.string()?;
                    }
                    _ => return Err(malformed("reply error kind").into()),
                }
                failure.finish()?;
                rejected = Some(self.reply_status(&body).unwrap_err());
                return Ok(None);
            }
            let len = input.len()?;
            input.finish()?;
            if len == 0 {
                return Err(malformed("empty allocation request").into());
            }
            let extent = match index.extent(len) {
                Ok(extent) => Some(extent),
                Err(error) => {
                    denied = Some(error);
                    None
                }
            };
            self.check_loaded_image_at(crate::image_identity::GuardSite::ParentExtentAllocated)?;
            if let Some(extent) = extent.as_ref() {
                let SharedRange {
                    fd,
                    offset,
                    len: usable,
                } = extent.shared_range();
                if usable < len {
                    return Err(malformed("allocated range too short").into());
                }
                send(
                    &self.stream,
                    &Out::op(EXTENT_READY).len(offset).len(len).0,
                    &[fd],
                )?;
            } else {
                send(&self.stream, &Out::op(EXTENT_DENIED).0, &[])?;
            }
            let (body, fds) = recv_with_limit(&self.stream, EXPORT_FRAME_LIMIT, None)?;
            self.check_loaded_image_at(crate::image_identity::GuardSite::ParentExportCompleted)?;
            if !fds.is_empty() {
                return Err(malformed("completion descriptors").into());
            }
            if denied.is_some() {
                let mut input = In(&body);
                if input.u8()? != FAILED || input.u8()? != OTHER {
                    return Err(malformed("allocation denial acknowledgement").into());
                }
                input.string()?;
                input.finish()?;
                return Ok(None);
            }
            let reply = self.reply_status(&body)?;
            In(&reply).finish()?;
            Ok(extent)
        })();
        match result {
            Ok(Some(extent)) => Ok(extent),
            Ok(None) => match rejected {
                Some(error) => Err(error),
                None => Err(denied.expect("denied allocation").into()),
            },
            Err(error) => {
                self.failed.set(true);
                let _ = self.stream.shutdown(Shutdown::Both);
                if error.is::<SessionError>() {
                    Err(error)
                } else {
                    match error.downcast::<io::Error>() {
                        Ok(error) => Err(transport_error(*error).into()),
                        Err(error) => Err(SessionError::Control(format!(
                            "session worker export: {error}"
                        ))
                        .into()),
                    }
                }
            }
        }
    }

    fn call_snap(&self, request: &Out) -> Result<(SnapId, u64), Box<dyn Error>> {
        let reply = self.call(request, &[])?;
        let mut input = In(&reply);
        let snap = input.snap()?;
        input.finish()?;
        Ok(snap)
    }

    fn call_unit(&self, request: &Out) -> Result<(), Box<dyn Error>> {
        let reply = self.call(request, &[])?;
        In(&reply).finish()?;
        Ok(())
    }
}

impl Drop for WorkerSession {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
        let Some(mut process) = self.process.take() else {
            return;
        };
        for _ in 0..EXIT_POLLS {
            if !matches!(process.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(EXIT_POLL);
        }
        let _ = process.kill();
        let _ = process.wait();
    }
}

impl SearchSession for WorkerSession {
    fn setup_handle(&self) -> (SnapId, u64) {
        self.setup
    }

    fn state_hash(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
        let reply = self.call(&Out::op(STATE_HASH), &[])?;
        Ok(reply
            .as_slice()
            .try_into()
            .map_err(|_| malformed("state hash"))?)
    }

    fn event_history(&mut self) -> Result<control_proto::EventHistory, Box<dyn Error>> {
        let reply = self.call(&Out::op(EVENT_HISTORY), &[])?;
        if reply.len() != 40 {
            return Err(malformed("event history").into());
        }
        Ok(control_proto::EventHistory {
            count: u64::from_le_bytes(
                reply[..8]
                    .try_into()
                    .map_err(|_| malformed("event count"))?,
            ),
            digest: reply[8..]
                .try_into()
                .map_err(|_| malformed("event digest"))?,
        })
    }

    fn console_tail(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
        self.call(&Out::op(CONSOLE_TAIL), &[])
    }

    fn telemetry_counters(&self) -> Vec<(String, u64)> {
        let decode = |reply: Vec<u8>| -> io::Result<Vec<(String, u64)>> {
            let mut input = In(&reply);
            let counters = (0..input.len()?)
                .map(|_| Ok((input.string()?, input.u64()?)))
                .collect::<io::Result<Vec<_>>>()?;
            input.finish()?;
            Ok(counters)
        };
        self.telemetry_queries
            .set(self.telemetry_queries.get().saturating_add(1));
        let mut fresh = false;
        if let Ok(Ok(counters)) = self.call(&Out::op(TELEMETRY), &[]).map(decode) {
            fresh = true;
            self.counters.replace(
                counters
                    .into_iter()
                    .map(|(name, value)| {
                        let name = name.strip_prefix("guard.session.").map_or_else(
                            || name.clone(),
                            |suffix| format!("guard.child.session.{suffix}"),
                        );
                        (name, value)
                    })
                    .collect(),
            );
        }
        if !fresh {
            self.telemetry_query_failures
                .set(self.telemetry_query_failures.get().saturating_add(1));
        }
        let mut counters = self.counters.borrow().clone();
        counters.extend(self.loaded_image_guard.counters("guard.parent"));
        counters.push((
            "guard.child.telemetry_queries".into(),
            self.telemetry_queries.get(),
        ));
        counters.push((
            "guard.child.telemetry_query_failures".into(),
            self.telemetry_query_failures.get(),
        ));
        counters
    }

    fn snapshot_owned_pages(&self, snapshot: SnapId) -> Option<u64> {
        let reply = self.call(Out::op(OWNED_PAGES).u64(snapshot.0), &[]).ok()?;
        let mut input = In(&reply);
        let pages = input.option().ok()?;
        input.finish().ok()?;
        pages
    }

    fn store_bytes(&self) -> Option<u64> {
        let reply = self.call(&Out::op(STORE_BYTES), &[]).ok()?;
        let mut input = In(&reply);
        let bytes = input.u64().ok()?;
        input.finish().ok()?;
        Some(bytes)
    }

    fn publish_snapshot(
        &self,
        index: &dyn CacheIndex,
        namespace: Namespace,
        key: &[u8],
        parent: Option<(SnapId, &Lease)>,
        target: SnapId,
        cost: u64,
    ) -> Result<Lease, Box<dyn Error>> {
        self.publish_snapshot_attached(
            index,
            namespace,
            key,
            parent,
            target,
            &SnapshotAttachment::default(),
            cost,
        )
    }

    fn publish_snapshot_attached(
        &self,
        index: &dyn CacheIndex,
        namespace: Namespace,
        key: &[u8],
        parent: Option<(SnapId, &Lease)>,
        target: SnapId,
        attachment: &SnapshotAttachment,
        cost: u64,
    ) -> Result<Lease, Box<dyn Error>> {
        let parent = parent.filter(|(_, lease)| lease.depth() + 1 < ANCHOR_DEPTH);
        let mut extent =
            self.export_extent(index, parent.map(|(snap, _)| snap), target, attachment)?;
        let target_commitment = vmm_target_commitment(&mut extent)?;
        self.check_loaded_image_at(crate::image_identity::GuardSite::PublicationBefore)?;
        let lease = index.publish(
            namespace,
            key,
            parent.map(|(_, lease)| lease),
            extent,
            target_commitment,
            cost,
        )?;
        match self.loaded_image_guard.check_published(index, lease) {
            Ok(lease) => {
                if let Err(error) = index.verify_lease(&lease) {
                    index.release(lease);
                    return Err(error.into());
                }
                Ok(lease)
            }
            Err(error) => {
                self.failed.set(true);
                Err(error.into())
            }
        }
    }

    fn import_cached(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
        near: SnapId,
    ) -> Result<(SnapId, u64), Box<dyn Error>> {
        let receipt = self.import_cached_receipt(index, lease, near)?;
        Ok((receipt.snapshot, receipt.moment))
    }

    fn import_cached_receipt(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
        near: SnapId,
    ) -> Result<CachedSnapshotReceipt, Box<dyn Error>> {
        let chain = index.chain(lease)?;
        let extents: Vec<&[u8]> = chain.iter().map(CommittedExtent::bytes).collect();
        let (machine_commitment, attachment, object_commitment) =
            checked_cached_attachment(&extents, index.target_commitment(lease)?)?;
        let ranges: Vec<SharedRange<'_>> =
            chain.iter().map(CommittedExtent::shared_range).collect();
        let mut request = Out::op(IMPORT);
        request.u64(near.0).len(ranges.len());
        for range in &ranges {
            request.len(range.offset).len(range.len);
        }
        let fds: Vec<BorrowedFd<'_>> = ranges.iter().map(|range| range.fd).collect();
        let reply = self.call(&request, &fds)?;
        let mut input = In(&reply);
        let snap = input.snap()?;
        input.finish()?;
        if let Err(error) = index.verify_lease(lease) {
            let _ = self.drop_snapshot(snap.0);
            return Err(error.into());
        }
        Ok(CachedSnapshotReceipt {
            snapshot: snap.0,
            moment: snap.1,
            machine_commitment,
            attachment,
            object_commitment,
        })
    }

    fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        self.call_unit(Out::op(REPLAY).u64(snapshot.0))
    }

    fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        self.call_unit(Out::op(DROP_SNAPSHOT).u64(snapshot.0))
    }

    fn branch_with_service(
        &mut self,
        snapshot: SnapId,
        config: ServiceConfig,
        payloads: Vec<Vec<u8>>,
        effects: Vec<(u64, Effect)>,
        coverage_schedule: Option<CoverageSchedule>,
    ) -> Result<(), Box<dyn Error>> {
        let spec = service_branch_spec(self.seed, config, payloads, effects, coverage_schedule)?;
        self.call_unit(Out::op(BRANCH).u64(snapshot.0).bytes(&spec.encode()))
    }

    fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
        let reply = self.call(Out::op(RUN_UNTIL).u64(deadline), &[])?;
        match decode_reply(&reply) {
            Ok(Some((_, Ok(Reply::Stop(stop)), consumed))) if consumed == reply.len() => Ok(stop),
            _ => Err(malformed("stop reason").into()),
        }
    }

    fn run_resolving(
        &mut self,
        deadline: u64,
        resolution: Resolution,
    ) -> Result<StopReason, Box<dyn Error>> {
        let reply = self.call(
            Out::op(RUN_RESOLVING)
                .u64(deadline)
                .u64(resolution.vtime.0)
                .u64(u64::from(resolution.service))
                .u64(resolution.id.0)
                .bytes(&resolution.answer.0),
            &[],
        )?;
        match decode_reply(&reply) {
            Ok(Some((_, Ok(Reply::Stop(stop)), consumed))) if consumed == reply.len() => Ok(stop),
            _ => Err(malformed("stop reason").into()),
        }
    }

    fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
        self.call_snap(&Out::op(SNAPSHOT))
    }

    fn sdk_events(&mut self) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
        self.sdk_events_since(0)
    }

    fn sdk_events_since(&mut self, offset: u32) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
        let reply = self.call(Out::op(SDK_EVENTS).u64(u64::from(offset)), &[])?;
        let mut input = In(&reply);
        let count = input.len()?;
        offset
            .checked_add(u32::try_from(count).map_err(|_| malformed("SDK event count overflows"))?)
            .ok_or_else(|| malformed("SDK event offset overflows"))?;
        let events = (0..count)
            .map(|_| {
                let moment = input.u64()?;
                let id = u32::try_from(input.u64()?).map_err(|_| malformed("event id"))?;
                Ok((moment, id, input.bytes()?.to_vec()))
            })
            .collect::<io::Result<Vec<_>>>()?;
        input.finish()?;
        Ok(events)
    }

    fn abandoned(&self) -> bool {
        self.failed.get()
    }
}

struct Init {
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
    config: SessionConfig,
    setup_payloads: Vec<Vec<u8>>,
    service: String,
}

fn take_init(input: &mut In<'_>) -> io::Result<Init> {
    let config = take_config(input)?;
    let kernel = input.bytes()?.to_vec();
    let initramfs = input.bytes()?.to_vec();
    let setup_payloads = (0..input.len()?)
        .map(|_| Ok(input.bytes()?.to_vec()))
        .collect::<io::Result<Vec<_>>>()?;
    Ok(Init {
        kernel,
        initramfs,
        config,
        setup_payloads,
        service: input.string()?,
    })
}

type Open<'a> = dyn FnMut(Init) -> Result<Box<dyn ServedSession>, Box<dyn Error>> + 'a;

#[derive(Default)]
struct Served {
    loaded_image_guard: Option<crate::image_identity::LoadedImageGuard>,
    session: Option<Box<dyn ServedSession>>,
}

impl Served {
    fn session(&mut self) -> Result<&mut dyn ServedSession, Box<dyn Error>> {
        Ok(self
            .session
            .as_deref_mut()
            .ok_or("session worker is not initialized")?)
    }

    fn handle(
        &mut self,
        body: &[u8],
        fds: &[OwnedFd],
        open: &mut Open<'_>,
        allocate: &mut dyn FnMut(usize) -> Result<Mapping, Box<dyn Error>>,
    ) -> Result<Out, Box<dyn Error>> {
        let mut input = In(body);
        let op = input.u8()?;
        let mut out = Out::default();
        if op == INIT {
            let init = take_init(&mut input)?;
            input.finish()?;
            if self.session.is_some() {
                return Err("session worker is already initialized".into());
            }
            let session = open(init)?;
            let (setup, at) = session.setup_handle();
            self.session = Some(session);
            out.u64(setup.0).u64(at);
            return Ok(out);
        }
        let session = self.session()?;
        match op {
            EVENT_HISTORY => {
                input.finish()?;
                let history = session.event_history()?;
                out.0.extend_from_slice(&history.count.to_le_bytes());
                out.0.extend_from_slice(&history.digest);
            }
            STATE_HASH => {
                input.finish()?;
                out.0.extend_from_slice(&session.state_hash()?);
            }
            CONSOLE_TAIL => {
                input.finish()?;
                out.0 = session.console_tail()?;
            }
            TELEMETRY => {
                input.finish()?;
                let mut counters = session.telemetry_counters();
                if let Some(guard) = &self.loaded_image_guard {
                    counters.extend(guard.counters("guard.child.loop"));
                }
                out.len(counters.len());
                for (name, value) in &counters {
                    out.bytes(name.as_bytes()).u64(*value);
                }
            }
            OWNED_PAGES => {
                let snap = SnapId(input.u64()?);
                input.finish()?;
                out.option(session.snapshot_owned_pages(snap));
            }
            STORE_BYTES => {
                input.finish()?;
                out.u64(session.store_bytes().unwrap_or(0));
            }
            EXPORT => {
                let parent = input.option()?.map(SnapId);
                let target = SnapId(input.u64()?);
                let attachment = SnapshotAttachment {
                    domain: input.bytes()?.to_vec(),
                    bytes: input.bytes()?.to_vec(),
                };
                input.finish()?;
                if !fds.is_empty() {
                    return Err(malformed("export descriptors").into());
                }
                session.export_delta_into(parent, target, &attachment, allocate)?;
            }
            IMPORT => {
                let near = SnapId(input.u64()?);
                let count = input.len()?;
                if count != fds.len() {
                    return Err(malformed("an import carries one descriptor per extent").into());
                }
                let mappings = fds
                    .iter()
                    .map(|fd| Mapping::new(fd.as_fd(), input.len()?, input.len()?, false))
                    .collect::<io::Result<Vec<_>>>()?;
                input.finish()?;
                let extents: Vec<&[u8]> = mappings.iter().map(Mapping::bytes).collect();
                let (snap, at) = session.import_extents(&extents, near)?;
                out.u64(snap.0).u64(at);
            }
            SNAPSHOT => {
                input.finish()?;
                let (snap, at) = session.snapshot()?;
                out.u64(snap.0).u64(at);
            }
            DROP_SNAPSHOT => {
                let snap = SnapId(input.u64()?);
                input.finish()?;
                session.drop_snapshot(snap)?;
            }
            REPLAY => {
                let snap = SnapId(input.u64()?);
                input.finish()?;
                session.replay_snapshot(snap)?;
            }
            BRANCH => {
                let snap = SnapId(input.u64()?);
                let spec = InputSpec::decode_reproducer(input.bytes()?)
                    .map_err(|error| format!("branch input: {error}"))?;
                input.finish()?;
                session.branch_input(snap, &spec)?;
            }
            RUN_UNTIL => {
                let deadline = input.u64()?;
                input.finish()?;
                let stop = session.run_until(deadline)?;
                encode_reply(0, &Ok(Reply::Stop(stop)), &mut out.0)
                    .map_err(|error| format!("encode stop reason: {error}"))?;
            }
            RUN_RESOLVING => {
                let deadline = input.u64()?;
                let vtime = Moment(input.u64()?);
                let service = u16::try_from(input.u64()?)
                    .map_err(|_| malformed("service namespace overflows"))?;
                let id = DecisionId(input.u64()?);
                let answer = Answer(input.bytes()?.to_vec());
                input.finish()?;
                let stop = session.run_resolving(
                    deadline,
                    Resolution {
                        vtime,
                        service,
                        id,
                        answer,
                    },
                )?;
                encode_reply(0, &Ok(Reply::Stop(stop)), &mut out.0)
                    .map_err(|error| format!("encode stop reason: {error}"))?;
            }
            SDK_EVENTS => {
                let offset = u32::try_from(input.u64()?)
                    .map_err(|_| malformed("SDK event offset overflows"))?;
                input.finish()?;
                if !fds.is_empty() {
                    return Err(malformed("SDK events carries no descriptors").into());
                }
                let events = session.sdk_events_since(offset)?;
                out.len(events.len());
                for (moment, id, bytes) in &events {
                    out.u64(*moment).u64(u64::from(*id)).bytes(bytes);
                }
            }
            _ => return Err(malformed("unknown request").into()),
        }
        Ok(out)
    }
}

fn status(result: Result<Out, Box<dyn Error>>) -> Vec<u8> {
    match result {
        Ok(out) => {
            let mut body = Vec::with_capacity(out.0.len() + 1);
            body.push(OK);
            body.extend_from_slice(&out.0);
            body
        }
        Err(error) => {
            let mut out = Out::op(FAILED);
            match error.downcast_ref::<SessionError>() {
                Some(SessionError::Hung(limit)) => {
                    out.u8(HUNG)
                        .u64(u64::try_from(limit.as_nanos()).unwrap_or(u64::MAX));
                }
                Some(SessionError::Abandoned) => {
                    out.u8(ABANDONED);
                }
                Some(SessionError::Infrastructure(detail)) => {
                    out.u8(INFRASTRUCTURE).bytes(detail.as_bytes());
                }
                _ => {
                    out.u8(OTHER).bytes(error.to_string().as_bytes());
                }
            }
            out.0
        }
    }
}

fn serve(
    stream: &UnixStream,
    open: &mut Open<'_>,
    loaded_image_guard: crate::image_identity::LoadedImageGuard,
) -> Result<(), Box<dyn Error>> {
    let mut served = Served {
        loaded_image_guard: Some(loaded_image_guard.clone()),
        ..Served::default()
    };
    loaded_image_guard.check_at(crate::image_identity::GuardSite::ChildReceive)?;
    loop {
        let (body, fds) = match recv(stream) {
            Ok(frame) => frame,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut requested = false;
        let mut allocation_broken = None;
        let mut allocate = |len| {
            let result: Result<Option<Mapping>, Box<dyn Error>> = (|| {
                if std::mem::replace(&mut requested, true) {
                    return Err(malformed("repeated allocation").into());
                }
                loaded_image_guard.check_at(crate::image_identity::GuardSite::ChildExtentBefore)?;
                send(stream, &Out::op(NEED_EXTENT).len(len).0, &[])?;
                loaded_image_guard.check_at(crate::image_identity::GuardSite::ChildExtentSent)?;
                let (body, fds) = recv_with_limit(stream, EXPORT_FRAME_LIMIT, None)?;
                loaded_image_guard
                    .check_at(crate::image_identity::GuardSite::ChildExtentReceived)?;
                let mut input = In(&body);
                match input.u8()? {
                    EXTENT_READY => {
                        let offset = input.len()?;
                        if input.len()? != len {
                            return Err(malformed("destination length differs").into());
                        }
                        input.finish()?;
                        let [fd] = fds.as_slice() else {
                            return Err(malformed("destination requires one descriptor").into());
                        };
                        Ok(Some(Mapping::new(fd.as_fd(), offset, len, true)?))
                    }
                    EXTENT_DENIED => {
                        input.finish()?;
                        if !fds.is_empty() {
                            return Err(malformed("denial descriptors").into());
                        }
                        Ok(None)
                    }
                    _ => Err(malformed("expected allocation response").into()),
                }
            })();
            match result {
                Ok(Some(mapping)) => Ok(mapping),
                Ok(None) => Err("destination allocation denied".into()),
                Err(error) => {
                    allocation_broken = Some(error.to_string());
                    Err(error)
                }
            }
        };
        let reply = status(served.handle(&body, &fds, open, &mut allocate));
        if let Some(cause) = allocation_broken {
            return Err(malformed(&format!("allocation exchange failed: {cause}")).into());
        }
        loaded_image_guard.check_at(crate::image_identity::GuardSite::ChildHandled)?;
        drop(fds);
        send(stream, &reply, &[])?;
        loaded_image_guard.check_at(crate::image_identity::GuardSite::ChildSent)?;
    }
}

fn inherited_stream() -> Result<UnixStream, Box<dyn Error>> {
    let raw: RawFd = std::env::var(WORKER_FD_ENV)?.parse()?;
    set_cloexec(raw, true)?;
    // SAFETY: the launcher names in the environment the one descriptor it passed to this
    // process, and nothing else in this process owns it.
    Ok(unsafe { UnixStream::from_raw_fd(raw) })
}

pub fn serve_inherited(
    services: impl Fn(&str) -> Option<ServiceFactory>,
) -> Result<(), Box<dyn Error>> {
    serve_inherited_with_open(&mut |init: Init| {
        let factory = services(&init.service)
            .ok_or_else(|| format!("unknown session service {}", init.service))?;
        let mut session = Session::new_with_config_and_payloads(
            &init.kernel,
            &init.initramfs,
            init.config,
            init.setup_payloads,
        )?;
        session.set_service_factory(factory);
        Ok(Box::new(session) as Box<dyn ServedSession>)
    })
}

fn serve_inherited_with_open(open: &mut Open<'_>) -> Result<(), Box<dyn Error>> {
    crate::image_identity::reject_loader_overrides()?;
    let stream = inherited_stream()?;
    let (identity, loaded_image_guard) = crate::image_identity::LoadedImageGuard::identity()?;
    let mut hello = Out::default();
    hello.bytes(IMAGE_HELLO).bytes(identity.as_bytes());
    loaded_image_guard.check_at(crate::image_identity::GuardSite::Admission)?;
    send(&stream, &hello.0, &[])?;
    serve(&stream, open, loaded_image_guard)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use control_proto::Moment;

    use super::*;
    use crate::cache::{
        LocalIndex,
        extent::{extent_len, read_extent, resolve, write_extent},
    };

    const HANG: u64 = u64::MAX;
    const HOST_FAILURE: u64 = u64::MAX - 1;
    const GUEST_PANIC: u64 = u64::MAX - 2;
    const GUEST_SHUTDOWN: u64 = u64::MAX - 3;

    #[derive(Debug, Default)]
    struct Fake {
        next: u64,
        states: BTreeMap<u64, u8>,
        current: u8,
        runs: u64,
        quarantine_on_import: Option<Arc<LocalIndex>>,
        sdk_history: Option<Vec<SdkEvent>>,
    }

    impl Fake {
        fn open(init: Init) -> Result<Box<dyn ServedSession>, Box<dyn Error>> {
            Self::open_quarantining(init, None)
        }

        fn open_quarantining(
            init: Init,
            quarantine_on_import: Option<Arc<LocalIndex>>,
        ) -> Result<Box<dyn ServedSession>, Box<dyn Error>> {
            if init.service != "fake" {
                return Err(format!("unknown session service {}", init.service).into());
            }
            let mut fake = Self {
                next: 1,
                current: init.kernel[0],
                quarantine_on_import,
                ..Self::default()
            };
            fake.states.insert(1, fake.current);
            Ok(Box::new(fake))
        }

        fn state(&self, snapshot: SnapId) -> Result<u8, Box<dyn Error>> {
            Ok(*self.states.get(&snapshot.0).ok_or("unknown snapshot")?)
        }
    }

    impl SearchSession for Fake {
        fn setup_handle(&self) -> (SnapId, u64) {
            (SnapId(1), 10)
        }

        fn state_hash(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
            Ok([self.current; 32])
        }

        fn event_history(&mut self) -> Result<control_proto::EventHistory, Box<dyn Error>> {
            Ok(control_proto::EventHistory {
                count: self.runs,
                digest: [self.current; 32],
            })
        }

        fn console_tail(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
            Ok(b"tail".to_vec())
        }

        fn telemetry_counters(&self) -> Vec<(String, u64)> {
            vec![
                ("runs".to_owned(), self.runs),
                ("guard.session.fixture.dispatches".to_owned(), self.runs),
            ]
        }

        fn snapshot_owned_pages(&self, snapshot: SnapId) -> Option<u64> {
            self.states.contains_key(&snapshot.0).then_some(1)
        }

        fn store_bytes(&self) -> Option<u64> {
            Some(self.states.len() as u64 * 4096)
        }

        fn publish_snapshot(
            &self,
            _: &dyn CacheIndex,
            _: Namespace,
            _: &[u8],
            _: Option<(SnapId, &Lease)>,
            _: SnapId,
            _: u64,
        ) -> Result<Lease, Box<dyn Error>> {
            Err("a served session publishes through its coordinator".into())
        }

        fn import_cached(
            &mut self,
            _: &dyn CacheIndex,
            _: &Lease,
            _: SnapId,
        ) -> Result<(SnapId, u64), Box<dyn Error>> {
            Err("a served session imports through its coordinator".into())
        }

        fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
            self.current = self.state(snapshot)?;
            Ok(())
        }

        fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
            self.states
                .remove(&snapshot.0)
                .map(drop)
                .ok_or_else(|| "unknown snapshot".into())
        }

        fn branch_with_service(
            &mut self,
            _: SnapId,
            _: ServiceConfig,
            _: Vec<Vec<u8>>,
            _: Vec<(u64, Effect)>,
            _: Option<CoverageSchedule>,
        ) -> Result<(), Box<dyn Error>> {
            Err("a served session branches from an input".into())
        }

        fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
            if deadline == HANG {
                return Err(SessionError::Hung(Duration::from_secs(3)).into());
            }
            if deadline == HOST_FAILURE {
                return Err(SessionError::Infrastructure("owned host exited87".into()).into());
            }
            if let Some(kind) = match deadline {
                GUEST_PANIC => Some(control_proto::CrashKind::Panic),
                GUEST_SHUTDOWN => Some(control_proto::CrashKind::Shutdown),
                _ => None,
            } {
                return Ok(StopReason::Crash {
                    vtime: Moment(40),
                    info: control_proto::CrashInfo {
                        kind,
                        detail: b"framed guest stop".to_vec(),
                    },
                });
            }
            self.current = self.current.wrapping_add(1);
            self.runs += 1;
            Ok(StopReason::Deadline {
                vtime: Moment(deadline),
            })
        }

        fn run_resolving(
            &mut self,
            deadline: u64,
            resolution: Resolution,
        ) -> Result<StopReason, Box<dyn Error>> {
            if resolution.vtime != Moment(12)
                || resolution.service != 19
                || resolution.id != DecisionId(7)
            {
                return Err("wrong service decision".into());
            }
            let [1, byte] = resolution.answer.0.as_slice() else {
                return Err("wrong service answer".into());
            };
            self.current = *byte;
            self.runs += 1;
            Ok(StopReason::Deadline {
                vtime: Moment(deadline),
            })
        }

        fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
            self.next += 1;
            self.states.insert(self.next, self.current);
            Ok((SnapId(self.next), self.next * 10))
        }

        fn sdk_events(&mut self) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
            Ok(self
                .sdk_history
                .clone()
                .unwrap_or_else(|| vec![(5, 7, vec![self.current])]))
        }

        fn abandoned(&self) -> bool {
            false
        }
    }

    impl ServedSession for Fake {
        fn export_delta_into(
            &self,
            _: Option<SnapId>,
            target: SnapId,
            attachment: &SnapshotAttachment,
            allocate: &mut dyn FnMut(usize) -> Result<Mapping, Box<dyn Error>>,
        ) -> Result<(), Box<dyn Error>> {
            let fill = self.state(target)?;
            let policy = ServiceConfig::default().encode();
            #[cfg(target_arch = "x86_64")]
            let vm_state = {
                let mut state = vm_state::VmState::default();
                state.xsave.0 = vec![0; 576];
                vmm_backend::arch::x86::canonicalize_xsave(&mut state.xsave.0);
                state.regs.rax = u64::from(fill);
                state.encode()?
            };
            #[cfg(target_arch = "aarch64")]
            let vm_state = {
                let mut state = vm_state::Arm64VmState::default();
                state.regs.x[0] = u64::from(fill);
                state.encode()?
            };
            let mut sidecar = Vec::new();
            sidecar.extend_from_slice(b"HMSESS01");
            sidecar.extend_from_slice(&[0; 32]);
            sidecar.extend_from_slice(b"HMSSNAP1");
            sidecar.extend_from_slice(&10u16.to_le_bytes());
            sidecar.extend_from_slice(&4u16.to_le_bytes());
            for len in [vm_state.len() as u64, 0, policy.len() as u64, 1, 0] {
                sidecar.extend_from_slice(&len.to_le_bytes());
            }
            sidecar.extend_from_slice(&[0; 4 * 8]);
            sidecar.extend_from_slice(&[0; 32]);
            sidecar.extend_from_slice(&[fill; 32]);
            sidecar.extend_from_slice(&vm_state);
            sidecar.extend_from_slice(&policy);
            sidecar.push(0);
            sidecar.push(0);
            let machine = super::super::live::raw_target_commitment(&sidecar)?;
            let sidecar = super::super::attachment::encode(&sidecar, machine, attachment)?;
            let mut out = allocate(extent_len(1, 0, sidecar.len()).unwrap())?;
            write_extent(
                out.bytes_mut(),
                &[(0, &[fill; 32], &[fill; 4096])],
                &[],
                &sidecar,
            )?;
            drop(out);
            Ok(())
        }

        fn import_extents(
            &mut self,
            extents: &[&[u8]],
            _: SnapId,
        ) -> Result<(SnapId, u64), Box<dyn Error>> {
            let deltas = extents
                .iter()
                .map(|extent| read_extent(extent))
                .collect::<Result<Vec<_>, _>>()?;
            self.current = resolve(&deltas)?.pages.last().ok_or("no pages")?.2[0];
            if let Some(index) = self.quarantine_on_import.take() {
                let mut extent = index.extent(extent_len(0, 0, 0).unwrap())?;
                write_extent(extent.bytes_mut(), &[], &[], &[])?;
                assert!(matches!(
                    index.publish(namespace(), b"key", None, extent, [0xee; 32], 0),
                    Err(crate::cache::CacheError::DivergentDuplicate { .. })
                ));
            }
            self.snapshot()
        }

        fn branch_input(&mut self, snapshot: SnapId, _: &InputSpec) -> Result<(), Box<dyn Error>> {
            self.current = self.state(snapshot)?;
            Ok(())
        }
    }

    #[cfg(target_os = "macos")]
    fn live_task_descriptors() -> std::collections::BTreeSet<RawFd> {
        let candidates: Vec<_> = std::fs::read_dir("/dev/fd")
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_str()
                    .unwrap()
                    .parse::<RawFd>()
                    .unwrap()
            })
            .collect();
        candidates
            .into_iter()
            .filter(|fd| {
                // SAFETY: F_GETFD queries a candidate number without taking ownership or modifying it.
                unsafe { libc::fcntl(*fd, libc::F_GETFD) >= 0 }
            })
            .collect()
    }

    #[cfg(target_os = "macos")]
    fn send_two_rights_headers(
        stream: &UnixStream,
        descriptor: BorrowedFd<'_>,
    ) -> io::Result<usize> {
        let space = control_space(1);
        let mut control = crate::ancillary::tests::two_rights_control(descriptor.as_raw_fd());
        let mut byte = [1_u8];
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: 1,
        };
        // SAFETY: the integer/pointer fields of msghdr have a valid empty zero representation.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &raw mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = (space * 2) as _;
        // SAFETY: message describes only the live byte/iovec and initialized control allocation above.
        let result = unsafe { libc::sendmsg(stream.as_raw_fd(), &raw const message, 0) };
        usize::try_from(result).map_err(|_| io::Error::last_os_error())
    }

    #[test]
    #[cfg(all(target_os = "macos", not(miri)))]
    #[allow(clippy::disallowed_methods)]
    fn darwin_control_bound_recovers_all_installed_rights_before_protocol_refusal() {
        const CHILD: &str = "HARMONY_DARWIN_RIGHTS_BOUND_CHILD";
        if std::env::var_os(CHILD).is_none() {
            const TEST: &str = concat!(
                "session::worker::tests::",
                "darwin_control_bound_recovers_all_installed_rights_before_protocol_refusal"
            );
            let output = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg(TEST)
                .arg("--nocapture")
                .env(CHILD, "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success()
                    && stdout.lines().any(|line| line.trim() == "running 1 test"),
                "{}\n{stdout}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            print!("{stdout}");
            return;
        }
        // SAFETY: rlimit contains integer fields with a valid zero representation.
        let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
        // SAFETY: getrlimit writes one live correctly aligned rlimit without retaining its pointer.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
            0
        );
        assert!(
            limit.rlim_max >= 1024,
            "the isolated qualification needs room for one maximum-size rights message"
        );
        if limit.rlim_cur < 1024 {
            limit.rlim_cur = 1024;
            // SAFETY: the initialized rlimit raises only this sacrificial child's soft FD limit within its existing hard limit.
            assert_eq!(
                unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) },
                0
            );
        }
        let baseline = live_task_descriptors();
        {
            let (sender, receiver) = UnixStream::pair().unwrap();
            let descriptor = tempfile::tempfile().unwrap();
            let active = live_task_descriptors();
            for count in [64, 65, 72] {
                let rights = vec![descriptor.as_fd(); count];
                assert_eq!(send_with_fds(&sender, &[1], &rights).unwrap(), 1);
                let received = recv_with_fds(
                    &receiver,
                    &mut [0; 1],
                    Some(Instant::now() + Duration::from_secs(1)),
                );
                if count == MAX_FDS {
                    assert_eq!(received.unwrap().1.len(), MAX_FDS);
                } else {
                    let error = received.unwrap_err();
                    assert_eq!(
                        live_task_descriptors(),
                        active,
                        "installed descriptor leak at {count}"
                    );
                    assert!(
                        error.to_string().contains("too many descriptors"),
                        "{error}"
                    );
                }
                assert_eq!(
                    live_task_descriptors(),
                    active,
                    "installed descriptor leak at {count}"
                );
            }
            let mut maximum = 0;
            for count in 73..=520 {
                let rights = vec![descriptor.as_fd(); count];
                match send_with_fds(&sender, &[1], &rights) {
                    Ok(1) => {
                        let error = recv_with_fds(
                            &receiver,
                            &mut [0; 1],
                            Some(Instant::now() + Duration::from_secs(1)),
                        )
                        .unwrap_err();
                        assert!(
                            error.to_string().contains("too many descriptors"),
                            "{count}: {error}"
                        );
                        maximum = count;
                    }
                    Err(error) => {
                        assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
                        break;
                    }
                    other => panic!("unexpected send result {other:?}"),
                }
                assert_eq!(
                    live_task_descriptors(),
                    active,
                    "installed descriptor leak at {count}"
                );
            }
            assert!(
                (73..520).contains(&maximum),
                "a bounded maximum and oversized refusal must both execute"
            );
            println!("DARWIN_RIGHTS_MAXIMUM_ACCEPTED={maximum}");
            assert_eq!(
                send_two_rights_headers(&sender, descriptor.as_fd())
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::EINVAL)
            );
            assert_eq!(live_task_descriptors(), active);
            for value in [1, 2, 3] {
                assert_eq!(
                    send_with_fds(&sender, &[value], &[descriptor.as_fd()]).unwrap(),
                    1
                );
            }
            for value in [1, 2, 3] {
                let mut bytes = [0; 16];
                let (size, rights) = recv_with_fds(
                    &receiver,
                    &mut bytes,
                    Some(Instant::now() + Duration::from_secs(1)),
                )
                .unwrap();
                assert_eq!(&bytes[..size], &[value]);
                assert_eq!(rights.len(), 1);
                drop(rights);
                assert_eq!(live_task_descriptors(), active);
            }
            assert_eq!(
                send_with_fds(&sender, &[], &[descriptor.as_fd()]).unwrap(),
                0
            );
            assert_eq!(
                send_with_fds(&sender, &[9], &[descriptor.as_fd()]).unwrap(),
                1
            );
            let (size, rights) = recv_with_fds(
                &receiver,
                &mut [0; 16],
                Some(Instant::now() + Duration::from_secs(1)),
            )
            .unwrap();
            assert_eq!(size, 0);
            assert_eq!(rights.len(), 1);
            drop(rights);
            let mut bytes = [0; 16];
            let (size, rights) = recv_with_fds(
                &receiver,
                &mut bytes,
                Some(Instant::now() + Duration::from_secs(1)),
            )
            .unwrap();
            assert_eq!(&bytes[..size], &[9]);
            assert_eq!(rights.len(), 1);
            drop(rights);
            assert_eq!(live_task_descriptors(), active);
        }
        assert_eq!(live_task_descriptors(), baseline);
    }

    #[test]
    #[cfg(all(target_os = "macos", not(miri)))]
    #[allow(clippy::disallowed_methods)]
    fn darwin_frame_remainders_own_and_refuse_unexpected_rights() {
        const CHILD: &str = "HARMONY_DARWIN_FRAME_RIGHTS_CHILD";
        if std::env::var_os(CHILD).is_none() {
            const TEST: &str = concat!(
                "session::worker::tests::",
                "darwin_frame_remainders_own_and_refuse_unexpected_rights"
            );
            let output = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg(TEST)
                .arg("--nocapture")
                .env(CHILD, "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success()
                    && stdout.lines().any(|line| line.trim() == "running 1 test"),
                "{}\n{stdout}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            print!("{stdout}");
            return;
        }
        let baseline = live_task_descriptors();
        for header_remainder in [true, false] {
            for timed in [true, false] {
                for count in [1, MAX_FDS + 8] {
                    {
                        let (mut sender, receiver) = UnixStream::pair().unwrap();
                        let descriptor = tempfile::tempfile().unwrap();
                        let active = live_task_descriptors();
                        let mut header = [0_u8; HEADER];
                        header[..8].copy_from_slice(&2_u64.to_le_bytes());
                        if header_remainder {
                            sender.write_all(&header[..1]).unwrap();
                            assert_eq!(
                                send_with_fds(
                                    &sender,
                                    &header[1..],
                                    &vec![descriptor.as_fd(); count]
                                )
                                .unwrap(),
                                HEADER - 1
                            );
                        } else {
                            sender.write_all(&header).unwrap();
                            sender.write_all(&[1]).unwrap();
                            assert_eq!(
                                send_with_fds(&sender, &[2], &vec![descriptor.as_fd(); count])
                                    .unwrap(),
                                1
                            );
                        }
                        sender.shutdown(std::net::Shutdown::Write).unwrap();
                        let deadline = timed.then(|| Instant::now() + Duration::from_secs(1));
                        let result = recv_with_limit(&receiver, 2, deadline);
                        assert_eq!(
                            live_task_descriptors(),
                            active,
                            "unreported rights: header={header_remainder} timed={timed} count={count}"
                        );
                        let error = result.unwrap_err();
                        assert!(
                            error.to_string().contains(if count == 1 {
                                "unexpected descriptors"
                            } else {
                                "too many descriptors"
                            }),
                            "{error}"
                        );
                    }
                    assert_eq!(live_task_descriptors(), baseline);
                }
            }
        }
        {
            let (mut sender, receiver) = UnixStream::pair().unwrap();
            let mut header = [0_u8; HEADER];
            header[..8].copy_from_slice(&2_u64.to_le_bytes());
            sender.write_all(&header[..1]).unwrap();
            sender.write_all(&header[1..]).unwrap();
            sender.write_all(&[1, 2]).unwrap();
            assert_eq!(recv_with_limit(&receiver, 2, None).unwrap().0, [1, 2]);
            drop(sender);
            assert_eq!(
                recv_with_limit(&receiver, 2, None).unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
        }
        {
            let (mut sender, receiver) = UnixStream::pair().unwrap();
            assert_eq!(
                read_frame_exact(&receiver, &mut [0; 1], Some(Instant::now()))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::TimedOut
            );
            sender.write_all(&[7]).unwrap();
            sender.shutdown(Shutdown::Write).unwrap();
            let mut body = [0; 2];
            assert_eq!(
                read_frame_exact(
                    &receiver,
                    &mut body,
                    Some(Instant::now() + Duration::from_secs(1))
                )
                .unwrap_err()
                .kind(),
                io::ErrorKind::UnexpectedEof
            );
            assert_eq!(body[0], 7);
        }
        assert_eq!(live_task_descriptors(), baseline);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn truncated_rights_preserve_unrelated_descriptors_and_close_received_handles() {
        const CHILD: &str = "HARMONY_TRUNCATED_RIGHTS_CHILD";
        if std::env::var_os(CHILD).is_none() {
            const CASE: &str = concat!(
                "session::worker::tests::",
                "truncated_rights_preserve_unrelated_descriptors_and_close_received_handles"
            );
            let output = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg(CASE)
                .arg("--nocapture")
                .env(CHILD, "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success()
                    && stdout.lines().any(|line| line.trim() == "running 1 test"),
                "{}\n{stdout}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (left, right) = UnixStream::pair().unwrap();
        let (rights, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let sentinel = tempfile::tempfile().unwrap();
        // SAFETY: fcntl F_GETFD only queries the existing descriptor and does not modify it.
        let stdin_before = unsafe { libc::fcntl(0, libc::F_GETFD) };
        assert!(
            stdin_before >= 0,
            "descriptor zero must be open for this regression"
        );
        let capacity = (control_space(MAX_FDS) - size_of::<libc::cmsghdr>()) / size_of::<RawFd>();
        let excess = tempfile::tempfile().unwrap();
        let mut fds = vec![rights.as_fd(); capacity];
        fds.push(excess.as_fd());
        assert_eq!(send_with_fds(&left, &[1], &fds).unwrap(), 1);
        let error = recv_with_fds_control_space(&right, &mut [0; 1], None, control_space(MAX_FDS))
            .unwrap_err();
        assert!(
            error.to_string().contains("descriptors were truncated"),
            "{error}"
        );
        // SAFETY: fcntl F_GETFD only queries descriptor zero, just as before the receive.
        assert_eq!(
            unsafe { libc::fcntl(0, libc::F_GETFD) },
            stdin_before,
            "truncated padding must not become descriptor zero"
        );
        sentinel.metadata().unwrap();
        drop(fds);
        drop(rights);
        drop(excess);
        drop(left);
        drop(right);
        assert_eq!(
            peer.read(&mut [0; 1]).unwrap(),
            0,
            "all real received rights must close on refusal"
        );
    }

    fn config() -> SessionConfig {
        SessionConfig {
            wall_limit: Some(Duration::from_secs(9)),
            identity_tag: "worker-test".to_owned(),
            ..SessionConfig::default()
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn worker_entry_and_launch_refuse_loader_overrides() {
        const CHILD_ENV: &str = "HARMONY_LOADER_WORKER_TEST_CHILD";
        #[cfg(target_os = "linux")]
        let override_name = "LD_LIBRARY_PATH";
        #[cfg(target_os = "macos")]
        let override_name = "DYLD_LIBRARY_PATH";

        if let Some(which) = std::env::var_os(CHILD_ENV) {
            let error = if which == "launch" {
                WorkerLauncher {
                    program: std::env::current_exe().unwrap(),
                    args: vec!["--version".into()],
                    expected_image: String::new(),
                    expected_main: String::new(),
                    loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
                }
                .spawn()
                .unwrap_err()
                .to_string()
            } else {
                serve_inherited(|_| None).unwrap_err().to_string()
            };
            assert!(error.contains(override_name), "{error}");
            return;
        }

        for which in ["launch", "entry"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("session::worker::tests::worker_entry_and_launch_refuse_loader_overrides")
                .env(CHILD_ENV, which)
                .env(override_name, std::env::temp_dir())
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success()
                    && stdout.lines().any(|line| line.trim() == "running 1 test"),
                "{which}: {}\nstdout: {stdout}\nstderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn worker_config_carries_the_named_guest_contract_and_rejects_unknown_profiles() {
        let config = config().with_nested_host();
        let mut bytes = Out::default();
        put_config(&mut bytes, &config);
        let mut input = In(&bytes.0);
        assert_eq!(take_config(&mut input).unwrap(), config);
        bytes.0[0] = 2;
        assert!(take_config(&mut In(&bytes.0)).is_err());
    }

    fn threaded(start: u8) -> (WorkerSession, std::thread::JoinHandle<()>) {
        threaded_quarantining(start, None)
    }

    fn threaded_quarantining(
        start: u8,
        quarantine_on_import: Option<Arc<LocalIndex>>,
    ) -> (WorkerSession, std::thread::JoinHandle<()>) {
        threaded_with_abort(start, quarantine_on_import, false)
    }

    fn threaded_with_abort(
        start: u8,
        quarantine_on_import: Option<Arc<LocalIndex>>,
        aborted_export: bool,
    ) -> (WorkerSession, std::thread::JoinHandle<()>) {
        let (parent, child) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let result = serve(
                &child,
                &mut |init| Fake::open_quarantining(init, quarantine_on_import.clone()),
                crate::image_identity::LoadedImageGuard::fixture(),
            );
            match result {
                Err(error) if aborted_export => {
                    assert!(error.to_string().contains("allocation exchange failed"))
                }
                result => result.unwrap(),
            }
        });
        let session = WorkerSession::connect(
            WorkerConnection {
                stream: parent,
                process: None,
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
            },
            &[start],
            b"i",
            &config(),
            &[],
            "fake",
        )
        .unwrap();
        (session, server)
    }

    fn spawned(start: u8) -> WorkerSession {
        let launcher = WorkerLauncher::current_exe(vec![
            "--exact".into(),
            "session::worker::tests::serve_fake_worker".into(),
        ])
        .unwrap();
        WorkerSession::spawn(&launcher, &[start], b"i", &config(), &[], "fake").unwrap()
    }

    #[test]
    fn worker_transports_ordered_event_history() {
        let (mut session, server) = threaded_quarantining(3, None);
        assert_eq!(
            session.event_history().unwrap(),
            control_proto::EventHistory {
                count: 0,
                digest: [3; 32]
            }
        );
        session.run_until(20).unwrap();
        assert_eq!(session.event_history().unwrap().count, 1);
        drop(session);
        server.join().unwrap();
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn sealed_host_reexecs_a_public_worker() {
        if !crate::image_identity::strict_test_host_ready() {
            return;
        }
        let mut session = spawned(3);
        assert_eq!(session.setup_handle(), (SnapId(1), 10));
        assert_eq!(session.state_hash().unwrap(), [3; 32]);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn sealed_worker_sigkill_is_infrastructure_and_guest_stops_are_framed() {
        use std::os::unix::process::ExitStatusExt;

        if !crate::image_identity::strict_test_host_ready() {
            return;
        }
        let mut session = spawned(3);
        for (deadline, expected) in [
            (GUEST_PANIC, control_proto::CrashKind::Panic),
            (GUEST_SHUTDOWN, control_proto::CrashKind::Shutdown),
        ] {
            let stop = session.run_until(deadline).unwrap();
            assert!(matches!(stop, StopReason::Crash { info, .. } if info.kind == expected));
            assert!(!session.abandoned());
        }
        let pid = session.process_id().unwrap();
        assert!(
            Command::new("/bin/kill")
                .args(["-KILL", &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
        let error = session.run_until(80).unwrap_err();
        assert!(
            matches!(error.downcast_ref::<SessionError>(),
            Some(SessionError::Infrastructure(detail)) if detail.contains("session worker disconnected")),
            "{error}"
        );
        assert!(session.abandoned());
        assert_eq!(
            session.process.as_mut().unwrap().wait().unwrap().signal(),
            Some(9)
        );
        assert!(matches!(
            session
                .state_hash()
                .unwrap_err()
                .downcast_ref::<SessionError>(),
            Some(SessionError::Abandoned)
        ));
        drop(session);
        eprintln!(
            "worker transport provenance: SIGKILL infrastructure; framed guest panic/shutdown preserved"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn shared_launcher_forks_session_telemetry_owners() {
        if !crate::image_identity::strict_test_host_ready() {
            return;
        }
        let launcher = WorkerLauncher::current_exe(vec![
            "--exact".into(),
            "session::worker::tests::serve_fake_worker".into(),
        ])
        .unwrap();
        let before = launcher.telemetry_counters();
        let mut first =
            WorkerSession::spawn(&launcher, &[3], b"i", &config(), &[], "fake").unwrap();
        first.state_hash().unwrap();
        let first_before = first.loaded_image_guard.counters("test");
        let second = WorkerSession::spawn(&launcher, &[3], b"i", &config(), &[], "fake").unwrap();
        assert_eq!(first.loaded_image_guard.counters("test"), first_before);
        assert_eq!(launcher.telemetry_counters(), before);
        let second_before = second.loaded_image_guard.counters("test");
        first.state_hash().unwrap();
        assert_eq!(second.loaded_image_guard.counters("test"), second_before);
    }

    fn kill(session: &WorkerSession) {
        let pid = libc::pid_t::try_from(session.process_id().unwrap()).unwrap();
        // SAFETY: kill only signals the child process this test spawned.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        let mut status = 0;
        // SAFETY: waitpid reaps only the child this test spawned and writes its status into a live local.
        assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
    }

    fn namespace() -> Namespace {
        Namespace::new(&[b"worker-test"])
    }

    #[test]
    fn serve_fake_worker() {
        if std::env::var_os(WORKER_FD_ENV).is_none() {
            return;
        }
        serve_inherited_with_open(&mut Fake::open).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[allow(clippy::disallowed_methods)]
    fn worker_image_hello_has_a_deadline_and_size_limit() {
        let (receiver, sender) = UnixStream::pair().unwrap();
        let deadline = Instant::now() + Duration::from_millis(50);
        let error = recv_with_limit(&receiver, IMAGE_HELLO_LIMIT, Some(deadline)).unwrap_err();
        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ),
            "{error}"
        );
        receiver.set_read_timeout(None).unwrap();
        let mut header = [0_u8; HEADER];
        header[..8].copy_from_slice(&((IMAGE_HELLO_LIMIT + 1) as u64).to_le_bytes());
        (&sender).write_all(&header).unwrap();
        let error = recv_with_limit(
            &receiver,
            IMAGE_HELLO_LIMIT,
            Some(Instant::now() + Duration::from_secs(1)),
        )
        .unwrap_err();
        assert!(error.to_string().contains("frame length exceeds limit"));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[allow(clippy::disallowed_methods)]
    fn worker_launcher_refuses_a_child_without_image_hello() {
        if !crate::image_identity::strict_test_host_ready() {
            return;
        }
        if crate::image_identity::rerun_clean_loader_test(
            "session::worker::tests::worker_launcher_refuses_a_child_without_image_hello",
        ) {
            return;
        }
        let launcher = WorkerLauncher::current_exe(vec!["--help".into()]).unwrap();
        let started = Instant::now();
        let error = launcher.spawn().unwrap_err();
        assert!(started.elapsed() < IMAGE_HELLO_TIMEOUT, "{error}");
    }

    #[test]
    fn frames_carry_descriptors_and_split_headers() {
        let (left, right) = UnixStream::pair().unwrap();
        let (file, _) = UnixStream::pair().unwrap();
        let body = vec![7_u8; 1 << 20];
        let sender = std::thread::spawn(move || {
            send(&left, &body, &[file.as_fd(), file.as_fd()]).unwrap();
            send(&left, b"", &[]).unwrap();
        });
        let (body, fds) = recv(&right).unwrap();
        assert_eq!((body.len(), body[0], fds.len()), (1 << 20, 7, 2));
        let (body, fds) = recv(&right).unwrap();
        assert!(body.is_empty() && fds.is_empty());
        sender.join().unwrap();
        assert_eq!(
            recv(&right).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn sdk_suffix_transport_preserves_offsets_and_the_full_history_fallback() {
        let (mut session, server) = threaded(3);
        assert_eq!(session.sdk_events_since(0).unwrap(), vec![(5, 7, vec![3])]);
        assert!(session.sdk_events_since(1).unwrap().is_empty());
        assert!(session.sdk_events_since(u32::MAX).unwrap().is_empty());
        assert_eq!(session.sdk_events().unwrap(), vec![(5, 7, vec![3])]);
        drop(session);
        server.join().unwrap();
        let mut fake = Fake::default();
        assert_eq!(
            fake.sdk_events_since(0).unwrap(),
            fake.sdk_events().unwrap()
        );
        assert!(fake.sdk_events_since(1).unwrap().is_empty());
        assert!(fake.sdk_events_since(u32::MAX).unwrap().is_empty());
        let history = vec![(5, 7, vec![3]), (5, 8, vec![4]), (9, 9, vec![5])];
        fake.sdk_history = Some(history.clone());
        assert_eq!(fake.sdk_events_since(1).unwrap(), history[1..]);
        assert_eq!(fake.sdk_events_since(2).unwrap(), history[2..]);
        let mut served = Served {
            session: Some(Box::new(fake)),
        };
        let mut request = Out::op(SDK_EVENTS);
        request.u64(2);
        let reply = served
            .handle(&request.0, &[], &mut Fake::open, &mut |_| {
                panic!("unexpected SDK allocation")
            })
            .unwrap();
        let mut input = In(&reply.0);
        assert_eq!(input.len().unwrap(), 1);
        assert_eq!(input.u64().unwrap(), 9);
        assert_eq!(input.u64().unwrap(), 9);
        assert_eq!(input.bytes().unwrap(), &[5]);
        input.finish().unwrap();
    }

    #[test]
    fn sdk_suffix_requests_reject_truncation_overflow_trailing_bytes_and_descriptors() {
        let mut served = Served {
            session: Some(Box::<Fake>::default()),
        };
        let mut too_large = Out::op(SDK_EVENTS);
        too_large.u64(u64::from(u32::MAX) + 1);
        let mut trailing = Out::op(SDK_EVENTS);
        trailing.u64(0).u8(1);
        for body in [vec![SDK_EVENTS], too_large.0, trailing.0] {
            assert!(
                served
                    .handle(&body, &[], &mut Fake::open, &mut |_| panic!(
                        "unexpected SDK allocation"
                    ))
                    .is_err()
            );
        }
        let (left, _right) = UnixStream::pair().unwrap();
        let fd: OwnedFd = left.into();
        let mut request = Out::op(SDK_EVENTS);
        request.u64(0);
        assert!(
            served
                .handle(&request.0, &[fd], &mut Fake::open, &mut |_| panic!(
                    "unexpected SDK allocation"
                ))
                .is_err()
        );
        assert!(
            served
                .handle(&request.0, &[], &mut Fake::open, &mut |_| panic!(
                    "unexpected SDK allocation"
                ))
                .is_ok()
        );
    }

    #[test]
    fn sdk_suffix_replies_reject_malformed_rows_and_cursor_overflow() {
        let mut trailing = Out::op(OK);
        trailing.len(0).u8(1);
        let mut bad_id = Out::op(OK);
        bad_id.len(1).u64(5).u64(u64::from(u32::MAX) + 1).bytes(&[]);
        let mut overflow = Out::op(OK);
        overflow.len(1).u64(5).u64(7).bytes(&[]);
        for (offset, reply) in [
            (0, vec![OK]),
            (0, trailing.0),
            (0, bad_id.0),
            (u32::MAX, overflow.0),
        ] {
            let (parent, child) = UnixStream::pair().unwrap();
            let server = std::thread::spawn(move || {
                let mut served = Served::default();
                let (body, fds) = recv(&child).unwrap();
                send(
                    &child,
                    &status(served.handle(&body, &fds, &mut Fake::open, &mut |_| {
                        panic!("unexpected SDK allocation")
                    })),
                    &[],
                )
                .unwrap();
                let (body, fds) = recv(&child).unwrap();
                assert!(fds.is_empty());
                let mut input = In(&body);
                assert_eq!(input.u8().unwrap(), SDK_EVENTS);
                assert_eq!(input.u64().unwrap(), u64::from(offset));
                input.finish().unwrap();
                send(&child, &reply, &[]).unwrap();
            });
            let mut session = WorkerSession::connect(
                WorkerConnection {
                    stream: parent,
                    process: None,
                    loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
                },
                &[3],
                b"i",
                &config(),
                &[],
                "fake",
            )
            .unwrap();
            assert!(session.sdk_events_since(offset).is_err());
            drop(session);
            server.join().unwrap();
        }
    }

    #[test]
    fn a_complete_malformed_frame_from_a_live_worker_remains_control() {
        let (parent, mut child) = UnixStream::pair().unwrap();
        let (release, hold) = std::sync::mpsc::channel();
        let (ready, complete) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut served = Served::default();
            let (body, fds) = recv(&child).unwrap();
            let reply = status(served.handle(&body, &fds, &mut Fake::open, &mut |_| {
                panic!("unexpected setup allocation")
            }));
            send(&child, &reply, &[]).unwrap();
            let (body, _) = recv(&child).unwrap();
            assert_eq!(body[0], STATE_HASH);
            let mut header = [0; HEADER];
            header[..8].copy_from_slice(&1_u64.to_le_bytes());
            header[8..].copy_from_slice(&1_u64.to_le_bytes());
            child.write_all(&header).unwrap();
            child.write_all(&[OK]).unwrap();
            ready.send(()).unwrap();
            hold.recv().unwrap();
        });
        let mut session = WorkerSession::connect(
            WorkerConnection {
                stream: parent,
                process: None,
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
            },
            &[3],
            b"i",
            &config(),
            &[],
            "fake",
        )
        .unwrap();
        let error = session.state_hash().unwrap_err();
        complete.recv().unwrap();
        assert!(
            matches!(error.downcast_ref::<SessionError>(),
            Some(SessionError::Control(detail)) if detail.contains("descriptor count")),
            "{error}"
        );
        assert!(session.abandoned());
        release.send(()).unwrap();
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn a_worker_session_preserves_typed_infrastructure_failure_and_abandons() {
        let (mut session, server) = threaded(3);
        let error = session.run_until(HOST_FAILURE).unwrap_err();
        assert!(matches!(error.downcast_ref::<SessionError>(),
            Some(SessionError::Infrastructure(detail)) if detail == "owned host exited87"));
        assert!(session.abandoned());
        assert!(matches!(
            session
                .state_hash()
                .unwrap_err()
                .downcast_ref::<SessionError>(),
            Some(SessionError::Abandoned)
        ));
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn a_worker_session_forwards_every_call_and_its_errors() {
        let (mut session, server) = threaded(3);
        assert_eq!(session.setup_handle(), (SnapId(1), 10));
        assert_eq!(
            session.run_until(40).unwrap(),
            StopReason::Deadline { vtime: Moment(40) }
        );
        let (snap, at) = session.snapshot().unwrap();
        assert_eq!((snap, at), (SnapId(2), 20));
        assert_eq!(session.state_hash().unwrap(), [4; 32]);
        assert_eq!(session.sdk_events().unwrap(), vec![(5, 7, vec![4])]);
        assert_eq!(
            session
                .telemetry_counters()
                .into_iter()
                .filter(|(name, _)| !name.starts_with("guard."))
                .collect::<Vec<_>>(),
            vec![("runs".to_owned(), 1)]
        );
        assert_eq!(session.snapshot_owned_pages(snap), Some(1));
        assert_eq!(session.snapshot_owned_pages(SnapId(9)), None);
        let held = session.store_bytes().expect("store bytes");
        assert!(held > 0);
        assert_eq!(session.console_tail().unwrap(), b"tail");
        session
            .branch_with_service(
                SnapId(1),
                ServiceConfig::default(),
                Vec::new(),
                Vec::new(),
                None,
            )
            .unwrap();
        assert_eq!(session.state_hash().unwrap(), [3; 32]);
        session.replay_snapshot(snap).unwrap();
        assert_eq!(session.state_hash().unwrap(), [4; 32]);
        session.drop_snapshot(snap).unwrap();
        let error = session.replay_snapshot(snap).unwrap_err();
        assert!(error.to_string().contains("unknown snapshot"), "{error}");
        assert!(!session.abandoned(), "an ordinary error keeps the worker");
        let error = session.run_until(HANG).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SessionError>(),
            Some(SessionError::Hung(limit)) if *limit == Duration::from_secs(3)
        ));
        assert!(session.abandoned());
        assert!(matches!(
            session
                .state_hash()
                .unwrap_err()
                .downcast_ref::<SessionError>(),
            Some(SessionError::Abandoned)
        ));
        assert_eq!(
            session
                .telemetry_counters()
                .into_iter()
                .filter(|(name, _)| !name.starts_with("guard."))
                .collect::<Vec<_>>(),
            vec![("runs".to_owned(), 1)],
            "the last counters outlive the worker"
        );
        assert_eq!(session.store_bytes(), None);
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn guard_telemetry_separates_parent_child_queries_and_cached_totals() {
        let (mut first, first_server) = threaded(3);
        let (second, second_server) = threaded(3);
        let a_values = first.telemetry_counters();
        let a_count = a_values.len();
        let a = a_values
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        let b_values = first.telemetry_counters();
        let b_count = b_values.len();
        let b = b_values
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(a.len(), a_count);
        assert_eq!(b.len(), b_count);
        assert_eq!(b["runs"], a["runs"]);
        assert_eq!(
            b["guard.parent.rpc_before.dispatches"] - a["guard.parent.rpc_before.dispatches"],
            1
        );
        assert_eq!(
            b["guard.parent.rpc_after.dispatches"] - a["guard.parent.rpc_after.dispatches"],
            1
        );
        assert_eq!(
            b["guard.child.loop.handled.dispatches"] - a["guard.child.loop.handled.dispatches"],
            1
        );
        assert_eq!(
            b["guard.child.loop.sent.dispatches"] - a["guard.child.loop.sent.dispatches"],
            1
        );
        assert_eq!(b["guard.child.loop.receive.dispatches"], 1);
        assert_eq!(b["guard.child.telemetry_queries"], 2);
        assert_eq!(b["guard.child.telemetry_query_failures"], 0);
        assert_eq!(b["guard.child.session.fixture.dispatches"], b["runs"]);
        assert!(!b.contains_key("guard.session.fixture.dispatches"));
        assert_eq!(b["guard.parent.rpc_before.strict_attempts"], 0);
        assert_eq!(
            b["guard.parent.rpc_before.fixture_dispatches"],
            b["guard.parent.rpc_before.dispatches"]
        );
        let separate = second
            .telemetry_counters()
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            separate["guard.parent.rpc_before.dispatches"],
            a["guard.parent.rpc_before.dispatches"]
        );
        assert!(first.run_until(u64::MAX).is_err());
        let failed = first
            .telemetry_counters()
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(failed["guard.child.telemetry_query_failures"], 1);
        let again = first
            .telemetry_counters()
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        let successful = second
            .telemetry_counters()
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            again["guard.child.telemetry_query_failures"]
                + successful["guard.child.telemetry_query_failures"],
            2
        );
        assert_eq!(
            failed["guard.child.loop.handled.dispatches"],
            b["guard.child.loop.handled.dispatches"]
        );
        drop(first);
        drop(second);
        first_server.join().unwrap();
        second_server.join().unwrap();
    }

    #[test]
    fn a_worker_forwards_explicit_service_resolution() {
        let (mut session, server) = threaded(3);
        assert_eq!(
            session
                .run_resolving(
                    40,
                    Resolution {
                        vtime: Moment(12),
                        service: 19,
                        id: DecisionId(7),
                        answer: Answer(vec![1, 0x42]),
                    },
                )
                .unwrap(),
            StopReason::Deadline { vtime: Moment(40) }
        );
        assert_eq!(session.state_hash().unwrap(), [0x42; 32]);
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn rpc_refusal_prevents_request_send_and_reply_acceptance() {
        use crate::image_identity::{LoadedImageGuard, trace_checks};
        for refusal in [1, 2] {
            let (parent, child) = UnixStream::pair().unwrap();
            child
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            let server = std::thread::spawn(move || {
                if refusal == 1 {
                    assert!(recv(&child).is_err());
                } else {
                    assert_eq!(recv(&child).unwrap().0, vec![STORE_BYTES]);
                    send(&child, &Out::default().u8(OK).u64(42).0, &[]).unwrap();
                }
            });
            let session = WorkerSession {
                stream: parent,
                process: None,
                setup: (SnapId(1), 10),
                seed: 0,
                counters: RefCell::new(Vec::new()),
                exchanging: Cell::new(false),
                telemetry_queries: Cell::new(0),
                telemetry_query_failures: Cell::new(0),
                failed: std::cell::Cell::new(false),
                loaded_image_guard: LoadedImageGuard::fixture(),
            };
            let (result, entries, strict) =
                trace_checks(Some(refusal), || session.call(&Out::op(STORE_BYTES), &[]));
            assert!(result.is_err());
            assert_eq!((entries, strict), (refusal, 0));
            assert!(session.failed.get());
            let counters = session
                .loaded_image_guard
                .counters("guard.parent")
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>();
            let refused_site = if refusal == 1 {
                "guard.parent.rpc_before.errors"
            } else {
                "guard.parent.rpc_after.errors"
            };
            assert_eq!(counters[refused_site], 1);
            assert!(session.call(&Out::op(STORE_BYTES), &[]).is_err());
            assert_eq!(
                session
                    .loaded_image_guard
                    .counters("guard.parent")
                    .into_iter()
                    .collect::<std::collections::BTreeMap<_, _>>(),
                counters
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn publication_checks_export_exchange_and_both_index_boundaries() {
        use crate::image_identity::trace_checks;
        for attached in [false, true] {
            for refusal in [None, Some(1), Some(2), Some(3), Some(4), Some(5), Some(6)] {
                let index = LocalIndex::new(1 << 20);
                let (session, server) = threaded_with_abort(3, None, true);
                let (rpc, entries, strict) =
                    trace_checks(None, || session.call(&Out::op(STORE_BYTES), &[]));
                assert!(rpc.is_ok());
                assert_eq!((entries, strict), (2, 0));
                let (result, entries, strict) = trace_checks(refusal, || {
                    if attached {
                        session.publish_snapshot_attached(
                            &index,
                            namespace(),
                            b"guard",
                            None,
                            SnapId(1),
                            &SnapshotAttachment {
                                domain: b"test".to_vec(),
                                bytes: b"payload".to_vec(),
                            },
                            1,
                        )
                    } else {
                        session.publish_snapshot(&index, namespace(), b"guard", None, SnapId(1), 1)
                    }
                });
                assert_eq!(strict, 0);
                assert_eq!(entries, refusal.unwrap_or(6));
                let counters = session
                    .loaded_image_guard
                    .counters("guard.parent")
                    .into_iter()
                    .collect::<std::collections::BTreeMap<_, _>>();
                for (step, site) in [
                    "export_before",
                    "extent_requested",
                    "extent_allocated",
                    "export_completed",
                    "publication_before",
                    "publication_after",
                ]
                .into_iter()
                .enumerate()
                {
                    let reached = u64::from(step < refusal.unwrap_or(6));
                    assert_eq!(
                        counters
                            .get(&format!("guard.parent.{site}.dispatches"))
                            .copied()
                            .unwrap_or(0),
                        reached
                    );
                    assert_eq!(
                        counters
                            .get(&format!("guard.parent.{site}.fixture_dispatches"))
                            .copied()
                            .unwrap_or(0),
                        reached
                    );
                    assert_eq!(
                        counters
                            .get(&format!("guard.parent.{site}.strict_attempts"))
                            .copied()
                            .unwrap_or(0),
                        0
                    );
                    assert_eq!(
                        counters
                            .get(&format!("guard.parent.{site}.errors"))
                            .copied()
                            .unwrap_or(0),
                        u64::from(refusal == Some(step + 1))
                    );
                }
                if refusal.is_some() {
                    assert!(result.is_err());
                    assert!(index.lookup(namespace(), b"guard").is_none());
                    assert_eq!(index.stats().leased, 0);
                    assert!(session.call(&Out::op(STORE_BYTES), &[]).is_err());
                } else {
                    index.release(result.unwrap());
                }
                drop(session);
                server.join().unwrap();
            }
        }
    }

    #[test]
    fn child_checks_initial_admission_and_each_response_even_on_handler_error() {
        use crate::image_identity::{LoadedImageGuard, trace_checks};
        for frames in 0..=2 {
            for valid in [false, true] {
                let (parent, child) = UnixStream::pair().unwrap();
                let server = std::thread::spawn(move || {
                    let (result, entries, strict) = trace_checks(None, || {
                        serve(
                            &child,
                            &mut |init| Fake::open_quarantining(init, None),
                            LoadedImageGuard::fixture(),
                        )
                    });
                    assert!(result.is_ok());
                    assert_eq!((entries, strict), (2 * frames + 1, 0));
                });
                for frame in 0..frames {
                    let mut request = Out::op(INIT);
                    put_config(&mut request, &config());
                    request.bytes(&[3]).bytes(b"i").len(0).bytes(b"fake");
                    send(
                        &parent,
                        if valid && frame == 0 {
                            &request.0
                        } else if valid {
                            &[STORE_BYTES]
                        } else {
                            &[255]
                        },
                        &[],
                    )
                    .unwrap();
                    let (reply, _) = recv(&parent).unwrap();
                    assert_eq!(reply[0], if valid { OK } else { FAILED });
                }
                drop(parent);
                server.join().unwrap();
            }
        }
    }

    #[test]
    fn child_send_failure_stops_before_another_receive() {
        use crate::image_identity::{LoadedImageGuard, trace_checks};
        let (parent, child) = UnixStream::pair().unwrap();
        send(&parent, &[255], &[]).unwrap();
        drop(parent);
        let (result, entries, strict) = trace_checks(None, || {
            serve(
                &child,
                &mut |_| panic!("unexpected open"),
                LoadedImageGuard::fixture(),
            )
        });
        assert!(result.is_err());
        assert_eq!((entries, strict), (2, 0));
    }

    #[test]
    fn child_refusal_prevents_send_or_next_handler() {
        use crate::image_identity::{LoadedImageGuard, trace_checks};
        for refusal in 1..=3 {
            let (parent, child) = UnixStream::pair().unwrap();
            let server = std::thread::spawn(move || {
                let (result, entries, _) = trace_checks(Some(refusal), || {
                    serve(
                        &child,
                        &mut |_| panic!("unexpected open"),
                        LoadedImageGuard::fixture(),
                    )
                });
                assert!(result.is_err());
                assert_eq!(entries, refusal);
            });
            let _ = send(&parent, &[255], &[]);
            if refusal == 3 {
                assert_eq!(recv(&parent).unwrap().0[0], FAILED);
                let _ = send(&parent, &[255], &[]);
            }
            assert!(recv(&parent).is_err());
            server.join().unwrap();
        }
    }

    #[derive(Debug)]
    struct ExtentHook {
        inner: LocalIndex,
        hook: fn(),
    }

    impl CacheIndex for ExtentHook {
        fn divergences(&self) -> Vec<Arc<crate::cache::CacheDivergence>> {
            self.inner.divergences()
        }
        fn verify_namespace(&self, n: Namespace) -> Result<(), crate::cache::CacheError> {
            self.inner.verify_namespace(n)
        }
        fn verify_integrity(&self) -> Result<(), crate::cache::CacheError> {
            self.inner.verify_integrity()
        }
        fn verify_lease(&self, l: &Lease) -> Result<(), crate::cache::CacheError> {
            self.inner.verify_lease(l)
        }
        fn lookup(&self, n: Namespace, k: &[u8]) -> Option<Lease> {
            self.inner.lookup(n, k)
        }
        fn extent(
            &self,
            len: usize,
        ) -> Result<crate::cache::WritableExtent, crate::cache::CacheError> {
            (self.hook)();
            self.inner.extent(len)
        }
        fn publish(
            &self,
            n: Namespace,
            k: &[u8],
            p: Option<&Lease>,
            e: crate::cache::WritableExtent,
            t: [u8; 32],
            c: u64,
        ) -> Result<Lease, crate::cache::CacheError> {
            self.inner.publish(n, k, p, e, t, c)
        }
        fn target_commitment(&self, l: &Lease) -> Result<[u8; 32], crate::cache::CacheError> {
            self.inner.target_commitment(l)
        }
        fn chain(&self, l: &Lease) -> Result<Vec<CommittedExtent>, crate::cache::CacheError> {
            self.inner.chain(l)
        }
        fn release(&self, l: Lease) {
            self.inner.release(l)
        }
        fn revoke(&self, l: Lease) {
            self.inner.revoke(l)
        }
        fn report_store(&self, h: u64, b: u64) -> bool {
            self.inner.report_store(h, b)
        }
        fn forget_store(&self, h: u64) {
            self.inner.forget_store(h)
        }
        fn stats(&self) -> crate::cache::CacheStats {
            self.inner.stats()
        }
    }

    #[test]
    fn allocation_callback_cannot_hide_refusal_poisoning_or_reenter_the_exchange() {
        thread_local! { static SESSION: RefCell<Option<WorkerSession>> = const { RefCell::new(None) }; }
        for case in 0..3 {
            let (parent, child) = UnixStream::pair().unwrap();
            let server = std::thread::spawn(move || {
                let (result, writes) = crate::cache::extent::trace_writes(|| {
                    serve(
                        &child,
                        &mut Fake::open,
                        crate::image_identity::LoadedImageGuard::fixture(),
                    )
                });
                if let Err(error) = result {
                    assert!(error.to_string().contains("allocation exchange failed"));
                }
                writes.len()
            });
            let session = connected_to_script(parent);
            SESSION.with(|slot| *slot.borrow_mut() = Some(session));
            let hook = match case {
                0 => crate::image_identity::refuse_next_check as fn(),
                1 => || SESSION.with(|slot| slot.borrow().as_ref().unwrap().failed.set(true)),
                _ => || {
                    SESSION.with(|slot| {
                        let borrow = slot.borrow();
                        let session = borrow.as_ref().unwrap();
                        assert!(
                            session
                                .call(&Out::op(STORE_BYTES), &[])
                                .unwrap_err()
                                .to_string()
                                .contains("already active")
                        );
                    })
                },
            };
            let index = ExtentHook {
                inner: LocalIndex::new(1 << 20),
                hook,
            };
            let (result, _, _) = crate::image_identity::trace_checks(None, || {
                SESSION.with(|slot| {
                    slot.borrow().as_ref().unwrap().publish_snapshot(
                        &index,
                        namespace(),
                        b"callback",
                        None,
                        SnapId(1),
                        1,
                    )
                })
            });
            if case < 2 {
                if case == 1 {
                    assert!(matches!(
                        result.as_ref().unwrap_err().downcast_ref::<SessionError>(),
                        Some(SessionError::Abandoned)
                    ));
                }
                assert!(result.is_err());
                assert_eq!(
                    (
                        index.stats().entries,
                        index.stats().leased,
                        index.stats().publishes
                    ),
                    (0, 0, 0)
                );
            } else {
                index.release(result.unwrap());
            }
            SESSION.with(|slot| slot.borrow_mut().take());
            assert_eq!(server.join().unwrap(), usize::from(case == 2));
        }
    }

    #[test]
    fn child_reverse_guards_refuse_before_the_next_send_receive_or_mapping() {
        for refusal in 4..=6 {
            let (parent, child) = UnixStream::pair().unwrap();
            parent
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let server = std::thread::spawn(move || {
                let guard = crate::image_identity::LoadedImageGuard::fixture();
                let ((result, writes), _, _) =
                    crate::image_identity::trace_checks(Some(refusal), || {
                        crate::cache::extent::trace_writes(|| {
                            serve(&child, &mut Fake::open, guard.clone())
                        })
                    });
                let error = result.unwrap_err().to_string();
                assert!(error.contains("injected loaded-image refusal"), "{error}");
                assert_eq!(MAPPING_ATTEMPTS.with(Cell::get), 0);
                assert!(writes.is_empty());
                let counters = guard
                    .counters("guard.child.loop")
                    .into_iter()
                    .collect::<std::collections::BTreeMap<_, _>>();
                for (step, site) in ["extent_before", "extent_sent", "extent_received"]
                    .into_iter()
                    .enumerate()
                {
                    let reached = u64::from(step + 4 <= refusal);
                    assert_eq!(
                        counters
                            .get(&format!("guard.child.loop.{site}.dispatches"))
                            .copied()
                            .unwrap_or(0),
                        reached
                    );
                    assert_eq!(
                        counters
                            .get(&format!("guard.child.loop.{site}.fixture_dispatches"))
                            .copied()
                            .unwrap_or(0),
                        reached
                    );
                    assert_eq!(
                        counters
                            .get(&format!("guard.child.loop.{site}.strict_attempts"))
                            .copied()
                            .unwrap_or(0),
                        0
                    );
                    assert_eq!(
                        counters
                            .get(&format!("guard.child.loop.{site}.errors"))
                            .copied()
                            .unwrap_or(0),
                        u64::from(step + 4 == refusal)
                    );
                }
            });
            send(&parent, &init_frame().0, &[]).unwrap();
            assert_eq!(recv(&parent).unwrap().0[0], OK);
            send(
                &parent,
                &Out::op(EXPORT).option(None).u64(1).bytes(b"").bytes(b"").0,
                &[],
            )
            .unwrap();
            if refusal != 4 {
                let (body, fds) = recv(&parent).unwrap();
                assert!(fds.is_empty());
                let mut input = In(&body);
                assert_eq!(input.u8().unwrap(), NEED_EXTENT);
                let len = input.len().unwrap();
                input.finish().unwrap();
                if refusal == 6 {
                    let destination = tempfile::tempfile().unwrap();
                    destination.set_len(len as u64).unwrap();
                    send(
                        &parent,
                        &Out::op(EXTENT_READY).len(0).len(len).0,
                        &[destination.as_fd()],
                    )
                    .unwrap();
                }
            }
            assert_eq!(
                recv(&parent).unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn export_postallocation_errors_keep_their_type_and_diagnostic_while_abandoning() {
        for kind in [HUNG, ABANDONED, OTHER] {
            let (parent, child) = UnixStream::pair().unwrap();
            let server = std::thread::spawn(move || {
                recv(&child).unwrap();
                send(&child, &Out::op(OK).u64(1).u64(10).0, &[]).unwrap();
                assert_eq!(recv(&child).unwrap().0[0], EXPORT);
                send(&child, &Out::op(NEED_EXTENT).len(8192).0, &[]).unwrap();
                let (_, fds) = recv(&child).unwrap();
                assert_eq!(fds.len(), 1);
                let mut reply = Out::op(FAILED);
                reply.u8(kind);
                if kind == HUNG {
                    reply.u64(123);
                }
                if kind == OTHER {
                    reply.bytes(b"child export diagnostic");
                }
                send(&child, &reply.0, &[]).unwrap();
            });
            let session = connected_to_script(parent);
            let index = LocalIndex::new(1 << 20);
            let error = session
                .publish_snapshot(&index, namespace(), b"typed", None, SnapId(1), 1)
                .unwrap_err();
            match (kind, error.downcast_ref::<SessionError>()) {
                (HUNG, Some(SessionError::Hung(limit))) => {
                    assert_eq!(*limit, Duration::from_nanos(123))
                }
                (ABANDONED, Some(SessionError::Abandoned)) => {}
                (OTHER, Some(SessionError::Control(message))) => {
                    assert_eq!(message, "child export diagnostic")
                }
                _ => panic!("wrong error type: {error}"),
            }
            assert!(session.abandoned());
            assert_eq!(index.stats().publishes, 0);
            server.join().unwrap();
        }
    }

    #[test]
    fn oversized_export_failure_is_bounded_and_abandons_without_publication() {
        let (parent, child) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            recv(&child).unwrap();
            send(&child, &Out::op(OK).u64(1).u64(10).0, &[]).unwrap();
            assert_eq!(recv(&child).unwrap().0[0], EXPORT);
            let _ = send(
                &child,
                &Out::op(FAILED)
                    .u8(OTHER)
                    .bytes(&vec![b'x'; EXPORT_FRAME_LIMIT + 1])
                    .0,
                &[],
            );
            assert!(recv(&child).is_err());
        });
        let session = connected_to_script(parent);
        let index = LocalIndex::new(1 << 20);
        let error = session
            .publish_snapshot(&index, namespace(), b"oversized", None, SnapId(1), 1)
            .unwrap_err();
        assert!(
            error.to_string().contains("frame length exceeds limit"),
            "{error}"
        );
        assert!(session.abandoned());
        assert_eq!((index.stats().entries, index.stats().publishes), (0, 0));
        server.join().unwrap();
    }

    #[test]
    fn export_guard_counts_include_the_reverse_allocation_boundaries() {
        let (parent, child) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let guard = crate::image_identity::LoadedImageGuard::fixture();
            let (result, entries, strict) = crate::image_identity::trace_checks(None, || {
                serve(&child, &mut Fake::open, guard.clone())
            });
            result.unwrap();
            assert_eq!((entries, strict), (8, 0));
            let counters = guard
                .counters("child")
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>();
            for (site, count) in [
                ("receive", 1),
                ("handled", 2),
                ("sent", 2),
                ("extent_before", 1),
                ("extent_sent", 1),
                ("extent_received", 1),
            ] {
                assert_eq!(counters[&format!("child.{site}.dispatches")], count);
                assert_eq!(counters[&format!("child.{site}.fixture_dispatches")], count);
                assert_eq!(counters[&format!("child.{site}.strict_attempts")], 0);
            }
            assert_eq!(
                counters
                    .iter()
                    .filter(|(key, _)| key.ends_with(".dispatches"))
                    .map(|(_, value)| *value)
                    .sum::<u64>(),
                8
            );
        });
        let session = connected_to_script(parent);
        let index = LocalIndex::new(1 << 20);
        let (result, entries, strict) = crate::image_identity::trace_checks(None, || {
            session.publish_snapshot(&index, namespace(), b"counts", None, SnapId(1), 1)
        });
        assert_eq!((entries, strict), (6, 0));
        index.release(result.unwrap());
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn export_allocation_refusal_drains_and_preserves_the_source_session() {
        let (mut session, server) = threaded(3);
        let refused = LocalIndex::new(0);
        assert!(
            session
                .publish_snapshot(&refused, namespace(), b"missing", None, SnapId(u64::MAX), 1)
                .is_err()
        );
        assert!(!session.abandoned());
        assert_eq!(session.state_hash().unwrap(), [3; 32]);
        let error = session
            .publish_snapshot(&refused, namespace(), b"refused", None, SnapId(1), 1)
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<crate::cache::CacheError>(),
            Some(crate::cache::CacheError::Refused { .. })
        ));
        assert!(!session.abandoned());
        assert_eq!(session.state_hash().unwrap(), [3; 32]);
        let exchange = session.begin_exchange().unwrap();
        assert!(
            session
                .call(&Out::op(STORE_BYTES), &[])
                .unwrap_err()
                .to_string()
                .contains("already active")
        );
        drop(exchange);
        let index = LocalIndex::new(1 << 20);
        let lease = session
            .publish_snapshot(&index, namespace(), b"valid", None, SnapId(1), 1)
            .unwrap();
        index.release(lease);
        drop(session);
        server.join().unwrap();
    }

    fn init_frame() -> Out {
        let mut request = Out::op(INIT);
        put_config(&mut request, &config());
        request.bytes(&[3]).bytes(b"i").len(0).bytes(b"fake");
        request
    }

    #[test]
    fn export_destination_rejects_bad_frames_descriptors_and_backing_ranges() {
        for case in 0..11 {
            let (parent, child) = UnixStream::pair().unwrap();
            let (finished, completion) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let result = serve(
                    &child,
                    &mut Fake::open,
                    crate::image_identity::LoadedImageGuard::fixture(),
                )
                .map_err(|error| error.to_string());
                let _ = finished.send(result);
            });
            send(&parent, &init_frame().0, &[]).unwrap();
            assert_eq!(recv(&parent).unwrap().0[0], OK);
            send(
                &parent,
                &Out::op(EXPORT).option(None).u64(1).bytes(b"").bytes(b"").0,
                &[],
            )
            .unwrap();
            let (request, fds) = recv(&parent).unwrap();
            assert!(fds.is_empty());
            let mut input = In(&request);
            assert_eq!(input.u8().unwrap(), NEED_EXTENT);
            let len = input.len().unwrap();
            input.finish().unwrap();
            let named = tempfile::NamedTempFile::new().unwrap();
            let file = named.as_file();
            file.set_len(if case == 5 { 1 } else { len as u64 })
                .unwrap();
            let mut reply = Out::op(EXTENT_READY);
            reply
                .len(if case == 4 { usize::MAX } else { 0 })
                .len(if case == 3 { len - 1 } else { len });
            let read_only = std::fs::File::open(named.path()).unwrap();
            let fd = file.as_fd();
            match case {
                0 => send(&parent, &reply.0, &[]).unwrap(),
                1 => send(&parent, &reply.0, &[fd, fd]).unwrap(),
                2 => {
                    reply.u8(0);
                    send(&parent, &reply.0, &[fd]).unwrap();
                }
                6 => send(&parent, &Out::op(EXTENT_DENIED).0, &[fd]).unwrap(),
                7 => send(&parent, &Out::op(RUN_UNTIL).u64(42).0, &[]).unwrap(),
                9 => send(&parent, &reply.0, &[read_only.as_fd()]).unwrap(),
                10 => {
                    let mut header = [0_u8; HEADER];
                    header[..8].copy_from_slice(&((EXPORT_FRAME_LIMIT + 1) as u64).to_le_bytes());
                    (&parent).write_all(&header).unwrap();
                }
                8 => {
                    parent.shutdown(Shutdown::Write).unwrap();
                }
                _ => send(&parent, &reply.0, &[fd]).unwrap(),
            }
            let result = completion.recv_timeout(Duration::from_secs(5));
            if result.is_err() {
                let _ = parent.shutdown(Shutdown::Both);
            }
            server.join().unwrap();
            let error = result
                .unwrap_or_else(|error| panic!("case {case} did not refuse the frame: {error}"))
                .unwrap_err();
            if case == 10 {
                assert!(error.contains("frame length exceeds limit"), "{error}");
            }
            assert!(recv(&parent).is_err(), "case {case}");
        }
    }

    fn connected_to_script(parent: UnixStream) -> WorkerSession {
        WorkerSession::connect(
            WorkerConnection {
                stream: parent,
                process: None,
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
            },
            &[3],
            b"i",
            &config(),
            &[],
            "fake",
        )
        .unwrap()
    }

    #[test]
    fn export_parent_rejects_unsolicited_or_repeated_allocations_and_bad_completion() {
        for case in 0..8 {
            let (parent, child) = UnixStream::pair().unwrap();
            let server = std::thread::spawn(move || {
                assert_eq!(recv(&child).unwrap().0[0], INIT);
                send(&child, &Out::op(OK).u64(1).u64(10).0, &[]).unwrap();
                assert_eq!(recv(&child).unwrap().0[0], EXPORT);
                let file = tempfile::tempfile().unwrap();
                let mut reply = Out::op(NEED_EXTENT);
                reply.len(if case == 0 { 0 } else { 8192 });
                if case == 1 {
                    reply.u8(0);
                }
                if case == 7 {
                    reply.0 = vec![0; EXPORT_FRAME_LIMIT + 1];
                }
                if case == 2 {
                    reply.0 = vec![OK];
                }
                let descriptors = [file.as_fd()];
                send(&child, &reply.0, if case == 3 { &descriptors } else { &[] }).unwrap();
                if case < 4 || case == 7 {
                    assert!(recv(&child).is_err());
                    return;
                }
                let (_, fds) = recv(&child).unwrap();
                assert_eq!(fds.len(), 1);
                let completion = match case {
                    4 => Out::op(NEED_EXTENT).len(8192).0.clone(),
                    5 => Out::op(OK).u8(0).0.clone(),
                    _ => Out::op(OK).0,
                };
                send(
                    &child,
                    &completion,
                    if case == 6 { &descriptors } else { &[] },
                )
                .unwrap();
            });
            let session = connected_to_script(parent);
            let index = LocalIndex::new(1 << 20);
            assert!(
                session
                    .publish_snapshot(&index, namespace(), b"bad", None, SnapId(1), 1)
                    .is_err()
            );
            assert!(session.abandoned());
            assert_eq!(
                (
                    index.stats().entries,
                    index.stats().leased,
                    index.stats().publishes
                ),
                (0, 0, 0)
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn lost_final_ack_never_publishes_or_reuses_a_still_mapped_destination() {
        use std::os::unix::fs::MetadataExt;
        let (parent, child) = UnixStream::pair().unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            recv(&child).unwrap();
            send(&child, &Out::op(OK).u64(1).u64(10).0, &[]).unwrap();
            assert_eq!(recv(&child).unwrap().0[0], EXPORT);
            send(&child, &Out::op(NEED_EXTENT).len(8192).0, &[]).unwrap();
            let (body, fds) = recv(&child).unwrap();
            let mut input = In(&body);
            assert_eq!(input.u8().unwrap(), EXTENT_READY);
            let offset = input.len().unwrap();
            let len = input.len().unwrap();
            let file = std::fs::File::from(fds[0].try_clone().unwrap());
            let metadata = file.metadata().unwrap();
            let mut writer = Mapping::new(file.as_fd(), offset, len, true).unwrap();
            ready_tx
                .send((metadata.dev(), metadata.ino(), offset, len))
                .unwrap();
            child.shutdown(Shutdown::Both).unwrap();
            release_rx.recv().unwrap();
            writer.bytes_mut().fill(0xab);
        });
        let session = connected_to_script(parent);
        let index = LocalIndex::new(1 << 20);
        assert!(
            session
                .publish_snapshot(&index, namespace(), b"uncertain", None, SnapId(1), 1)
                .is_err()
        );
        let (device, inode, old_offset, old_len) = ready_rx.recv().unwrap();
        assert!(session.abandoned());
        assert_eq!(
            (
                index.stats().entries,
                index.stats().leased,
                index.stats().publishes
            ),
            (0, 0, 0)
        );
        let mut next = index.extent(old_len).unwrap();
        let SharedRange { fd, offset, len } = next.shared_range();
        let metadata = std::fs::File::from(fd.try_clone_to_owned().unwrap())
            .metadata()
            .unwrap();
        if cfg!(target_os = "linux") {
            assert!(
                (device, inode) != (metadata.dev(), metadata.ino())
                    || offset + len <= old_offset
                    || old_offset + old_len <= offset
            );
        }
        next.bytes_mut().fill(0xcd);
        release_tx.send(()).unwrap();
        server.join().unwrap();
        assert!(next.bytes_mut().iter().all(|byte| *byte == 0xcd));
    }

    #[test]
    fn a_published_extent_imports_in_another_worker() {
        let index = LocalIndex::new(1 << 20);
        let (mut first, first_server) = threaded(3);
        let (mut second, second_server) = threaded(50);
        first.run_until(40).unwrap();
        let (snap, _) = first.snapshot().unwrap();
        let lease = first
            .publish_snapshot(&index, namespace(), b"key", None, snap, 1)
            .unwrap();
        let (imported, _) = second.import_cached(&index, &lease, SnapId(1)).unwrap();
        assert_eq!(second.state_hash().unwrap(), first.state_hash().unwrap());
        second.run_until(80).unwrap();
        second.replay_snapshot(imported).unwrap();
        assert_eq!(second.state_hash().unwrap(), [4; 32]);
        index.release(lease);
        assert_eq!(index.stats().entries, 1);
        drop((first, second));
        first_server.join().unwrap();
        second_server.join().unwrap();
    }

    #[test]
    fn worker_import_selects_target_attachment_and_rejects_independent_digest_mismatch() {
        use crate::cache::extent::{extent_len, read_extent, write_extent};
        let index = LocalIndex::new(1 << 20);
        let space = Namespace::exact(&[b"worker-exact"]);
        let (mut source, server) = threaded(3);
        let (mut destination, other_server) = threaded(50);
        let first = SnapshotAttachment {
            domain: b"opaque-v1".to_vec(),
            bytes: b"parent".to_vec(),
        };
        let second = SnapshotAttachment {
            domain: b"opaque-v1".to_vec(),
            bytes: b"target".to_vec(),
        };
        let (parent, _) = source.snapshot().unwrap();
        let parent_lease = source
            .publish_snapshot_attached(&index, space, &[9; 32], None, parent, &first, 1)
            .unwrap();
        source.run_until(40).unwrap();
        let (child, _) = source.snapshot().unwrap();
        let lease = source
            .publish_snapshot_attached(
                &index,
                space,
                &[3; 32],
                Some((parent, &parent_lease)),
                child,
                &second,
                1,
            )
            .unwrap();
        let receipt = destination
            .import_cached_receipt(&index, &lease, SnapId(1))
            .unwrap();
        assert_eq!(receipt.attachment, second);
        assert_eq!(
            receipt.object_commitment,
            index.target_commitment(&lease).unwrap()
        );
        assert_eq!(
            destination.state_hash().unwrap(),
            source.state_hash().unwrap()
        );
        let chain = index.chain(&lease).unwrap();
        let target = read_extent(chain.last().unwrap().bytes()).unwrap();
        let envelope = super::super::attachment::decode(target.sidecar).unwrap();
        let forged =
            super::super::attachment::encode(envelope.raw, envelope.machine_commitment, &first)
                .unwrap();
        let len = extent_len(target.pages.len(), target.reverted.len(), forged.len()).unwrap();
        let mut extent = index.extent(len).unwrap();
        write_extent(extent.bytes_mut(), &target.pages, &target.reverted, &forged).unwrap();
        let bad = index
            .publish(
                space,
                &[4; 32],
                Some(&parent_lease),
                extent,
                receipt.object_commitment,
                1,
            )
            .unwrap();
        let before = destination.store_bytes();
        let error = destination
            .import_cached_receipt(&index, &bad, SnapId(1))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cached object commitment differs")
        );
        assert_eq!(destination.store_bytes(), before);
        let forged = super::super::attachment::encode(envelope.raw, [0xee; 32], &second).unwrap();
        let len = extent_len(target.pages.len(), target.reverted.len(), forged.len()).unwrap();
        let mut extent = index.extent(len).unwrap();
        write_extent(extent.bytes_mut(), &target.pages, &target.reverted, &forged).unwrap();
        let digest = super::super::attachment::commitment([0xee; 32], &second);
        let wrong_machine = index
            .publish(space, &[5; 32], Some(&parent_lease), extent, digest, 1)
            .unwrap();
        let error = destination
            .import_cached_receipt(&index, &wrong_machine, SnapId(1))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cached machine commitment differs")
        );
        assert_eq!(destination.store_bytes(), before);
        index.release(wrong_machine);
        index.release(bad);
        index.release(lease);
        index.release(parent_lease);
        drop((source, destination));
        server.join().unwrap();
        other_server.join().unwrap();
    }

    #[test]
    fn identical_machine_with_different_attachment_quarantines_publication() {
        let index = LocalIndex::new(1 << 20);
        let (mut source, server) = threaded(3);
        let (snap, _) = source.snapshot().unwrap();
        let a = SnapshotAttachment {
            domain: b"opaque-v1".to_vec(),
            bytes: vec![1],
        };
        let b = SnapshotAttachment {
            domain: b"opaque-v1".to_vec(),
            bytes: vec![2],
        };
        let lease = source
            .publish_snapshot_attached(&index, namespace(), b"key", None, snap, &a, 1)
            .unwrap();
        let error = source
            .publish_snapshot_attached(&index, namespace(), b"key", None, snap, &b, 1)
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<crate::cache::CacheError>(),
            Some(crate::cache::CacheError::DivergentDuplicate { .. })
        ));
        let witnesses = index.divergences();
        assert_eq!(witnesses.len(), 1);
        assert_ne!(
            witnesses[0].existing.target_commitment,
            witnesses[0].published.target_commitment
        );
        assert!(index.target_commitment(&lease).is_err());
        index.release(lease);
        drop(source);
        server.join().unwrap();
    }

    #[test]
    fn quarantine_during_worker_import_refuses_and_drops_the_imported_handle() {
        let index = Arc::new(LocalIndex::new(1 << 20));
        let (mut first, first_server) = threaded(3);
        let (mut second, second_server) = threaded_quarantining(50, Some(Arc::clone(&index)));
        let (snapshot, _) = first.snapshot().unwrap();
        let lease = first
            .publish_snapshot(index.as_ref(), namespace(), b"key", None, snapshot, 1)
            .unwrap();
        let error = second
            .import_cached(index.as_ref(), &lease, SnapId(1))
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<crate::cache::CacheError>(),
            Some(crate::cache::CacheError::Quarantined { .. })
        ));
        assert_eq!(second.store_bytes(), Some(4096));
        assert_eq!(index.divergences().len(), 1);
        index.release(lease);
        drop((first, second));
        first_server.join().unwrap();
        second_server.join().unwrap();
    }

    #[test]
    fn worker_publication_rejects_a_divergent_duplicate_target() {
        let index = LocalIndex::new(1 << 20);
        let (mut first, first_server) = threaded(3);
        let (mut second, second_server) = threaded(4);
        let (first_snap, _) = first.snapshot().unwrap();
        let (second_snap, _) = second.snapshot().unwrap();
        let first_lease = first
            .publish_snapshot(&index, namespace(), b"key", None, first_snap, 1)
            .unwrap();
        let error = second
            .publish_snapshot(&index, namespace(), b"key", None, second_snap, 1)
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<crate::cache::CacheError>(),
            Some(crate::cache::CacheError::DivergentDuplicate { .. })
        ));
        assert_eq!(index.stats().entries, 1);
        assert_eq!(index.stats().duplicates, 0);
        index.release(first_lease);
        drop((first, second));
        first_server.join().unwrap();
        second_server.join().unwrap();
    }

    #[test]
    fn early_export_infrastructure_failure_is_typed_and_retires_session() {
        let (parent, child) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut served = Served::default();
            loop {
                let (body, fds) = recv(&child).unwrap();
                if body[0] == EXPORT {
                    send(
                        &child,
                        &status(Err(SessionError::Infrastructure(
                            "export host failure".into(),
                        )
                        .into())),
                        &[],
                    )
                    .unwrap();
                    return;
                }
                let reply = status(served.handle(&body, &fds, &mut Fake::open, &mut |_| {
                    panic!("unexpected allocation")
                }));
                send(&child, &reply, &[]).unwrap();
            }
        });
        let mut session = WorkerSession::connect(
            WorkerConnection {
                stream: parent,
                process: None,
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
            },
            &[3],
            b"i",
            &config(),
            &[],
            "fake",
        )
        .unwrap();
        let index = LocalIndex::new(1 << 20);
        let (snapshot, _) = session.snapshot().unwrap();
        let error = session
            .publish_snapshot(&index, namespace(), b"early-failure", None, snapshot, 1)
            .unwrap_err();
        assert!(
            matches!(error.downcast_ref::<SessionError>(), Some(SessionError::Infrastructure(detail)) if detail == "export host failure")
        );
        assert!(session.abandoned());
        assert!(matches!(
            session
                .state_hash()
                .unwrap_err()
                .downcast_ref::<SessionError>(),
            Some(SessionError::Abandoned)
        ));
        assert_eq!(
            (
                index.stats().entries,
                index.stats().leased,
                index.stats().publishes
            ),
            (0, 0, 0)
        );
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn a_worker_that_dies_before_acknowledging_a_write_publishes_nothing() {
        let (parent, child) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut served = Served::default();
            loop {
                let (body, fds) = recv(&child).unwrap();
                let mut allocate = |len| {
                    send(&child, &Out::op(NEED_EXTENT).len(len).0, &[])?;
                    let (reply, fds) = recv(&child)?;
                    let mut input = In(&reply);
                    assert_eq!(input.u8()?, EXTENT_READY);
                    let offset = input.len()?;
                    assert_eq!(input.len()?, len);
                    Mapping::new(fds[0].as_fd(), offset, len, true).map_err(Into::into)
                };
                let reply = status(served.handle(&body, &fds, &mut Fake::open, &mut allocate));
                if body[0] == EXPORT {
                    return;
                }
                send(&child, &reply, &[]).unwrap();
            }
        });
        let mut session = WorkerSession::connect(
            WorkerConnection {
                stream: parent,
                process: None,
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
            },
            &[3],
            b"i",
            &config(),
            &[],
            "fake",
        )
        .unwrap();
        let index = LocalIndex::new(1 << 20);
        let (snap, _) = session.snapshot().unwrap();
        let error = session
            .publish_snapshot(&index, namespace(), b"key", None, snap, 1)
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<SessionError>(),
                Some(SessionError::Infrastructure(_))
            ),
            "{error}"
        );
        server.join().unwrap();
        let stats = index.stats();
        assert_eq!((stats.entries, stats.leased, stats.publishes), (0, 0, 0));
        assert!(session.abandoned());
        assert!(index.lookup(namespace(), b"key").is_none());
        let (mut next, next_server) = threaded(3);
        let (snap, _) = next.snapshot().unwrap();
        let lease = next
            .publish_snapshot(&index, namespace(), b"key", None, snap, 1)
            .unwrap();
        assert_eq!(index.stats().entries, 1);
        index.release(lease);
        drop(next);
        next_server.join().unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn killed_worker_processes_release_their_leases_and_keep_the_cache_consistent() {
        if !crate::image_identity::strict_test_host_ready() {
            return;
        }
        if crate::image_identity::rerun_clean_loader_test(
            "session::worker::tests::killed_worker_processes_release_their_leases_and_keep_the_cache_consistent",
        ) {
            return;
        }
        let index = Arc::new(LocalIndex::new(1 << 20));
        let mut owner = spawned(3);
        let mut reader = spawned(50);
        owner.run_until(40).unwrap();
        let (snap, _) = owner.snapshot().unwrap();
        let owned = owner
            .publish_snapshot(index.as_ref(), namespace(), b"key", None, snap, 1)
            .unwrap();
        let read = index.lookup(namespace(), b"key").unwrap();
        reader
            .import_cached(index.as_ref(), &read, SnapId(1))
            .unwrap();
        kill(&owner);
        assert!(owner.run_until(80).is_err());
        assert!(owner.abandoned());
        index.release(owned);
        drop(owner);
        assert_eq!(index.stats().leased, 1, "the reader keeps its lease");
        reader.run_until(80).unwrap();
        reader
            .import_cached(index.as_ref(), &read, SnapId(1))
            .unwrap();
        assert_eq!(reader.state_hash().unwrap(), [4; 32]);
        index.release(read);

        let before = index.stats();
        let mut unpublished = spawned(9);
        unpublished.run_until(40).unwrap();
        let (snap, _) = unpublished.snapshot().unwrap();
        kill(&unpublished);
        assert!(
            unpublished
                .publish_snapshot(index.as_ref(), namespace(), b"other", None, snap, 1)
                .is_err()
        );
        drop(unpublished);
        let after = index.stats();
        assert_eq!(
            (after.entries, after.leased, after.charged),
            (before.entries, 0, before.charged)
        );
        let lease = index.lookup(namespace(), b"key").unwrap();
        assert_eq!(index.chain(&lease).unwrap().len(), 1);
        index.release(lease);
    }
}
