// SPDX-License-Identifier: AGPL-3.0-or-later

mod work;

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
    sync::Arc,
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
use crate::copy_work::{CopySite, CopyWork};

pub const WORKER_FD_ENV: &str = "HARMONY_SESSION_WORKER_FD";
const HEADER: usize = 16;
const MAX_FDS: usize = crate::import_layout::PROTOCOL_REFERENCE_LIMIT;
#[cfg(target_os = "macos")]
const DARWIN_CONTROL_CAPACITY: usize = 2048;
const EXIT_POLL: Duration = Duration::from_millis(5);
const EXIT_POLLS: u32 = 400;
const IMAGE_HELLO: &[u8] = b"harmony-session-worker-image-v3";
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
    fn shutdown(&mut self) -> Result<(), Box<dyn Error>> {
        Err("owned session shutdown is unavailable".into())
    }
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
    fn shutdown(&mut self) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
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

fn check_worker_image_hello(body: &[u8], expected_image: &str) -> io::Result<()> {
    let mut input = In(body);
    if input.bytes()? != IMAGE_HELLO {
        return Err(malformed("worker image hello has an unknown version"));
    }
    let actual = input.string()?;
    input.finish()?;
    if actual != expected_image {
        return Err(io::Error::other(
            "session worker loaded image differs from launcher",
        ));
    }
    Ok(())
}

#[derive(Debug, Default)]
struct Out(Vec<u8>, Option<Arc<CopyWork>>);

impl Out {
    fn measured(op: u8, owner: &Arc<CopyWork>) -> Self {
        Self(vec![op], Some(Arc::clone(owner)))
    }
    fn measured_empty(owner: &Arc<CopyWork>) -> Self {
        Self(Vec::new(), Some(Arc::clone(owner)))
    }
    fn op(op: u8) -> Self {
        Self(vec![op], None)
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
        if let Some(owner) = &self.1 {
            owner.copied(CopySite::Encode, value.len());
        }
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
    fn owned_bytes(&mut self, owner: &CopyWork) -> io::Result<Vec<u8>> {
        let bytes = self.bytes()?.to_vec();
        owner.copied(CopySite::Decode, bytes.len());
        Ok(bytes)
    }
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

fn send_with_fds(
    stream: &UnixStream,
    bytes: &[u8],
    fds: &[BorrowedFd<'_>],
    delta: &mut work::Delta,
) -> io::Result<usize> {
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
            delta.header = delta.header.saturating_add(sent as u64);
            if sent != 0 {
                delta.fds = delta.fds.saturating_add(fds.len() as u64);
            }
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
    delta: &mut work::Delta,
) -> io::Result<(usize, Vec<OwnedFd>)> {
    #[cfg(target_os = "macos")]
    let space = DARWIN_CONTROL_CAPACITY;
    #[cfg(not(target_os = "macos"))]
    let space = control_space(MAX_FDS);
    recv_with_fds_control_space(stream, buffer, deadline, space, delta)
}

#[allow(clippy::disallowed_methods)]
fn recv_with_fds_control_space(
    stream: &UnixStream,
    buffer: &mut [u8],
    deadline: Option<Instant>,
    space: usize,
    delta: &mut work::Delta,
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
    delta.header = delta.header.saturating_add(received as u64);
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
    delta.fds = delta.fds.saturating_add(fds.len() as u64);
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

#[cfg(test)]
thread_local! { static FAIL_AFTER_FD_HEADER: Cell<bool> = const { Cell::new(false) }; }

fn send_raw(
    stream: &UnixStream,
    body: &[u8],
    fds: &[BorrowedFd<'_>],
    delta: &mut work::Delta,
) -> io::Result<()> {
    if fds.len() > MAX_FDS {
        return Err(malformed("too many descriptors"));
    }
    let mut header = [0_u8; HEADER];
    header[..8].copy_from_slice(&(body.len() as u64).to_le_bytes());
    header[8..].copy_from_slice(&(fds.len() as u64).to_le_bytes());
    let sent = send_with_fds(stream, &header, fds, delta)?;
    #[cfg(test)]
    if !fds.is_empty() && FAIL_AFTER_FD_HEADER.with(|fail| fail.replace(false)) {
        return Err(io::Error::other(
            "injected failure after descriptor transfer",
        ));
    }
    let mut writer = stream;
    work::write_all_counted(&header[sent..], &mut delta.header, |bytes| {
        writer.write(bytes)
    })?;
    work::write_all_counted(body, &mut delta.body, |bytes| writer.write(bytes))
}

#[cfg(not(target_os = "macos"))]
#[allow(clippy::disallowed_methods)]
fn read_exact_until(
    stream: &UnixStream,
    buffer: &mut [u8],
    deadline: Instant,
    count: &mut u64,
) -> io::Result<()> {
    work::read_exact_counted(buffer, count, false, |buffer| {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        stream.set_read_timeout(Some(remaining))?;
        (&*stream).read(buffer)
    })
}

#[cfg(target_os = "macos")]
fn read_frame_exact(
    stream: &UnixStream,
    mut buffer: &mut [u8],
    deadline: Option<Instant>,
    count: &mut u64,
    fd_count: &mut u64,
) -> io::Result<()> {
    while !buffer.is_empty() {
        let mut delta = work::Delta::default();
        let result = recv_with_fds(stream, buffer, deadline, &mut delta);
        *count = count.saturating_add(delta.header);
        *fd_count = fd_count.saturating_add(delta.fds);
        let (size, rights) = result?;
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
    count: &mut u64,
    _fd_count: &mut u64,
) -> io::Result<()> {
    if let Some(deadline) = deadline {
        read_exact_until(stream, buffer, deadline, count)
    } else {
        work::read_exact_counted(buffer, count, true, |bytes| (&*stream).read(bytes))
    }
}

fn recv_raw(
    stream: &UnixStream,
    limit: usize,
    deadline: Option<Instant>,
    delta: &mut work::Delta,
) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut header = [0_u8; HEADER];
    let (received, fds) = recv_with_fds(stream, &mut header, deadline, delta)?;
    if received == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    read_frame_exact(
        stream,
        &mut header[received..],
        deadline,
        &mut delta.header,
        &mut delta.fds,
    )?;
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
    read_frame_exact(stream, &mut body, deadline, &mut delta.body, &mut delta.fds)?;
    Ok((body, fds))
}

fn send_measured(
    stream: &UnixStream,
    body: &[u8],
    fds: &[BorrowedFd<'_>],
    owner: &work::Work,
    group: work::Group,
) -> io::Result<()> {
    let mut delta = work::Delta::default();
    let result = send_raw(stream, body, fds, &mut delta);
    owner.record(group, false, delta, result.is_ok());
    result
}
fn receive_measured(
    stream: &UnixStream,
    limit: usize,
    deadline: Option<Instant>,
    owner: &work::Work,
    group: Option<work::Group>,
) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut delta = work::Delta::default();
    let result = recv_raw(stream, limit, deadline, &mut delta);
    let group = group.unwrap_or_else(|| {
        result
            .as_ref()
            .map_or(work::Group::Unknown, |(body, _)| work::Group::request(body))
    });
    owner.record(group, true, delta, result.is_ok());
    result
}
#[cfg(test)]
fn send(stream: &UnixStream, body: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    send_raw(stream, body, fds, &mut work::Delta::default())
}
#[cfg(test)]
fn recv_with_limit(
    stream: &UnixStream,
    limit: usize,
    deadline: Option<Instant>,
) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    recv_raw(stream, limit, deadline, &mut work::Delta::default())
}
#[cfg(test)]
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
    #[cfg(test)]
    fn new(fd: BorrowedFd<'_>, offset: usize, len: usize, writable: bool) -> io::Result<Self> {
        Self::measured(fd, offset, len, writable, &CopyWork::default())
    }
    fn measured(
        fd: BorrowedFd<'_>,
        offset: usize,
        len: usize,
        writable: bool,
        work: &CopyWork,
    ) -> io::Result<Self> {
        work.mapping_attempt(writable);
        let result = Self::new_inner(fd, offset, len, writable, work);
        if result.is_err() {
            work.mapping_result(writable, None);
        }
        result
    }
    fn new_inner(
        fd: BorrowedFd<'_>,
        offset: usize,
        len: usize,
        writable: bool,
        work: &CopyWork,
    ) -> io::Result<Self> {
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
        work.mapping_result(writable, Some((len, mapped_len)));
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
        self.spawn_guarded(&self.loaded_image_guard, &work::Work::default())
    }

    #[allow(clippy::disallowed_methods)]
    fn spawn_guarded(
        &self,
        loaded_image_guard: &crate::image_identity::LoadedImageGuard,
        transport: &work::Work,
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
            let (body, fds) = receive_measured(
                &parent,
                IMAGE_HELLO_LIMIT,
                Some(deadline),
                transport,
                Some(work::Group::Hello),
            )?;
            if !fds.is_empty() {
                return Err(malformed("worker image hello carries descriptors"));
            }
            check_worker_image_hello(&body, &self.expected_image)
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
    .u8(match config.qualification {
        execution_contract::Requirement::Ordinary => 0,
        execution_contract::Requirement::Qualified(
            execution_contract::Composition::RetainedLinuxContinuation9673,
        ) => 1,
        execution_contract::Requirement::Qualified(
            execution_contract::Composition::UmlFaultsWholeState,
        ) => 2,
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
        qualification: match input.u8()? {
            0 => execution_contract::Requirement::Ordinary,
            1 => execution_contract::Requirement::Qualified(
                execution_contract::Composition::RetainedLinuxContinuation9673,
            ),
            2 => execution_contract::Requirement::Qualified(
                execution_contract::Composition::UmlFaultsWholeState,
            ),
            _ => return Err(malformed("unknown execution qualification requirement")),
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
    transport: Arc<work::Work>,
    stream: UnixStream,
    process: Option<Child>,
    #[cfg(test)]
    owner: Option<Box<dyn crate::writer_lifetime::ExitOwner>>,
    loaded_image_guard: crate::image_identity::LoadedImageGuard,
}

#[derive(Debug)]
pub struct WorkerSession {
    stream: UnixStream,
    process: Option<Child>,
    #[cfg(test)]
    owner: Option<Box<dyn crate::writer_lifetime::ExitOwner>>,
    loaded_image_guard: crate::image_identity::LoadedImageGuard,
    setup: (SnapId, u64),
    seed: u64,
    failed: Cell<bool>,
    counters: RefCell<Vec<(String, u64)>>,
    telemetry_queries: Cell<u64>,
    telemetry_query_failures: Cell<u64>,
    exchanging: Cell<bool>,
    retained_export: RefCell<Vec<crate::cache::WritableExtent>>,
    copies: Arc<CopyWork>,
    transport: Arc<work::Work>,
    import_reuse: crate::import_reuse::ImportReuse,
    shutdown: Option<Result<(), String>>,
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
        config.validate()?;
        let loaded_image_guard = launcher.loaded_image_guard.fork();
        let transport = Arc::new(work::Work::default());
        let (process, stream) = launcher
            .spawn_guarded(&loaded_image_guard, &transport)
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
                #[cfg(test)]
                owner: None,
                loaded_image_guard,
                transport,
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
            #[cfg(test)]
            owner,
            loaded_image_guard,
            transport,
        } = connection;
        let mut session = Self {
            stream,
            process,
            #[cfg(test)]
            owner,
            loaded_image_guard,
            setup: (SnapId(0), 0),
            seed: config.seed,
            failed: Cell::new(false),
            counters: RefCell::default(),
            exchanging: Cell::new(false),
            copies: Arc::default(),
            import_reuse: Default::default(),
            shutdown: None,
            transport,
            telemetry_queries: Cell::new(0),
            telemetry_query_failures: Cell::new(0),
            retained_export: RefCell::new(Vec::new()),
        };
        session
            .loaded_image_guard
            .check_at(crate::image_identity::GuardSite::Admission)?;
        crate::writer_lifetime::reap_worker_quarantine(1);
        let mut request = Out::measured(INIT, &session.copies);
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
        let reply = send_measured(
            &self.stream,
            &request.0,
            fds,
            &self.transport,
            work::Group::request(&request.0),
        )
        .and_then(|()| {
            receive_measured(
                &self.stream,
                usize::MAX,
                None,
                &self.transport,
                Some(work::Group::request(&request.0)),
            )
        });
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
            OK => {
                let result = input.0.to_vec();
                self.copies.copied(CopySite::Decode, result.len());
                Ok(result)
            }
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
        let mut destination = crate::writer_lifetime::ExportDestination::new(&self.retained_export);
        let result: Result<bool, Box<dyn Error>> = (|| {
            let mut request = Out::measured(EXPORT, &self.copies);
            request
                .option(parent.map(|snap| snap.0))
                .u64(target.0)
                .bytes(&attachment.domain)
                .bytes(&attachment.bytes);
            send_measured(
                &self.stream,
                &request.0,
                &[],
                &self.transport,
                work::Group::Export,
            )?;
            let (body, fds) = receive_measured(
                &self.stream,
                EXPORT_FRAME_LIMIT,
                None,
                &self.transport,
                Some(work::Group::Export),
            )?;
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
                return Ok(false);
            }
            let len = input.len()?;
            input.finish()?;
            if len == 0 {
                return Err(malformed("empty allocation request").into());
            }
            destination.extent = match index.extent(len) {
                Ok(extent) => Some(extent),
                Err(error) => {
                    denied = Some(error);
                    None
                }
            };
            self.check_loaded_image_at(crate::image_identity::GuardSite::ParentExtentAllocated)?;
            if let Some(extent) = destination.extent.as_ref() {
                let SharedRange {
                    fd,
                    offset,
                    len: usable,
                } = extent.shared_range();
                if usable < len {
                    return Err(malformed("allocated range too short").into());
                }
                destination.exposed = true;
                send_measured(
                    &self.stream,
                    &Out::measured(EXTENT_READY, &self.copies)
                        .len(offset)
                        .len(len)
                        .0,
                    &[fd],
                    &self.transport,
                    work::Group::Export,
                )?;
            } else {
                send_measured(
                    &self.stream,
                    &Out::measured(EXTENT_DENIED, &self.copies).0,
                    &[],
                    &self.transport,
                    work::Group::Export,
                )?;
            }
            let (body, fds) = receive_measured(
                &self.stream,
                EXPORT_FRAME_LIMIT,
                None,
                &self.transport,
                Some(work::Group::Export),
            )?;
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
                return Ok(false);
            }
            let reply = self.reply_status(&body)?;
            In(&reply).finish()?;
            Ok(true)
        })();
        match result {
            Ok(true) => Ok(destination
                .extent
                .take()
                .expect("allocated export destination")),
            Ok(false) => match rejected {
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

    #[must_use]
    pub fn retained_export_bytes(&self) -> u64 {
        self.retained_export
            .borrow()
            .iter()
            .fold(0u64, |sum, extent| sum.saturating_add(extent.len() as u64))
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

#[cfg(test)]
type ExitPauseHook = Box<dyn FnMut(&crate::writer_lifetime::WorkerId)>;
#[cfg(test)]
thread_local! {
    static EXIT_PAUSE_HOOK: RefCell<Option<ExitPauseHook>> = RefCell::new(None);
}

fn exit_pause(_id: &crate::writer_lifetime::WorkerId) {
    #[cfg(test)]
    if EXIT_PAUSE_HOOK.with(|hook| {
        let mut hook = hook.borrow_mut();
        if let Some(hook) = hook.as_mut() {
            hook(_id);
            true
        } else {
            false
        }
    }) {
        return;
    }
    std::thread::sleep(EXIT_POLL);
}

impl WorkerSession {
    fn shutdown_owned(&mut self) -> Result<(), String> {
        if let Some(result) = &self.shutdown {
            return result.clone();
        }
        self.failed.set(true);
        self.shutdown = Some(Err(
            "owned worker shutdown was interrupted; exit unconfirmed".into(),
        ));
        let result = self.shutdown_once();
        self.shutdown = Some(result.clone());
        result
    }
    fn shutdown_once(&mut self) -> Result<(), String> {
        use crate::writer_lifetime::{ExitPoll, poll_worker, retain_worker};
        let _ = self.stream.shutdown(Shutdown::Both);
        let owner = self
            .process
            .take()
            .map(|process| Box::new(process) as Box<dyn crate::writer_lifetime::ExitOwner>);
        #[cfg(test)]
        let owner = owner.or_else(|| self.owner.take());
        let extents = std::mem::take(self.retained_export.get_mut());
        if owner.is_none() && extents.is_empty() {
            return Ok(());
        }
        let id = retain_worker(owner, extents);
        for _ in 0..EXIT_POLLS {
            match poll_worker(&id, false) {
                ExitPoll::Exited => return Ok(()),
                ExitPoll::Ambiguous => {
                    return Err("owned worker exit is ambiguous; resources retained".into());
                }
                ExitPoll::Running => exit_pause(&id),
            }
        }
        crate::writer_lifetime::allow_worker_signal(&id);
        match poll_worker(&id, true) {
            ExitPoll::Exited => return Ok(()),
            ExitPoll::Ambiguous => {
                return Err("owned worker exit is ambiguous; resources retained".into());
            }
            ExitPoll::Running => {}
        }
        for _ in 0..10 {
            match poll_worker(&id, false) {
                ExitPoll::Exited => return Ok(()),
                ExitPoll::Ambiguous => {
                    return Err("owned worker exit is ambiguous; resources retained".into());
                }
                ExitPoll::Running => exit_pause(&id),
            }
        }
        Err("owned worker exit remains unconfirmed after shutdown; resources retained".into())
    }
}
impl Drop for WorkerSession {
    fn drop(&mut self) {
        let _ = self.shutdown_owned();
    }
}

impl SearchSession for WorkerSession {
    fn shutdown(&mut self) -> Result<(), Box<dyn Error>> {
        self.shutdown_owned().map_err(Into::into)
    }
    fn setup_handle(&self) -> (SnapId, u64) {
        self.setup
    }

    fn state_hash(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
        let reply = self.call(&Out::measured(STATE_HASH, &self.copies), &[])?;
        Ok(reply
            .as_slice()
            .try_into()
            .map_err(|_| malformed("state hash"))?)
    }

    fn event_history(&mut self) -> Result<control_proto::EventHistory, Box<dyn Error>> {
        let reply = self.call(&Out::measured(EVENT_HISTORY, &self.copies), &[])?;
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
        self.call(&Out::measured(CONSOLE_TAIL, &self.copies), &[])
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
        if let Ok(Ok(counters)) = self
            .call(&Out::measured(TELEMETRY, &self.copies), &[])
            .map(decode)
        {
            fresh = true;
            self.counters.replace(
                counters
                    .into_iter()
                    .map(|(name, value)| {
                        let name = name.strip_prefix("guard.session.").map_or_else(
                            || name.clone(),
                            |suffix| format!("guard.child.session.{suffix}"),
                        );
                        let name = name.strip_prefix("copy.session.").map_or_else(
                            || name.clone(),
                            |suffix| format!("copy.child.session.{suffix}"),
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
        counters.extend(self.transport.counters("transport.parent"));
        counters.extend(self.copies.counters("copy.parent"));
        counters.extend(self.import_reuse.counters());
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
        let reply = self
            .call(
                Out::measured(OWNED_PAGES, &self.copies).u64(snapshot.0),
                &[],
            )
            .ok()?;
        let mut input = In(&reply);
        let pages = input.option().ok()?;
        input.finish().ok()?;
        pages
    }

    fn store_bytes(&self) -> Option<u64> {
        let reply = self
            .call(&Out::measured(STORE_BYTES, &self.copies), &[])
            .ok()?;
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
        let layout = crate::import_layout::ImportLayout::from_extents(&chain)?;
        let mut request = Out::measured(IMPORT, &self.copies);
        layout.encode_into(near.0, &mut request.0);
        let fds: Vec<BorrowedFd<'_>> = layout
            .backing_rows
            .iter()
            .map(|row| ranges[*row].fd)
            .collect();
        let reply = self.call(&request, &fds)?;
        let mut input = In(&reply);
        let snap = input.snap()?;
        input.finish()?;
        if let Err(error) = index.verify_lease(lease) {
            let _ = self.drop_snapshot(snap.0);
            return Err(error.into());
        }
        self.import_reuse.accepted(&chain);
        Ok(CachedSnapshotReceipt {
            snapshot: snap.0,
            moment: snap.1,
            machine_commitment,
            attachment,
            object_commitment,
        })
    }

    fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        self.call_unit(Out::measured(REPLAY, &self.copies).u64(snapshot.0))
    }

    fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>> {
        self.call_unit(Out::measured(DROP_SNAPSHOT, &self.copies).u64(snapshot.0))
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
        self.call_unit(
            Out::measured(BRANCH, &self.copies)
                .u64(snapshot.0)
                .bytes(&spec.encode()),
        )
    }

    fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
        let reply = self.call(Out::measured(RUN_UNTIL, &self.copies).u64(deadline), &[])?;
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
            Out::measured(RUN_RESOLVING, &self.copies)
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
        self.call_snap(&Out::measured(SNAPSHOT, &self.copies))
    }

    fn sdk_events(&mut self) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
        self.sdk_events_since(0)
    }

    fn sdk_events_since(&mut self, offset: u32) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
        let reply = self.call(
            Out::measured(SDK_EVENTS, &self.copies).u64(u64::from(offset)),
            &[],
        )?;
        let mut input = In(&reply);
        let count = input.len()?;
        offset
            .checked_add(u32::try_from(count).map_err(|_| malformed("SDK event count overflows"))?)
            .ok_or_else(|| malformed("SDK event offset overflows"))?;
        let events = (0..count)
            .map(|_| {
                let moment = input.u64()?;
                let id = u32::try_from(input.u64()?).map_err(|_| malformed("event id"))?;
                Ok((moment, id, input.owned_bytes(&self.copies)?))
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

fn take_init(input: &mut In<'_>, copies: &CopyWork) -> io::Result<Init> {
    let config = take_config(input)?;
    let kernel = input.owned_bytes(copies)?;
    let initramfs = input.owned_bytes(copies)?;
    let setup_payloads = (0..input.len()?)
        .map(|_| input.owned_bytes(copies))
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
    copies: Arc<CopyWork>,
    transport: Arc<work::Work>,
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
        let mut out = Out::measured_empty(&self.copies);
        if op == INIT {
            let init = take_init(&mut input, &self.copies)?;
            input.finish()?;
            if self.session.is_some() {
                return Err("session worker is already initialized".into());
            }
            init.config.validate()?;
            let session = open(init)?;
            let (setup, at) = session.setup_handle();
            self.session = Some(session);
            out.u64(setup.0).u64(at);
            return Ok(out);
        }
        let copies = Arc::clone(&self.copies);
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
                counters.extend(self.transport.counters("transport.child.loop"));
                counters.extend(self.copies.counters("copy.child.loop"));
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
                    domain: input.owned_bytes(&copies)?,
                    bytes: input.owned_bytes(&copies)?,
                };
                input.finish()?;
                if !fds.is_empty() {
                    return Err(malformed("export descriptors").into());
                }
                session.export_delta_into(parent, target, &attachment, allocate)?;
            }
            IMPORT => {
                let (near, layout) =
                    crate::import_layout::ImportLayout::decode(input.0, fds.len())?;
                let near = SnapId(near);
                let mappings = layout
                    .ranges
                    .iter()
                    .map(|row| {
                        Mapping::measured(
                            fds[row.slot].as_fd(),
                            row.offset,
                            row.len,
                            false,
                            &copies,
                        )
                    })
                    .collect::<io::Result<Vec<_>>>()?;
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
                let answer = Answer(input.owned_bytes(&copies)?);
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

#[cfg(test)]
fn status(result: Result<Out, Box<dyn Error>>) -> Vec<u8> {
    status_measured(result, &Arc::default())
}

fn status_measured(result: Result<Out, Box<dyn Error>>, work: &Arc<CopyWork>) -> Vec<u8> {
    match result {
        Ok(out) => {
            let mut body = Vec::with_capacity(out.0.len() + 1);
            body.push(OK);
            body.extend_from_slice(&out.0);
            work.copied(CopySite::Status, out.0.len());
            body
        }
        Err(error) => {
            let mut out = Out::measured(FAILED, work);
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

#[cfg(test)]
fn serve(
    stream: &UnixStream,
    open: &mut Open<'_>,
    loaded_image_guard: crate::image_identity::LoadedImageGuard,
) -> Result<(), Box<dyn Error>> {
    serve_measured(stream, open, loaded_image_guard, Arc::default())
}
fn serve_measured(
    stream: &UnixStream,
    open: &mut Open<'_>,
    loaded_image_guard: crate::image_identity::LoadedImageGuard,
    transport: Arc<work::Work>,
) -> Result<(), Box<dyn Error>> {
    let mut served = Served {
        loaded_image_guard: Some(loaded_image_guard.clone()),
        transport: Arc::clone(&transport),
        ..Served::default()
    };
    loaded_image_guard.check_at(crate::image_identity::GuardSite::ChildReceive)?;
    loop {
        let (body, fds) = match receive_measured(stream, usize::MAX, None, &transport, None) {
            Ok(frame) => frame,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut requested = false;
        let mut allocation_broken = None;
        let copies = Arc::clone(&served.copies);
        let mut allocate = |len| {
            let result: Result<Option<Mapping>, Box<dyn Error>> = (|| {
                if std::mem::replace(&mut requested, true) {
                    return Err(malformed("repeated allocation").into());
                }
                loaded_image_guard.check_at(crate::image_identity::GuardSite::ChildExtentBefore)?;
                send_measured(
                    stream,
                    &Out::op(NEED_EXTENT).len(len).0,
                    &[],
                    &transport,
                    work::Group::Export,
                )?;
                loaded_image_guard.check_at(crate::image_identity::GuardSite::ChildExtentSent)?;
                let (body, fds) = receive_measured(
                    stream,
                    EXPORT_FRAME_LIMIT,
                    None,
                    &transport,
                    Some(work::Group::Export),
                )?;
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
                        Ok(Some(Mapping::measured(
                            fd.as_fd(),
                            offset,
                            len,
                            true,
                            &copies,
                        )?))
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
        let reply = status_measured(served.handle(&body, &fds, open, &mut allocate), &copies);
        if let Some(cause) = allocation_broken {
            return Err(malformed(&format!("allocation exchange failed: {cause}")).into());
        }
        loaded_image_guard.check_at(crate::image_identity::GuardSite::ChildHandled)?;
        drop(fds);
        send_measured(stream, &reply, &[], &transport, work::Group::request(&body))?;
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
    let transport = Arc::new(work::Work::default());
    send_measured(&stream, &hello.0, &[], &transport, work::Group::Hello)?;
    serve_measured(&stream, open, loaded_image_guard, transport)
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

    #[derive(Clone, Debug)]
    struct FixtureThread(std::sync::Arc<std::sync::Mutex<FixtureJoin>>);
    #[derive(Debug)]
    struct FixtureJoin {
        handle: Option<std::thread::JoinHandle<()>>,
        result: Option<std::thread::Result<()>>,
        joined: bool,
    }
    impl FixtureThread {
        fn spawn(run: impl FnOnce() + Send + 'static) -> Self {
            Self(std::sync::Arc::new(std::sync::Mutex::new(FixtureJoin {
                handle: Some(std::thread::spawn(run)),
                result: None,
                joined: false,
            })))
        }
        fn join(self) -> std::thread::Result<()> {
            let mut state = self.0.lock().unwrap();
            if let Some(handle) = state.handle.take() {
                state.result = Some(handle.join());
                state.joined = true;
            }
            state.result.take().expect("fixture join result")
        }
    }
    impl crate::writer_lifetime::ExitOwner for FixtureThread {
        fn try_exited(&mut self) -> io::Result<bool> {
            let mut state = self.0.lock().unwrap();
            if state.joined {
                return Ok(true);
            }
            if state
                .handle
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished)
            {
                let handle = state.handle.take().unwrap();
                state.result = Some(handle.join());
                state.joined = true;
                return Ok(true);
            }
            Ok(false)
        }
        fn terminate(&mut self) -> io::Result<()> {
            Err(io::Error::other("fixture thread has no signal operation"))
        }
    }

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
                ("copy.session.fixture.bytes".to_owned(), self.runs),
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
            sidecar.extend_from_slice(&11u16.to_le_bytes());
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
                assert_eq!(
                    send_with_fds(&sender, &[1], &rights, &mut work::Delta::default()).unwrap(),
                    1
                );
                let mut delta = work::Delta::default();
                let received = recv_with_fds(
                    &receiver,
                    &mut [0; 1],
                    Some(Instant::now() + Duration::from_secs(1)),
                    &mut delta,
                );
                assert_eq!(delta.header, 1);
                assert_eq!(delta.body, 0);
                assert_eq!(delta.fds, count as u64);
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
                match send_with_fds(&sender, &[1], &rights, &mut work::Delta::default()) {
                    Ok(1) => {
                        let error = recv_with_fds(
                            &receiver,
                            &mut [0; 1],
                            Some(Instant::now() + Duration::from_secs(1)),
                            &mut work::Delta::default(),
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
                    send_with_fds(
                        &sender,
                        &[value],
                        &[descriptor.as_fd()],
                        &mut work::Delta::default()
                    )
                    .unwrap(),
                    1
                );
            }
            for value in [1, 2, 3] {
                let mut bytes = [0; 16];
                let (size, rights) = recv_with_fds(
                    &receiver,
                    &mut bytes,
                    Some(Instant::now() + Duration::from_secs(1)),
                    &mut work::Delta::default(),
                )
                .unwrap();
                assert_eq!(&bytes[..size], &[value]);
                assert_eq!(rights.len(), 1);
                drop(rights);
                assert_eq!(live_task_descriptors(), active);
            }
            assert_eq!(
                send_with_fds(
                    &sender,
                    &[],
                    &[descriptor.as_fd()],
                    &mut work::Delta::default()
                )
                .unwrap(),
                0
            );
            assert_eq!(
                send_with_fds(
                    &sender,
                    &[9],
                    &[descriptor.as_fd()],
                    &mut work::Delta::default()
                )
                .unwrap(),
                1
            );
            let (size, rights) = recv_with_fds(
                &receiver,
                &mut [0; 16],
                Some(Instant::now() + Duration::from_secs(1)),
                &mut work::Delta::default(),
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
                &mut work::Delta::default(),
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
                                    &vec![descriptor.as_fd(); count],
                                    &mut work::Delta::default(),
                                )
                                .unwrap(),
                                HEADER - 1
                            );
                        } else {
                            sender.write_all(&header).unwrap();
                            sender.write_all(&[1]).unwrap();
                            assert_eq!(
                                send_with_fds(
                                    &sender,
                                    &[2],
                                    &vec![descriptor.as_fd(); count],
                                    &mut work::Delta::default()
                                )
                                .unwrap(),
                                1
                            );
                        }
                        sender.shutdown(std::net::Shutdown::Write).unwrap();
                        let deadline = timed.then(|| Instant::now() + Duration::from_secs(1));
                        let mut delta = work::Delta::default();
                        let result = recv_raw(&receiver, 2, deadline, &mut delta);
                        assert_eq!(delta.header, HEADER as u64);
                        assert_eq!(delta.body, if header_remainder { 0 } else { 2 });
                        assert_eq!(delta.fds, count as u64);
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
                read_frame_exact(&receiver, &mut [0; 1], Some(Instant::now()), &mut 0, &mut 0)
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
                    Some(Instant::now() + Duration::from_secs(1)),
                    &mut 0,
                    &mut 0,
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
        assert_eq!(
            send_with_fds(&left, &[1], &fds, &mut work::Delta::default()).unwrap(),
            1
        );
        let error = recv_with_fds_control_space(
            &right,
            &mut [0; 1],
            None,
            control_space(MAX_FDS),
            &mut work::Delta::default(),
        )
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

    #[test]
    fn measured_frames_count_exact_bytes_fds_and_failed_partial_bodies() {
        let (left, right) = UnixStream::pair().unwrap();
        let sent = work::Work::default();
        let received = work::Work::default();
        let file = tempfile::tempfile().unwrap();
        send_measured(
            &left,
            &[IMPORT, 7, 8],
            &[file.as_fd(), file.as_fd()],
            &sent,
            work::Group::Import,
        )
        .unwrap();
        let (body, fds) = receive_measured(&right, 32, None, &received, None).unwrap();
        assert_eq!(body, [IMPORT, 7, 8]);
        assert_eq!(fds.len(), 2);
        let tx = sent.counters("t").into_iter().collect::<BTreeMap<_, _>>();
        let rx = received
            .counters("t")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        for (name, value) in [
            ("attempts", 1),
            ("completed", 1),
            ("errors", 0),
            ("header_bytes", 16),
            ("body_bytes", 3),
            ("fd_handles", 2),
        ] {
            assert_eq!(tx[&format!("t.import.send.{name}")], value);
            assert_eq!(rx[&format!("t.import.receive.{name}")], value);
        }
        let mut truncated = [0; HEADER];
        truncated[..8].copy_from_slice(&5u64.to_le_bytes());
        (&left).write_all(&truncated).unwrap();
        (&left).write_all(&[1, 2]).unwrap();
        left.shutdown(Shutdown::Write).unwrap();
        let error = receive_measured(&right, 32, None, &received, None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        let rx = received
            .counters("t")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(rx["t.unknown.receive.header_bytes"], 16);
        assert_eq!(rx["t.unknown.receive.body_bytes"], 2);
        assert_eq!(rx["t.unknown.receive.completed"], 0);
        assert_eq!(rx["t.unknown.receive.errors"], 1);
    }

    #[test]
    fn measured_header_rejections_preserve_actual_io() {
        for (len, declared_fds, limit) in [(9, 0, 8), (0, 1, 8)] {
            let (left, right) = UnixStream::pair().unwrap();
            let owner = work::Work::default();
            let mut header = [0; HEADER];
            header[..8].copy_from_slice(&(len as u64).to_le_bytes());
            header[8..].copy_from_slice(&(declared_fds as u64).to_le_bytes());
            (&left).write_all(&header).unwrap();
            assert!(receive_measured(&right, limit, None, &owner, None).is_err());
            let values = owner.counters("t").into_iter().collect::<BTreeMap<_, _>>();
            assert_eq!(values["t.unknown.receive.header_bytes"], 16);
            assert_eq!(values["t.unknown.receive.body_bytes"], 0);
            assert_eq!(values["t.unknown.receive.fd_handles"], 0);
            assert_eq!(values["t.unknown.receive.errors"], 1);
        }
    }

    #[test]
    fn transport_session_groups_cover_setup_run_sdk_export_import_and_parent_replies() {
        let (mut source, server) = threaded(3);
        let (mut destination, other) = threaded(5);
        source.run_until(20).unwrap();
        source.sdk_events_since(0).unwrap();
        let (snapshot, _) = source.snapshot().unwrap();
        let index = LocalIndex::new(1 << 20);
        let lease = source
            .publish_snapshot(&index, namespace(), b"transport", None, snapshot, 1)
            .unwrap();
        destination
            .import_cached(&index, &lease, SnapId(1))
            .unwrap();
        let source_values = source
            .telemetry_counters()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let destination_values = destination
            .telemetry_counters()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        for (values, group, count) in [
            (&source_values, "setup", 1),
            (&source_values, "run", 1),
            (&source_values, "observe", 1),
            (&source_values, "export", 2),
            (&destination_values, "import", 1),
        ] {
            for owner in ["transport.parent", "transport.child.loop"] {
                for direction in ["send", "receive"] {
                    assert_eq!(
                        values[&format!("{owner}.{group}.{direction}.completed")],
                        count
                    );
                    assert_eq!(
                        values[&format!("{owner}.{group}.{direction}.header_bytes")],
                        16 * count
                    );
                    assert_eq!(values[&format!("{owner}.{group}.{direction}.errors")], 0);
                }
            }
        }
        assert_eq!(source_values["transport.parent.export.send.fd_handles"], 1);
        assert_eq!(
            source_values["transport.child.loop.export.receive.fd_handles"],
            1
        );
        assert_eq!(
            destination_values["transport.parent.import.send.fd_handles"],
            1
        );
        assert_eq!(
            destination_values["transport.child.loop.import.receive.fd_handles"],
            1
        );
        assert!(
            !source_values
                .keys()
                .any(|key| key.starts_with("transport.parent.unknown.receive"))
        );
        assert!(
            !destination_values
                .keys()
                .any(|key| key.starts_with("transport.parent.unknown.receive"))
        );
        index.release(lease);
        drop(source);
        drop(destination);
        server.join().unwrap();
        other.join().unwrap();
    }

    #[test]
    fn hello_transport_owner_moves_into_connected_session() {
        let (parent, child) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let work = Arc::new(work::Work::default());
            let mut hello = Out::default();
            hello.bytes(IMAGE_HELLO).bytes(b"fixture");
            send_measured(&child, &hello.0, &[], &work, work::Group::Hello).unwrap();
            serve_measured(
                &child,
                &mut Fake::open,
                crate::image_identity::LoadedImageGuard::fixture(),
                work,
            )
            .unwrap();
        });
        let work = Arc::new(work::Work::default());
        let (hello, fds) = receive_measured(
            &parent,
            IMAGE_HELLO_LIMIT,
            None,
            &work,
            Some(work::Group::Hello),
        )
        .unwrap();
        assert!(fds.is_empty());
        let mut input = In(&hello);
        assert_eq!(input.bytes().unwrap(), IMAGE_HELLO);
        assert_eq!(input.string().unwrap(), "fixture");
        input.finish().unwrap();
        let session = WorkerSession::connect(
            WorkerConnection {
                stream: parent,
                process: None,
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
                transport: work,
                owner: None,
            },
            &[3],
            b"i",
            &config(),
            &[],
            "fake",
        )
        .unwrap();
        let values = session
            .telemetry_counters()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(values["transport.parent.hello.receive.completed"], 1);
        assert_eq!(values["transport.parent.hello.receive.header_bytes"], 16);
        assert_eq!(
            values["transport.parent.hello.receive.body_bytes"],
            (8 + IMAGE_HELLO.len() + 8 + b"fixture".len()) as u64
        );
        assert_eq!(values["transport.child.loop.hello.send.completed"], 1);
        assert_eq!(values["transport.child.loop.hello.send.header_bytes"], 16);
        assert_eq!(
            values["transport.child.loop.hello.send.body_bytes"],
            values["transport.parent.hello.receive.body_bytes"]
        );
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn explicit_copy_sites_and_mapping_spans_are_disjoint() {
        let owner = Arc::new(CopyWork::default());
        let mut out = Out::measured(7, &owner);
        out.u64(42).bytes(b"hello");
        let length = out.0.len();
        let reply = status_measured(Ok(out), &owner);
        let counters = owner.counters("c").into_iter().collect::<BTreeMap<_, _>>();
        assert_eq!(counters["c.encoded_payload_bytes"], 5);
        assert_eq!(counters["c.status_payload_bytes"], length as u64);
        let mut encoded = Out::default();
        encoded.bytes(b"data");
        assert_eq!(In(&encoded.0).owned_bytes(&owner).unwrap(), b"data");
        assert_eq!(
            owner.counters("c").into_iter().collect::<BTreeMap<_, _>>()["c.decoded_payload_bytes"],
            4
        );
        assert_eq!(reply.len(), length + 1);
        let file = tempfile::tempfile().unwrap();
        file.set_len(8192).unwrap();
        let prior = owner.counters("c").into_iter().collect::<BTreeMap<_, _>>();
        let mapping = Mapping::measured(file.as_fd(), 3, 8, false, &owner).unwrap();
        assert_eq!(mapping.bytes().len(), 8);
        assert!(Mapping::measured(file.as_fd(), 8190, 8, false, &owner).is_err());
        let after = owner.counters("c").into_iter().collect::<BTreeMap<_, _>>();
        assert_eq!(after["c.import_mapping.attempts"], 2);
        assert_eq!(after["c.import_mapping.errors"], 1);
        assert_eq!(after["c.import_mapping.range_bytes"], 8);
        assert_eq!(after["c.import_mapping.span_bytes"], 11);
        for name in [
            "encoded_payload_bytes",
            "status_payload_bytes",
            "decoded_payload_bytes",
        ] {
            assert_eq!(after[&format!("c.{name}")], prior[&format!("c.{name}")]);
        }
    }

    #[test]
    fn worker_copy_reports_bind_export_import_mappings_and_status_to_the_served_owner() {
        let index = LocalIndex::new(1 << 20);
        let (mut session, server) = threaded(3);
        let lease = session
            .publish_snapshot(&index, namespace(), b"copies", None, SnapId(1), 1)
            .unwrap();
        let chain = index.chain(&lease).unwrap();
        assert_eq!(chain.len(), 1);
        let range = chain[0].shared_range();
        assert_eq!(range.offset, 0);
        let span = range.len;
        session.import_cached(&index, &lease, SnapId(1)).unwrap();
        let before = session
            .copies
            .counters("parent")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let config = config();
        let encoded =
            (config.cmdline.len() + config.identity_tag.len() + 1 + 1 + b"fake".len()) as u64;
        assert_eq!(before["parent.encoded_payload_bytes"], encoded);
        assert_eq!(before["parent.decoded_payload_bytes"], 32);
        let counters = session
            .telemetry_counters()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        for direction in ["import", "export"] {
            for (leaf, expected) in [
                ("attempts", 1),
                ("errors", 0),
                ("range_bytes", range.len as u64),
                ("span_bytes", span as u64),
            ] {
                assert_eq!(
                    counters[&format!("copy.child.loop.{direction}_mapping.{leaf}")],
                    expected
                );
                assert_eq!(
                    counters[&format!("copy.parent.{direction}_mapping.{leaf}")],
                    0
                );
            }
        }
        assert_eq!(counters["copy.child.loop.encoded_payload_bytes"], 0);
        assert_eq!(counters["copy.child.loop.decoded_payload_bytes"], 2);
        assert_eq!(counters["copy.child.loop.status_payload_bytes"], 32);
        assert_eq!(counters["copy.parent.encoded_payload_bytes"], encoded);
        assert_eq!(counters["copy.parent.status_payload_bytes"], 0);
        let after = session
            .copies
            .counters("parent")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            counters["copy.parent.decoded_payload_bytes"],
            after["parent.decoded_payload_bytes"]
        );
        assert!(after["parent.decoded_payload_bytes"] > before["parent.decoded_payload_bytes"]);
        index.release(lease);
        drop((chain, session));
        server.join().unwrap();
    }

    #[test]
    fn sdk_worker_counts_actual_status_and_event_payload_copies() {
        let (mut session, server) = threaded(3);
        let before = session
            .copies
            .counters("c")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(session.sdk_events_since(0).unwrap(), vec![(5, 7, vec![3])]);
        let after = session
            .copies
            .counters("c")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            after["c.decoded_payload_bytes"] - before["c.decoded_payload_bytes"],
            34
        );
        assert_eq!(
            after["c.encoded_payload_bytes"],
            before["c.encoded_payload_bytes"]
        );
        assert!(session.sdk_events_since(1).unwrap().is_empty());
        let empty = session
            .copies
            .counters("c")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            empty["c.decoded_payload_bytes"] - after["c.decoded_payload_bytes"],
            8
        );
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn transport_queries_replace_child_snapshot_without_duplicate_or_added_totals() {
        let (mut session, server) = threaded(3);
        for expected in [1, 2] {
            let values = session.telemetry_counters();
            let matches = values
                .iter()
                .filter(|(name, _)| name == "transport.child.loop.telemetry.receive.completed")
                .collect::<Vec<_>>();
            assert_eq!(
                matches.iter().map(|(_, value)| value).sum::<u64>(),
                expected
            );
            assert_eq!(matches.len(), 1);
            let transport = values
                .iter()
                .filter(|(name, _)| name.starts_with("transport.child.loop."))
                .map(|(name, _)| name)
                .collect::<Vec<_>>();
            assert_eq!(
                transport.len(),
                transport
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
            );
        }
        assert!(session.run_until(u64::MAX).is_err());
        let values = session.telemetry_counters();
        assert_eq!(
            values
                .iter()
                .filter(|(name, _)| name == "transport.child.loop.telemetry.receive.completed")
                .map(|(_, value)| value)
                .sum::<u64>(),
            2
        );
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn sdk_dispatch_child_snapshots_replace_cumulative_values_and_survive_query_failure() {
        let (parent, child) = UnixStream::pair().unwrap();
        let server = FixtureThread::spawn(move || {
            let mut served = Served::default();
            let (body, fds) = recv(&child).unwrap();
            send(
                &child,
                &status(served.handle(&body, &fds, &mut Fake::open, &mut |_| panic!("allocation"))),
                &[],
            )
            .unwrap();
            for value in [5, 7] {
                let (body, fds) = recv(&child).unwrap();
                assert_eq!(body, [TELEMETRY]);
                assert!(fds.is_empty());
                let mut out = Out::op(OK);
                out.len(2)
                    .bytes(b"sdk_dispatch.event.request_guard_calls")
                    .u64(value)
                    .bytes(b"sdk_dispatch.event_shape.2.9.0.request_guard_calls")
                    .u64(value);
                send(&child, &out.0, &[]).unwrap();
            }
            let (body, fds) = recv(&child).unwrap();
            assert_eq!(body, [TELEMETRY]);
            assert!(fds.is_empty());
            send(&child, &[OK], &[]).unwrap();
        });
        let session = WorkerSession::connect(
            WorkerConnection {
                stream: parent,
                process: None,
                owner: Some(Box::new(server.clone())),
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
                transport: Arc::default(),
            },
            &[3],
            b"i",
            &config(),
            &[],
            "fake",
        )
        .unwrap();
        for expected in [5, 7, 7] {
            let values = session.telemetry_counters();
            for key in [
                "sdk_dispatch.event.request_guard_calls",
                "sdk_dispatch.event_shape.2.9.0.request_guard_calls",
            ] {
                assert_eq!(
                    values
                        .iter()
                        .filter(|(name, _)| name == key)
                        .map(|(_, value)| *value)
                        .collect::<Vec<_>>(),
                    [expected]
                );
            }
        }
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn telemetry_query_decode_and_transport_errors_preserve_the_last_child_snapshot() {
        for transport_failure in [false, true] {
            let (parent, child) = UnixStream::pair().unwrap();
            let server = FixtureThread::spawn(move || {
                let mut served = Served::default();
                let (body, fds) = recv(&child).unwrap();
                send(
                    &child,
                    &status(
                        served.handle(&body, &fds, &mut Fake::open, &mut |_| panic!("allocation")),
                    ),
                    &[],
                )
                .unwrap();
                let (body, fds) = recv(&child).unwrap();
                assert_eq!(body, [TELEMETRY]);
                assert!(fds.is_empty());
                let mut reply = Out::op(OK);
                reply
                    .len(1)
                    .bytes(b"transport.child.loop.run.receive.completed")
                    .u64(7);
                send(&child, &reply.0, &[]).unwrap();
                let (body, fds) = recv(&child).unwrap();
                assert_eq!(body, [TELEMETRY]);
                assert!(fds.is_empty());
                if transport_failure {
                    let mut header = [0; HEADER];
                    header[..8].copy_from_slice(&8u64.to_le_bytes());
                    (&child).write_all(&header).unwrap();
                    (&child).write_all(&[OK]).unwrap();
                } else {
                    send(&child, &[OK], &[]).unwrap();
                }
            });
            let session = WorkerSession::connect(
                WorkerConnection {
                    stream: parent,
                    process: None,
                    owner: Some(Box::new(server.clone())),
                    loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
                    transport: Arc::default(),
                },
                &[3],
                b"i",
                &config(),
                &[],
                "fake",
            )
            .unwrap();
            let first = session.telemetry_counters();
            assert_eq!(
                first
                    .iter()
                    .filter(|(k, _)| k == "transport.child.loop.run.receive.completed")
                    .map(|(_, v)| v)
                    .copied()
                    .collect::<Vec<_>>(),
                [7]
            );
            assert!(!session.abandoned());
            let next = session.telemetry_counters();
            assert_eq!(
                next.iter()
                    .filter(|(k, _)| k == "transport.child.loop.run.receive.completed")
                    .map(|(_, v)| v)
                    .copied()
                    .collect::<Vec<_>>(),
                [7]
            );
            let values = next.into_iter().collect::<BTreeMap<_, _>>();
            assert_eq!(values["guard.child.telemetry_query_failures"], 1);
            assert_eq!(values["transport.parent.telemetry.send.completed"], 2);
            assert_eq!(
                values["transport.parent.telemetry.receive.errors"],
                u64::from(transport_failure)
            );
            drop(session);
            server.join().unwrap();
        }
    }

    #[test]
    fn truncated_ancillary_frames_keep_recovered_descriptor_counts() {
        const CHILD: &str = "HARMONY_TRANSPORT_TRUNCATED_COUNTER_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "session::worker::tests::truncated_ancillary_frames_keep_recovered_descriptor_counts",
                    "--nocapture",
                ])
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
        let (file, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let excess = tempfile::tempfile().unwrap();
        let capacity = (control_space(MAX_FDS) - size_of::<libc::cmsghdr>()) / size_of::<RawFd>();
        let mut fds = vec![file.as_fd(); capacity];
        fds.push(excess.as_fd());
        let mut header = [0; HEADER];
        header[8..].copy_from_slice(&((capacity + 1) as u64).to_le_bytes());
        assert_eq!(
            send_with_fds(&left, &header, &fds, &mut work::Delta::default()).unwrap(),
            HEADER
        );
        let owner = work::Work::default();
        let mut delta = work::Delta::default();
        let result = recv_with_fds_control_space(
            &right,
            &mut [0; HEADER],
            None,
            control_space(MAX_FDS),
            &mut delta,
        );
        owner.record(work::Group::Unknown, true, delta, result.is_ok());
        let error = result.unwrap_err();
        assert!(
            error.to_string().contains("descriptors were truncated"),
            "{error}"
        );
        let values = owner.counters("t").into_iter().collect::<BTreeMap<_, _>>();
        assert_eq!(values["t.unknown.receive.header_bytes"], 16);
        assert_eq!(values["t.unknown.receive.body_bytes"], 0);
        assert_eq!(values["t.unknown.receive.fd_handles"], capacity as u64);
        assert_eq!(values["t.unknown.receive.errors"], 1);
        assert_eq!(values["t.unknown.receive.completed"], 0);
        drop(fds);
        drop(file);
        drop(excess);
        drop(left);
        drop(right);
        assert_eq!(
            peer.read(&mut [0; 1]).unwrap(),
            0,
            "all recovered socket rights must close on rejection"
        );
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn measured_hello_deadline_keeps_partial_frame_work() {
        let (left, right) = UnixStream::pair().unwrap();
        let mut header = [0; HEADER];
        header[..8].copy_from_slice(&5u64.to_le_bytes());
        (&left).write_all(&header).unwrap();
        (&left).write_all(&[1, 2]).unwrap();
        let owner = work::Work::default();
        let error = receive_measured(
            &right,
            32,
            Some(Instant::now() + Duration::from_millis(200)),
            &owner,
            Some(work::Group::Hello),
        )
        .unwrap_err();
        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ),
            "{error}"
        );
        let values = owner.counters("t").into_iter().collect::<BTreeMap<_, _>>();
        assert_eq!(values["t.hello.receive.header_bytes"], 16);
        assert_eq!(values["t.hello.receive.body_bytes"], 2);
        assert_eq!(values["t.hello.receive.errors"], 1);
        assert_eq!(values["t.hello.receive.completed"], 0);
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
    fn qualified_worker_launch_and_received_init_refuse_before_starting_execution() {
        for composition in [
            execution_contract::Composition::RetainedLinuxContinuation9673,
            execution_contract::Composition::UmlFaultsWholeState,
        ] {
            let config = config()
                .with_qualification(execution_contract::Requirement::Qualified(composition));
            let launcher = WorkerLauncher {
                program: PathBuf::from("/must-not-spawn-unqualified-worker"),
                args: Vec::new(),
                expected_image: "fixture".into(),
                expected_main: "fixture".into(),
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
            };
            let (result, entries, _) = crate::image_identity::trace_checks(None, || {
                WorkerSession::spawn(&launcher, b"k", b"i", &config, &[], "fake")
            });
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("qualification refused")
            );
            assert_eq!(entries, 0);
            let mut request = Out::op(INIT);
            put_config(&mut request, &config);
            request.bytes(b"k").bytes(b"i").len(0).bytes(b"fake");
            let mut served = Served::default();
            let mut opened = false;
            let error = served
                .handle(
                    &request.0,
                    &[],
                    &mut |_| {
                        opened = true;
                        Err("must not open".into())
                    },
                    &mut |_| Err("must not allocate".into()),
                )
                .unwrap_err();
            assert!(error.to_string().contains("qualification refused"));
            assert!(!opened);
            assert!(served.session.is_none());
        }
        let mut request = Out::default();
        put_config(&mut request, &config());
        request.0[1] = 255;
        assert!(
            take_config(&mut In(&request.0))
                .unwrap_err()
                .to_string()
                .contains("unknown execution qualification")
        );
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

    fn threaded(start: u8) -> (WorkerSession, FixtureThread) {
        threaded_quarantining(start, None)
    }

    fn threaded_quarantining(
        start: u8,
        quarantine_on_import: Option<Arc<LocalIndex>>,
    ) -> (WorkerSession, FixtureThread) {
        threaded_with_abort(start, quarantine_on_import, false)
    }

    fn threaded_with_abort(
        start: u8,
        quarantine_on_import: Option<Arc<LocalIndex>>,
        aborted_export: bool,
    ) -> (WorkerSession, FixtureThread) {
        let (parent, child) = UnixStream::pair().unwrap();
        let server = FixtureThread::spawn(move || {
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
                owner: Some(Box::new(server.clone())),
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
                transport: Arc::default(),
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
        let hello = first
            .transport
            .counters("t")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(hello["t.hello.receive.completed"], 1);
        let child = first
            .telemetry_counters()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(child["transport.child.loop.hello.send.completed"], 1);
        let first_before = first.loaded_image_guard.counters("test");
        let second = WorkerSession::spawn(&launcher, &[3], b"i", &config(), &[], "fake").unwrap();
        assert_eq!(first.loaded_image_guard.counters("test"), first_before);
        assert_eq!(launcher.telemetry_counters(), before);
        let second_before = second.loaded_image_guard.counters("test");
        first.state_hash().unwrap();
        assert_eq!(second.loaded_image_guard.counters("test"), second_before);
    }

    fn kill(session: &mut WorkerSession) {
        let child = session.process.as_mut().expect("test child owner");
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
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
        let sender = FixtureThread::spawn(move || {
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
            ..Served::default()
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
            ..Served::default()
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
            let server = FixtureThread::spawn(move || {
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
                    owner: Some(Box::new(server.clone())),
                    loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
                    transport: Arc::default(),
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
                owner: None,
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
                transport: Arc::default(),
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
    fn work_counter_reads_use_one_guarded_rpc_and_no_ownership_queries() {
        for retained in [1, 8, 32] {
            let (mut session, server) = threaded(3);
            let snapshots: Vec<_> = (0..retained)
                .map(|_| session.snapshot().unwrap().0)
                .collect();
            let before: BTreeMap<_, _> = session
                .transport
                .counters("transport.parent")
                .into_iter()
                .collect();
            let guards_before: BTreeMap<_, _> = session
                .loaded_image_guard
                .counters("g")
                .into_iter()
                .collect();
            let counters = session.telemetry_counters();
            assert!(counters.iter().any(|(name, _)| name == "runs"));
            let after: BTreeMap<_, _> = session
                .transport
                .counters("transport.parent")
                .into_iter()
                .collect();
            for (group, expected) in [("telemetry", 1), ("snapshot", 0)] {
                let name = format!("transport.parent.{group}.send.attempts");
                assert_eq!(
                    after.get(&name).copied().unwrap_or(0)
                        - before.get(&name).copied().unwrap_or(0),
                    expected
                );
            }
            let guards_after: BTreeMap<_, _> = session
                .loaded_image_guard
                .counters("g")
                .into_iter()
                .collect();
            for site in ["rpc_before", "rpc_after"] {
                let name = format!("g.{site}.fixture_dispatches");
                assert_eq!(
                    guards_after[&name] - guards_before.get(&name).copied().unwrap_or(0),
                    1
                );
            }
            for snapshot in snapshots {
                assert_eq!(session.snapshot_owned_pages(snapshot), Some(1));
            }
            let diagnosed: BTreeMap<_, _> = session
                .transport
                .counters("transport.parent")
                .into_iter()
                .collect();
            let key = "transport.parent.snapshot.send.attempts";
            assert_eq!(diagnosed[key] - after[key], retained);
            drop(session);
            server.join().unwrap();
        }
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
                .filter(|(name, _)| !name.starts_with("guard.")
                    && !name.starts_with("transport.")
                    && !name.starts_with("copy.")
                    && !name.starts_with("cache_import."))
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
                .filter(|(name, _)| !name.starts_with("guard.")
                    && !name.starts_with("transport.")
                    && !name.starts_with("copy.")
                    && !name.starts_with("cache_import."))
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
        assert_eq!(b["transport.parent.telemetry.send.completed"], 2);
        assert_eq!(b["transport.child.loop.telemetry.receive.completed"], 2);
        assert_eq!(b["transport.child.loop.telemetry.send.completed"], 1);
        assert_eq!(b["transport.parent.telemetry.send.header_bytes"], 32);
        assert_eq!(b["transport.parent.telemetry.send.body_bytes"], 2);
        assert_eq!(b["guard.child.telemetry_query_failures"], 0);
        assert_eq!(b["guard.child.session.fixture.dispatches"], b["runs"]);
        assert!(!b.contains_key("guard.session.fixture.dispatches"));
        assert_eq!(b["copy.child.session.fixture.bytes"], b["runs"]);
        assert!(!b.contains_key("copy.session.fixture.bytes"));
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
            let server = FixtureThread::spawn(move || {
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
                owner: Some(Box::new(server.clone())),
                retained_export: RefCell::new(Vec::new()),
                setup: (SnapId(1), 10),
                seed: 0,
                counters: RefCell::new(Vec::new()),
                exchanging: Cell::new(false),
                copies: Arc::default(),
                import_reuse: Default::default(),
                shutdown: None,
                transport: Arc::default(),
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
                let server = FixtureThread::spawn(move || {
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
            let server = FixtureThread::spawn(move || {
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
            let (writes_tx, writes_rx) = std::sync::mpsc::channel();
            let server = FixtureThread::spawn(move || {
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
                writes_tx.send(writes.len()).unwrap();
            });
            let session = connected_to_script(parent, &server);
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
                SESSION.with(|slot| {
                    assert_eq!(slot.borrow().as_ref().unwrap().retained_export_bytes(), 0)
                });
                assert_eq!(index.stats().charged, 0);
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
            server.join().unwrap();
            assert_eq!(writes_rx.recv().unwrap(), usize::from(case == 2));
        }
    }

    #[test]
    fn child_reverse_guards_refuse_before_the_next_send_receive_or_mapping() {
        for refusal in 4..=6 {
            let (parent, child) = UnixStream::pair().unwrap();
            parent
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let server = FixtureThread::spawn(move || {
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
            let server = FixtureThread::spawn(move || {
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
            let session = connected_to_script(parent, &server);
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
        let server = FixtureThread::spawn(move || {
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
        let session = connected_to_script(parent, &server);
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
        let server = FixtureThread::spawn(move || {
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
        let session = connected_to_script(parent, &server);
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
        let values = session
            .telemetry_counters()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        for owner in ["transport.parent", "transport.child.loop"] {
            for direction in ["send", "receive"] {
                assert_eq!(values[&format!("{owner}.export.{direction}.completed")], 1);
                assert_eq!(values[&format!("{owner}.setup.{direction}.completed")], 1);
            }
        }

        let error = session
            .publish_snapshot(&refused, namespace(), b"refused", None, SnapId(1), 1)
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<crate::cache::CacheError>(),
            Some(crate::cache::CacheError::Refused { .. })
        ));
        assert!(!session.abandoned());
        assert_eq!(session.state_hash().unwrap(), [3; 32]);
        let values = session
            .telemetry_counters()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        for owner in ["transport.parent", "transport.child.loop"] {
            for direction in ["send", "receive"] {
                assert_eq!(values[&format!("{owner}.export.{direction}.completed")], 3);
                assert_eq!(values[&format!("{owner}.setup.{direction}.completed")], 1);
            }
        }

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
            let server = FixtureThread::spawn(move || {
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

    fn connected_to_script(parent: UnixStream, server: &FixtureThread) -> WorkerSession {
        WorkerSession::connect(
            WorkerConnection {
                stream: parent,
                process: None,
                owner: Some(Box::new(server.clone())),
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
                transport: Arc::default(),
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
            let server = FixtureThread::spawn(move || {
                assert_eq!(recv(&child).unwrap().0[0], INIT);
                send(&child, &Out::op(OK).u64(1).u64(10).0, &[]).unwrap();
                assert_eq!(recv(&child).unwrap().0[0], EXPORT);
                if case == 7 {
                    let mut header = [0; HEADER];
                    header[..8].copy_from_slice(&((EXPORT_FRAME_LIMIT + 1) as u64).to_le_bytes());
                    (&child).write_all(&header).unwrap();
                    assert!(recv(&child).is_err());
                    return;
                }
                let file = tempfile::tempfile().unwrap();
                let mut reply = Out::op(NEED_EXTENT);
                reply.len(if case == 0 { 0 } else { 8192 });
                if case == 1 {
                    reply.u8(0);
                }
                if case == 2 {
                    reply.0 = vec![OK];
                }
                let descriptors = [file.as_fd()];
                send(&child, &reply.0, if case == 3 { &descriptors } else { &[] }).unwrap();
                if case < 4 {
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
            let session = connected_to_script(parent, &server);
            let index = LocalIndex::new(1 << 20);
            let error = session
                .publish_snapshot(&index, namespace(), b"bad", None, SnapId(1), 1)
                .unwrap_err();
            if case == 7 {
                assert!(error.to_string().contains("frame length exceeds limit"));
            }
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
    #[cfg_attr(miri, ignore)]
    #[allow(clippy::disallowed_methods)]
    fn actual_process_writer_stays_charged_and_unpunched_until_confirmed_exit() {
        let directory = tempfile::tempdir().unwrap();
        let (parent, child) = UnixStream::pair().unwrap();
        parent
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let raw = child.as_raw_fd();
        let mut command = Command::new("python3");
        command
            .arg("-c")
            .arg(
                r#"
import array,mmap,os,pathlib,socket,struct,sys,time
s=socket.socket(fileno=int(sys.argv[1])); work=pathlib.Path(sys.argv[2])
def exact(n):
    out=b''
    while len(out)<n:
        part=s.recv(n-len(out))
        if not part: raise RuntimeError('unexpected EOF')
        out+=part
    return out
def receive():
    header,ancillary,_,_=s.recvmsg(16,socket.CMSG_SPACE(256))
    header+=exact(16-len(header))
    length,count=struct.unpack('<QQ',header)
    descriptors=[]
    for level,kind,data in ancillary:
        if level==socket.SOL_SOCKET and kind==socket.SCM_RIGHTS:
            row=array.array('i');row.frombytes(data);descriptors.extend(row)
    assert len(descriptors)==count
    return exact(length),descriptors
def send(body): s.sendall(struct.pack('<QQ',len(body),0)+body)
body,_=receive();assert body[0]==1
send(struct.pack('<BQQ',0,1,10))
body,_=receive();assert body[0]==6
send(struct.pack('<BQ',7,8192))
body,fds=receive();assert body[0]==18 and len(fds)==1
start,length=struct.unpack('<QQ',body[1:]);assert length==8192
skip=start%mmap.ALLOCATIONGRANULARITY
writer=mmap.mmap(fds[0],skip+length,access=mmap.ACCESS_WRITE,offset=start-skip)
writer[skip:skip+length]=b'Z'*length
s.close()
deadline=time.monotonic()+10
while not (work/'write').exists() and time.monotonic()<deadline:time.sleep(.005)
if writer[skip:skip+length]!=b'Z'*length:(work/'punched').write_text('unexpected reclamation')
writer[skip:skip+length]=b'\xab'*length
writer.flush();(work/'written').write_text('done')
while not (work/'exit').exists() and time.monotonic()<deadline:time.sleep(.005)
writer.close()
"#,
            )
            .arg(raw.to_string())
            .arg(directory.path());
        // SAFETY: child owns raw until spawn completes; the post-fork closure only invokes
        // async-signal-safe fcntl on that inherited descriptor.
        unsafe {
            command.pre_exec(move || set_cloexec(raw, false));
        }
        let process = command.spawn().unwrap();
        drop(child);
        let mut session = WorkerSession::connect(
            WorkerConnection {
                transport: Arc::default(),
                stream: parent,
                process: Some(process),
                owner: None,
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
            },
            &[3],
            b"i",
            &config(),
            &[],
            "fake",
        )
        .unwrap();
        let index = LocalIndex::new(3 * 4096);
        let mut keeper = index.extent(4096).unwrap();
        keeper.bytes_mut().fill(0xcd);
        assert!(
            session
                .publish_snapshot(&index, namespace(), b"uncertain", None, SnapId(1), 1)
                .is_err()
        );
        assert!(session.abandoned());
        assert_eq!(session.retained_export_bytes(), 8192);
        assert_eq!(index.stats().charged, 3 * 4096);
        assert!(index.extent(1).is_err());
        let id = crate::writer_lifetime::retain_worker(
            session
                .process
                .take()
                .map(|child| Box::new(child) as Box<dyn crate::writer_lifetime::ExitOwner>),
            std::mem::take(session.retained_export.get_mut()),
        );
        drop(session);
        assert_eq!(
            crate::writer_lifetime::poll_worker(&id, false),
            crate::writer_lifetime::ExitPoll::Running
        );
        assert_eq!(
            crate::writer_lifetime::retained_worker(&id),
            Some((1, 8192, false))
        );
        crate::writer_lifetime::reap_worker_quarantine(64);
        assert_eq!(
            crate::writer_lifetime::retained_worker(&id),
            Some((1, 8192, false))
        );
        assert_eq!(index.stats().charged, 3 * 4096);
        std::fs::write(directory.path().join("write"), b"go").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !directory.path().join("written").exists() {
            assert!(Instant::now() < deadline, "writer did not reach barrier");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!directory.path().join("punched").exists());
        assert!(keeper.bytes_mut().iter().all(|byte| *byte == 0xcd));
        assert_eq!(index.stats().charged, 3 * 4096);
        std::fs::write(directory.path().join("exit"), b"go").unwrap();
        while crate::writer_lifetime::retained_worker(&id).is_some() {
            assert!(Instant::now() < deadline, "worker exit was not confirmed");
            crate::writer_lifetime::reap_worker_quarantine(64);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            crate::writer_lifetime::poll_worker(&id, false),
            crate::writer_lifetime::ExitPoll::Exited
        );
        if crate::cache::segments::RELEASES_EXTENTS {
            assert_eq!(index.stats().charged, 4096);
        }
        drop(keeper);
        assert_eq!(index.stats().charged, 0);
        assert!(index.extent(8192).is_ok());
        assert_eq!(index.stats().publishes, 0);
    }

    #[derive(Debug)]
    struct DropOwner {
        polls: Arc<std::sync::atomic::AtomicUsize>,
        signals: Arc<std::sync::atomic::AtomicUsize>,
        pauses: Arc<std::sync::atomic::AtomicUsize>,
        signal_pause: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl crate::writer_lifetime::ExitOwner for DropOwner {
        fn try_exited(&mut self) -> io::Result<bool> {
            use std::sync::atomic::Ordering;
            self.polls.fetch_add(1, Ordering::SeqCst);
            Ok(self.signals.load(Ordering::SeqCst) != 0)
        }
        fn terminate(&mut self) -> io::Result<()> {
            use std::sync::atomic::Ordering;
            self.signal_pause
                .store(self.pauses.load(Ordering::SeqCst), Ordering::SeqCst);
            self.signals.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn explicit_shutdown_confirms_an_owned_protocol_child_and_is_idempotent() {
        use std::os::unix::process::CommandExt;
        let (parent, child) = UnixStream::pair().unwrap();
        let raw = child.as_raw_fd();
        let mut command = std::process::Command::new("python3");
        command
            .arg("-c")
            .arg(
                r#"
import os,socket,struct,sys
s=socket.socket(fileno=int(sys.argv[1]))
def exact(n):
    result=b''
    while len(result)<n:
        part=s.recv(n-len(result))
        if not part: raise RuntimeError('early EOF')
        result+=part
    return result
length,count=struct.unpack('<QQ',exact(16))
assert count==0 and exact(length)[0]==1
body=struct.pack('<BQQ',0,1,10)
s.sendall(struct.pack('<QQ',len(body),0)+body)
while s.recv(4096): pass
"#,
            )
            .arg(raw.to_string());
        // SAFETY: the child endpoint remains owned through spawn; this post-fork closure only changes its inherited descriptor's CLOEXEC flag with fcntl.
        unsafe {
            command.pre_exec(move || set_cloexec(raw, false));
        }
        let process = command.spawn().unwrap();
        drop(child);
        let mut session = WorkerSession::connect(
            WorkerConnection {
                stream: parent,
                process: Some(process),
                owner: None,
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
                transport: Arc::default(),
            },
            &[3],
            b"i",
            &config(),
            &[],
            "fake",
        )
        .unwrap();
        session.shutdown().unwrap();
        assert!(session.process.is_none());
        assert_eq!(session.shutdown, Some(Ok(())));
        session.shutdown().unwrap();
        assert!(session.call(&Out::op(STORE_BYTES), &[]).is_err());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[allow(clippy::disallowed_methods)]
    fn constructor_admission_refusal_shuts_down_owned_child_without_sending_init() {
        use std::os::unix::process::CommandExt;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        #[derive(Debug)]
        struct ObservedChild {
            child: std::process::Child,
            confirmed: Arc<AtomicBool>,
            fallback: Arc<AtomicBool>,
            polls: Arc<AtomicUsize>,
        }
        impl crate::writer_lifetime::ExitOwner for ObservedChild {
            fn try_exited(&mut self) -> io::Result<bool> {
                self.polls.fetch_add(1, Ordering::SeqCst);
                let exited = self.child.try_wait()?.is_some();
                self.confirmed.store(exited, Ordering::SeqCst);
                Ok(exited)
            }
            fn terminate(&mut self) -> io::Result<()> {
                self.child.kill()
            }
        }
        impl Drop for ObservedChild {
            fn drop(&mut self) {
                if self.polls.load(Ordering::SeqCst) == 0 {
                    self.fallback.store(true, Ordering::SeqCst);
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                }
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("ready");
        let received = directory.path().join("received");
        let (parent, child) = UnixStream::pair().unwrap();
        let raw = child.as_raw_fd();
        let mut command = std::process::Command::new("python3");
        command
            .arg("-c")
            .arg(
                r#"
import pathlib,socket,sys
s=socket.socket(fileno=int(sys.argv[1]))
pathlib.Path(sys.argv[2]).write_text('ready')
pathlib.Path(sys.argv[3]).write_bytes(s.recv(1))
"#,
            )
            .arg(raw.to_string())
            .arg(&ready)
            .arg(&received);
        // SAFETY: the endpoint remains owned through spawn; the post-fork closure only changes CLOEXEC on its inherited descriptor with fcntl.
        unsafe {
            command.pre_exec(move || set_cloexec(raw, false));
        }
        let confirmed = Arc::new(AtomicBool::new(false));
        let fallback = Arc::new(AtomicBool::new(false));
        let polls = Arc::new(AtomicUsize::new(0));
        let owner = ObservedChild {
            child: command.spawn().unwrap(),
            confirmed: confirmed.clone(),
            fallback: fallback.clone(),
            polls: polls.clone(),
        };
        drop(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if !ready.exists() {
            drop(owner);
            panic!("protocol child did not reach its receive barrier");
        }
        let (result, checks, strict) = crate::image_identity::trace_checks(Some(1), || {
            WorkerSession::connect(
                WorkerConnection {
                    stream: parent,
                    process: None,
                    owner: Some(Box::new(owner)),
                    loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
                    transport: Arc::default(),
                },
                &[3],
                b"i",
                &config(),
                &[],
                "fake",
            )
        });
        let error = result.unwrap_err();
        assert!(error.to_string().contains("injected loaded-image refusal"));
        assert_eq!((checks, strict), (1, 0));
        assert!(
            confirmed.load(Ordering::SeqCst),
            "shutdown did not confirm its owned child exit"
        );
        assert!(!fallback.load(Ordering::SeqCst));
        assert!(polls.load(Ordering::SeqCst) > 0);
        assert_eq!(std::fs::read(received).unwrap(), b"");
    }

    #[test]
    fn explicit_shutdown_retains_ambiguous_owner_and_never_repolls_on_retry() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        #[derive(Debug)]
        struct Ambiguous(Arc<AtomicUsize>);
        impl crate::writer_lifetime::ExitOwner for Ambiguous {
            fn try_exited(&mut self) -> io::Result<bool> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(io::Error::other("injected ambiguous wait"))
            }
            fn terminate(&mut self) -> io::Result<()> {
                panic!("ambiguous owner signalled")
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let (mut session, server) = threaded(3);
        session.owner = Some(Box::new(Ambiguous(calls.clone())));
        let index = LocalIndex::new(4096);
        session
            .retained_export
            .get_mut()
            .push(index.extent(4096).unwrap());
        let first = session.shutdown().unwrap_err().to_string();
        assert!(first.contains("ambiguous"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(index.stats().charged, 4096);
        assert_eq!(session.shutdown().unwrap_err().to_string(), first);
        drop(session);
        crate::writer_lifetime::reap_worker_quarantine(64);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(index.stats().charged, 4096);
        server.join().unwrap();
    }

    #[test]
    fn actual_drop_preserves_grace_before_enabling_one_signal_and_confirming_exit() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (parent, child) = UnixStream::pair().unwrap();
        drop(child);
        let polls = Arc::new(AtomicUsize::new(0));
        let signals = Arc::new(AtomicUsize::new(0));
        let pauses = Arc::new(AtomicUsize::new(0));
        let signal_pause = Arc::new(AtomicUsize::new(usize::MAX));
        let index = LocalIndex::new(4096);
        let session = WorkerSession {
            stream: parent,
            process: None,
            owner: Some(Box::new(DropOwner {
                polls: polls.clone(),
                signals: signals.clone(),
                pauses: pauses.clone(),
                signal_pause: signal_pause.clone(),
            })),
            loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
            setup: (SnapId(0), 0),
            seed: 0,
            failed: Cell::new(false),
            counters: RefCell::new(Vec::new()),
            telemetry_queries: Cell::new(0),
            telemetry_query_failures: Cell::new(0),
            exchanging: Cell::new(false),
            retained_export: RefCell::new(vec![index.extent(4096).unwrap()]),
            copies: Arc::default(),
            import_reuse: Default::default(),
            shutdown: None,
            transport: Arc::default(),
        };
        let (notice_tx, notice_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let dropper = {
            let pauses = pauses.clone();
            std::thread::spawn(move || {
                EXIT_PAUSE_HOOK.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move |id| {
                        let pause = pauses.fetch_add(1, Ordering::SeqCst) + 1;
                        if pause == 1 || pause == EXIT_POLLS as usize {
                            let _ = notice_tx.send((pause, id.clone()));
                            let _ = release_rx.recv_timeout(Duration::from_secs(10));
                        }
                    }))
                });
                drop(session);
                EXIT_PAUSE_HOOK.with(|hook| hook.borrow_mut().take());
            })
        };
        let mut before = Vec::new();
        let mut retained_id = None;
        for _ in 0..2 {
            let Ok((pause, id)) = notice_rx.recv_timeout(Duration::from_secs(10)) else {
                break;
            };
            let prior_polls = polls.load(Ordering::SeqCst);
            for _ in 0..4096 {
                crate::writer_lifetime::reap_worker_quarantine(64);
                if polls.load(Ordering::SeqCst) > prior_polls {
                    break;
                }
            }
            before.push((
                pause,
                polls.load(Ordering::SeqCst) > prior_polls,
                signals.load(Ordering::SeqCst),
                crate::writer_lifetime::retained_worker(&id),
                index.stats().charged,
            ));
            retained_id = Some(id);
            let _ = release_tx.send(());
        }
        let _ = release_tx.send(());
        let joined = dropper.join();
        let after = retained_id
            .as_ref()
            .map(crate::writer_lifetime::retained_worker);
        let final_pauses = pauses.load(Ordering::SeqCst);
        let final_signals = signals.load(Ordering::SeqCst);
        let at_signal = signal_pause.load(Ordering::SeqCst);
        if let Some(id) = &retained_id {
            crate::writer_lifetime::allow_worker_signal(id);
            let _ = crate::writer_lifetime::poll_worker(id, true);
            let _ = crate::writer_lifetime::poll_worker(id, false);
        }
        assert!(joined.is_ok());
        assert_eq!(EXIT_POLLS, 400);
        assert_eq!(EXIT_POLL, Duration::from_millis(5));
        assert_eq!(
            before,
            vec![
                (1, true, 0, Some((1, 4096, false)), 4096),
                (400, true, 0, Some((1, 4096, false)), 4096),
            ]
        );
        assert_eq!(final_pauses, EXIT_POLLS as usize);
        assert_eq!(final_signals, 1);
        assert_eq!(at_signal, EXIT_POLLS as usize);
        assert_eq!(after, Some(None));
        assert_eq!(index.stats().charged, 0);
    }

    #[test]
    fn failed_send_after_fd_handoff_retains_destination_until_join_proof() {
        let (parent, child) = UnixStream::pair().unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = FixtureThread::spawn(move || {
            recv(&child).unwrap();
            send(&child, &Out::op(OK).u64(1).u64(10).0, &[]).unwrap();
            assert_eq!(recv(&child).unwrap().0[0], EXPORT);
            send(&child, &Out::op(NEED_EXTENT).len(8192).0, &[]).unwrap();
            let mut header = [0; HEADER];
            let (received, fds) =
                recv_with_fds(&child, &mut header, None, &mut work::Delta::default()).unwrap();
            (&child).read_exact(&mut header[received..]).unwrap();
            assert_eq!(fds.len(), 1);
            let mut writer = Mapping::new(fds[0].as_fd(), 0, 8192, true).unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            writer.bytes_mut().fill(0x5a);
        });
        let session = connected_to_script(parent, &server);
        let index = LocalIndex::new(8192);
        FAIL_AFTER_FD_HEADER.with(|fail| fail.set(true));
        let error = session
            .publish_snapshot(&index, namespace(), b"partial", None, SnapId(1), 1)
            .unwrap_err();
        assert!(error.to_string().contains("after descriptor transfer"));
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(session.retained_export_bytes(), 8192);
        assert_eq!(index.stats().charged, 8192);
        assert!(index.extent(1).is_err());
        assert_eq!(index.stats().publishes, 0);
        release_tx.send(()).unwrap();
        server.join().unwrap();
        drop(session);
        assert_eq!(index.stats().charged, 0);
    }

    #[test]
    fn lost_final_ack_never_publishes_or_reuses_a_still_mapped_destination() {
        use std::os::unix::fs::MetadataExt;
        let (parent, child) = UnixStream::pair().unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = FixtureThread::spawn(move || {
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
        let session = connected_to_script(parent, &server);
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
    fn accepted_worker_imports_report_references_backings_and_session_recurrence() {
        for (segment_pages, unique) in [(8, 1), (4, 2), (2, 4)] {
            let index = LocalIndex::with_segment_bytes(1 << 20, segment_pages * 4096);
            let (mut source, source_server) = threaded(3);
            let (mut destination, destination_server) = threaded(50);
            let (mut other, other_server) = threaded(70);
            let space = Namespace::exact(&[b"import-layout"]);
            let mut snapshots = Vec::new();
            let mut leases = Vec::new();
            for step in 0..4u8 {
                source.run_until(40 + u64::from(step)).unwrap();
                let (snapshot, _) = source.snapshot().unwrap();
                let parent = snapshots
                    .last()
                    .zip(leases.last())
                    .map(|(snap, lease)| (*snap, lease));
                let lease = source
                    .publish_snapshot(&index, space, &[step], parent, snapshot, 1)
                    .unwrap();
                snapshots.push(snapshot);
                leases.push(lease);
            }
            let lease = leases.last().unwrap();
            let chain = index.chain(lease).unwrap();
            assert_eq!(chain.len(), 4);
            assert_eq!(
                crate::import_layout::ImportLayout::from_extents(&chain)
                    .unwrap()
                    .backing_rows
                    .len(),
                unique
            );
            destination.import_cached(&index, lease, SnapId(1)).unwrap();
            destination.import_cached(&index, lease, SnapId(1)).unwrap();
            let telemetry: BTreeMap<_, _> = destination.telemetry_counters().into_iter().collect();
            for (name, value) in [
                ("accepted_imports", 2),
                ("measured_imports", 2),
                ("extent_references", 8),
                ("backing_occurrences", 2 * unique as u64),
                ("duplicate_references", 8 - 2 * unique as u64),
                ("recent_backing_hits", unique as u64),
                ("recent_backing_misses", unique as u64),
                ("unmeasured_overlimit", 0),
            ] {
                assert_eq!(telemetry[&format!("cache_import.parent.{name}")], value);
            }
            assert_eq!(
                telemetry["transport.parent.import.send.fd_handles"],
                2 * unique as u64
            );
            assert_eq!(
                telemetry["transport.child.loop.import.receive.fd_handles"],
                2 * unique as u64
            );
            assert_eq!(telemetry["copy.child.loop.import_mapping.attempts"], 8);
            assert_eq!(
                telemetry["transport.parent.import.send.body_bytes"],
                2 * (1 + 24 + 24 * 4)
            );
            assert_eq!(
                telemetry["transport.child.loop.import.receive.body_bytes"],
                2 * (1 + 24 + 24 * 4)
            );
            assert_eq!(
                destination.state_hash().unwrap(),
                source.state_hash().unwrap()
            );
            other.import_cached(&index, lease, SnapId(1)).unwrap();
            let fresh: BTreeMap<_, _> = other.telemetry_counters().into_iter().collect();
            assert_eq!(fresh["cache_import.parent.recent_backing_hits"], 0);
            assert_eq!(
                fresh["cache_import.parent.recent_backing_misses"],
                unique as u64
            );
            for lease in leases {
                index.release(lease);
            }
            drop((source, destination, other));
            source_server.join().unwrap();
            destination_server.join().unwrap();
            other_server.join().unwrap();
        }
    }

    #[test]
    fn import_descriptor_table_refuses_malformed_rows_before_mapping_or_mutation() {
        let (mut session, server) = threaded(50);
        let file = tempfile::tempfile().unwrap();
        file.set_len(4096).unwrap();
        let before = session.state_hash().unwrap();
        let cases = [
            (1, vec![[0, 0, 1]], 0, 0),
            (1, vec![[0, 0, 1]], 2, 0),
            (1, vec![[1, 0, 1]], 1, 0),
            (2, vec![[1, 0, 1], [0, 0, 1]], 2, 0),
            (2, vec![[1, 0, 1], [0, 0, 1], [1, 1, 1]], 2, 0),
            (2, vec![[0, 0, 1], [0, 1, 1]], 2, 0),
            (1, vec![[0, u64::MAX, 1]], 1, 0),
            (1, vec![[0, 0, 0]], 1, 0),
            (1, vec![[0, 4096, 4096]], 1, 1),
        ];
        for (unique, rows, descriptors, attempts) in cases {
            let prior: BTreeMap<_, _> = session.telemetry_counters().into_iter().collect();
            let mut request = Out::op(IMPORT);
            request.u64(1).u64(unique).len(rows.len());
            for row in rows {
                for value in row {
                    request.u64(value);
                }
            }
            let fds = vec![file.as_fd(); descriptors];
            let error = session.call(&request, &fds).unwrap_err();
            assert!(matches!(
                error.downcast_ref::<SessionError>(),
                Some(SessionError::Control(_))
            ));
            assert_eq!(session.state_hash().unwrap(), before);
            assert_eq!(file.metadata().unwrap().len(), 4096);
            let after: BTreeMap<_, _> = session.telemetry_counters().into_iter().collect();
            assert_eq!(
                after["copy.child.loop.import_mapping.attempts"]
                    - prior["copy.child.loop.import_mapping.attempts"],
                attempts
            );
        }
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn descriptor_table_import_requires_the_new_host_hello_version() {
        assert_eq!(IMAGE_HELLO, b"harmony-session-worker-image-v3");
        for version in [
            b"harmony-session-worker-image-v1".as_slice(),
            b"harmony-session-worker-image-v2".as_slice(),
        ] {
            let mut hello = Out::default();
            hello.bytes(version).bytes(b"fixture");
            assert!(
                check_worker_image_hello(&hello.0, "fixture")
                    .unwrap_err()
                    .to_string()
                    .contains("unknown version")
            );
        }
        let mut hello = Out::default();
        hello.bytes(IMAGE_HELLO).bytes(b"fixture");
        check_worker_image_hello(&hello.0, "fixture").unwrap();
        assert!(check_worker_image_hello(&hello.0, "different").is_err());
        hello.0.push(0);
        assert!(check_worker_image_hello(&hello.0, "fixture").is_err());
    }

    #[test]
    fn refused_import_guards_do_not_record_accepted_backings() {
        for refusal in [1, 2] {
            let index = LocalIndex::new(1 << 20);
            let (mut source, source_server) = threaded(3);
            let (mut destination, destination_server) = threaded(50);
            let (snapshot, _) = source.snapshot().unwrap();
            let lease = source
                .publish_snapshot(&index, namespace(), b"guarded", None, snapshot, 1)
                .unwrap();
            let (result, checks, strict) =
                crate::image_identity::trace_checks(Some(refusal), || {
                    destination.import_cached(&index, &lease, SnapId(1))
                });
            assert!(result.is_err());
            assert_eq!((checks, strict), (refusal, 0));
            assert!(
                destination
                    .import_reuse
                    .counters()
                    .iter()
                    .all(|(_, value)| *value == 0)
            );
            assert!(destination.failed.get());
            index.release(lease);
            drop((source, destination));
            source_server.join().unwrap();
            destination_server.join().unwrap();
        }
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
    fn cache_import_survives_confirmed_publisher_session_shutdown() {
        let index = LocalIndex::new(1 << 20);
        let (mut publisher, publisher_server) = threaded(3);
        let (mut importer, importer_server) = threaded(50);
        publisher.run_until(40).unwrap();
        let expected = publisher.state_hash().unwrap();
        let (snapshot, _) = publisher.snapshot().unwrap();
        let lease = publisher
            .publish_snapshot(&index, namespace(), b"closed-publisher", None, snapshot, 1)
            .unwrap();
        publisher.shutdown().unwrap();
        publisher_server.join().unwrap();
        drop(publisher);
        importer.import_cached(&index, &lease, SnapId(1)).unwrap();
        assert_eq!(importer.state_hash().unwrap(), expected);
        index.release(lease);
        importer.shutdown().unwrap();
        drop(importer);
        importer_server.join().unwrap();
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
        let layout_before = destination.import_reuse.counters();
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
        assert_eq!(destination.import_reuse.counters(), layout_before);
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
        assert!(
            second
                .import_reuse
                .counters()
                .iter()
                .all(|(_, value)| *value == 0)
        );
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
                owner: None,
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
                transport: Arc::default(),
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
        let server = FixtureThread::spawn(move || {
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
                owner: Some(Box::new(server.clone())),
                loaded_image_guard: crate::image_identity::LoadedImageGuard::fixture(),
                transport: Arc::default(),
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
        kill(&mut owner);
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
        kill(&mut unpublished);
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
