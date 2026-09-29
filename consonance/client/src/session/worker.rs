// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    error::Error,
    ffi::OsString,
    io::{self, Read, Write},
    net::Shutdown,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    ptr::NonNull,
    time::Duration,
};

use control_proto::{Reply, SnapId, StopReason, decode_reply, encode_reply};
use environment::{
    channel::Effect,
    input_spec::{InputSpec, ServiceConfig, ServiceFactory},
};

use super::{SdkEvent, Session, SessionConfig, SessionError, service_branch_spec};
use crate::cache::segments::SharedRange;
use crate::cache::{ANCHOR_DEPTH, CacheIndex, CommittedExtent, Lease, Namespace};

pub const WORKER_FD_ENV: &str = "HARMONY_SESSION_WORKER_FD";
const HEADER: usize = 16;
const MAX_FDS: usize = 2 * ANCHOR_DEPTH as usize;
const EXIT_POLL: Duration = Duration::from_millis(5);
const EXIT_POLLS: u32 = 400;

const INIT: u8 = 1;
const STATE_HASH: u8 = 2;
const CONSOLE_TAIL: u8 = 3;
const TELEMETRY: u8 = 4;
const OWNED_PAGES: u8 = 5;
const EXPORT: u8 = 6;
const WRITE_EXTENT: u8 = 7;
const IMPORT: u8 = 8;
const SNAPSHOT: u8 = 9;
const DROP_SNAPSHOT: u8 = 10;
const REPLAY: u8 = 11;
const BRANCH: u8 = 12;
const RUN_UNTIL: u8 = 13;
const SDK_EVENTS: u8 = 14;
const STORE_BYTES: u8 = 15;

const OK: u8 = 0;
const FAILED: u8 = 1;
const OTHER: u8 = 0;
const HUNG: u8 = 1;
const ABANDONED: u8 = 2;

pub trait SearchSession: std::fmt::Debug {
    fn setup_handle(&self) -> (SnapId, u64);
    fn state_hash(&mut self) -> Result<[u8; 32], Box<dyn Error>>;
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
    fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>>;
    fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>>;
    fn branch_with_service(
        &mut self,
        snapshot: SnapId,
        config: ServiceConfig,
        payloads: Vec<Vec<u8>>,
        effects: Vec<(u64, Effect)>,
    ) -> Result<(), Box<dyn Error>>;
    fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>>;
    fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>>;
    fn sdk_events(&mut self) -> Result<Vec<SdkEvent>, Box<dyn Error>>;
    fn abandoned(&self) -> bool;
}

trait ServedSession: SearchSession {
    fn export_delta(
        &self,
        parent: Option<SnapId>,
        target: SnapId,
    ) -> Result<Vec<u8>, Box<dyn Error>>;
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
    ) -> Result<(), Box<dyn Error>> {
        Session::branch_with_service(self, snapshot, config, payloads, effects)
    }

    fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
        Session::run_until(self, deadline)
    }

    fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
        Session::snapshot(self)
    }

    fn sdk_events(&mut self) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
        Session::sdk_events(self)
    }

    fn abandoned(&self) -> bool {
        Session::abandoned(self)
    }
}

impl ServedSession for Session {
    fn export_delta(
        &self,
        parent: Option<SnapId>,
        target: SnapId,
    ) -> Result<Vec<u8>, Box<dyn Error>> {
        self.write_delta(parent, target, |len| Ok(vec![0; len]), Vec::as_mut_slice)
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

fn recv_with_fds(stream: &UnixStream, buffer: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
    let space = control_space(MAX_FDS);
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
    // SAFETY: recvmsg filled msg_control with msg_controllen bytes of well-formed control
    // headers; CMSG_FIRSTHDR and CMSG_NXTHDR stay within that range, and each SCM_RIGHTS
    // payload holds descriptors the kernel just installed in this process for us to own.
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&raw const message);
        while !header.is_null() {
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(header);
                let bytes = (*header).cmsg_len as usize - data.offset_from(header.cast()) as usize;
                for index in 0..bytes / size_of::<RawFd>() {
                    let raw = std::ptr::read_unaligned(data.cast::<RawFd>().add(index));
                    fds.push(OwnedFd::from_raw_fd(raw));
                }
            }
            header = libc::CMSG_NXTHDR(&raw const message, header);
        }
    }
    if message.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(malformed("descriptors were truncated"));
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

fn recv(stream: &UnixStream) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut header = [0_u8; HEADER];
    let (received, fds) = recv_with_fds(stream, &mut header)?;
    if received == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let mut reader = stream;
    reader.read_exact(&mut header[received..])?;
    let len = usize::try_from(u64::from_le_bytes(
        header[..8].try_into().expect("eight bytes"),
    ))
    .map_err(|_| malformed("length overflows"))?;
    if u64::from_le_bytes(header[8..].try_into().expect("eight bytes")) != fds.len() as u64 {
        return Err(malformed("descriptor count"));
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body)?;
    Ok((body, fds))
}

struct Mapping {
    base: NonNull<u8>,
    skip: usize,
    len: usize,
}

impl Mapping {
    fn new(fd: BorrowedFd<'_>, offset: usize, len: usize, writable: bool) -> io::Result<Self> {
        if len == 0 {
            return Err(malformed("empty extent"));
        }
        // SAFETY: sysconf reads a constant system parameter.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .map_err(|_| io::Error::last_os_error())?;
        let skip = offset % page;
        let start = libc::off_t::try_from(offset - skip).map_err(|_| malformed("offset"))?;
        let protection = if writable {
            libc::PROT_READ | libc::PROT_WRITE
        } else {
            libc::PROT_READ
        };
        // SAFETY: a fresh shared mapping of a descriptor the caller holds open aliases no Rust
        // object; the kernel rejects a range beyond the file.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                skip + len,
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
}

impl WorkerLauncher {
    #[must_use]
    pub fn new(program: PathBuf, args: Vec<OsString>) -> Self {
        Self { program, args }
    }

    pub fn current_exe(args: Vec<OsString>) -> io::Result<Self> {
        Ok(Self::new(std::env::current_exe()?, args))
    }

    fn spawn(&self) -> io::Result<(Child, UnixStream)> {
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
        let process = command.spawn()?;
        drop(child);
        Ok((process, parent))
    }
}

fn put_config(out: &mut Out, config: &SessionConfig) {
    out.len(config.ram_bytes)
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
        ram_bytes: input.len()?,
        seed: input.u64()?,
        run_budget: input.u64()?,
        cmdline: input.string()?,
        identity_tag: input.string()?,
        wall_limit: input.option()?.map(Duration::from_nanos),
        defer_virtual_time_checkpoint_hashes: input.u8()? != 0,
    })
}

#[derive(Debug)]
pub struct WorkerSession {
    stream: UnixStream,
    process: Option<Child>,
    setup: (SnapId, u64),
    seed: u64,
    failed: Cell<bool>,
    counters: RefCell<Vec<(String, u64)>>,
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
        let (process, stream) = launcher.spawn().map_err(|error| {
            SessionError::Control(format!(
                "start session worker {}: {error}",
                launcher.program.display()
            ))
        })?;
        Self::connect(
            stream,
            Some(process),
            kernel,
            initramfs,
            config,
            setup_payloads,
            service,
        )
    }

    fn connect(
        stream: UnixStream,
        process: Option<Child>,
        kernel: &[u8],
        initramfs: &[u8],
        config: &SessionConfig,
        setup_payloads: &[Vec<u8>],
        service: &str,
    ) -> Result<Self, Box<dyn Error>> {
        let mut session = Self {
            stream,
            process,
            setup: (SnapId(0), 0),
            seed: config.seed,
            failed: Cell::new(false),
            counters: RefCell::default(),
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
        Ok(session)
    }

    #[must_use]
    pub fn process_id(&self) -> Option<u32> {
        self.process.as_ref().map(Child::id)
    }

    fn call(&self, request: &Out, fds: &[BorrowedFd<'_>]) -> Result<Vec<u8>, Box<dyn Error>> {
        if self.failed.get() {
            return Err(SessionError::Abandoned.into());
        }
        let reply = send(&self.stream, &request.0, fds).and_then(|()| recv(&self.stream));
        let body = match reply {
            Ok((body, _)) => body,
            Err(error) => {
                self.failed.set(true);
                return Err(SessionError::Control(format!("session worker: {error}")).into());
            }
        };
        let mut input = In(&body);
        match input.u8()? {
            OK => Ok(input.0.to_vec()),
            FAILED => {
                let kind = input.u8()?;
                let error = match kind {
                    HUNG => SessionError::Hung(Duration::from_nanos(input.u64()?)),
                    ABANDONED => SessionError::Abandoned,
                    _ => SessionError::Control(input.string()?),
                };
                if kind != OTHER {
                    self.failed.set(true);
                }
                Err(error.into())
            }
            _ => Err(malformed("reply status").into()),
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
        if let Ok(Ok(counters)) = self.call(&Out::op(TELEMETRY), &[]).map(decode) {
            self.counters.replace(counters);
        }
        self.counters.borrow().clone()
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
        let parent = parent.filter(|(_, lease)| lease.depth() + 1 < ANCHOR_DEPTH);
        let reply = self.call(
            Out::op(EXPORT)
                .option(parent.map(|(snap, _)| snap.0))
                .u64(target.0),
            &[],
        )?;
        let mut input = In(&reply);
        let len = input.len()?;
        input.finish()?;
        let extent = index.extent(len)?;
        let SharedRange { fd, offset, len } = extent.shared_range();
        let reply = self.call(Out::op(WRITE_EXTENT).len(offset).len(len), &[fd])?;
        In(&reply).finish()?;
        Ok(index.publish(namespace, key, parent.map(|(_, lease)| lease), extent, cost)?)
    }

    fn import_cached(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
        near: SnapId,
    ) -> Result<(SnapId, u64), Box<dyn Error>> {
        let chain = index.chain(lease)?;
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
        Ok(snap)
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
    ) -> Result<(), Box<dyn Error>> {
        let spec = service_branch_spec(self.seed, config, payloads, effects)?;
        self.call_unit(Out::op(BRANCH).u64(snapshot.0).bytes(&spec.encode()))
    }

    fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
        let reply = self.call(Out::op(RUN_UNTIL).u64(deadline), &[])?;
        match decode_reply(&reply) {
            Ok(Some((_, Ok(Reply::Stop(stop)), consumed))) if consumed == reply.len() => Ok(stop),
            _ => Err(malformed("stop reason").into()),
        }
    }

    fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>> {
        self.call_snap(&Out::op(SNAPSHOT))
    }

    fn sdk_events(&mut self) -> Result<Vec<SdkEvent>, Box<dyn Error>> {
        let reply = self.call(&Out::op(SDK_EVENTS), &[])?;
        let mut input = In(&reply);
        let events = (0..input.len()?)
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
    session: Option<Box<dyn ServedSession>>,
    pending: Option<Vec<u8>>,
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
    ) -> Result<Out, Box<dyn Error>> {
        let mut input = In(body);
        let op = input.u8()?;
        let mut out = Out::default();
        match op {
            INIT => {
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
            WRITE_EXTENT => {
                let offset = input.len()?;
                let len = input.len()?;
                input.finish()?;
                let [fd] = fds else {
                    return Err(malformed("an extent write carries one descriptor").into());
                };
                let pending = self.pending.take().ok_or("no exported delta is pending")?;
                if pending.len() > len {
                    return Err(malformed("extent is smaller than its delta").into());
                }
                let mut mapping = Mapping::new(fd.as_fd(), offset, len, true)?;
                mapping.bytes_mut()[..pending.len()].copy_from_slice(&pending);
                return Ok(out);
            }
            _ => {}
        }
        let session = self.session()?;
        match op {
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
                let counters = session.telemetry_counters();
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
                input.finish()?;
                let delta = session.export_delta(parent, target)?;
                out.len(delta.len());
                self.pending = Some(delta);
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
                let spec = InputSpec::decode(input.bytes()?)
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
            SDK_EVENTS => {
                input.finish()?;
                let events = session.sdk_events()?;
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
                _ => {
                    out.u8(OTHER).bytes(error.to_string().as_bytes());
                }
            }
            out.0
        }
    }
}

fn serve(stream: &UnixStream, open: &mut Open<'_>) -> Result<(), Box<dyn Error>> {
    let mut served = Served::default();
    loop {
        let (body, fds) = match recv(stream) {
            Ok(frame) => frame,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let reply = status(served.handle(&body, &fds, open));
        drop(fds);
        send(stream, &reply, &[])?;
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
    serve(&inherited_stream()?, &mut |init: Init| {
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

    #[derive(Debug, Default)]
    struct Fake {
        next: u64,
        states: BTreeMap<u64, u8>,
        current: u8,
        runs: u64,
    }

    impl Fake {
        fn open(init: Init) -> Result<Box<dyn ServedSession>, Box<dyn Error>> {
            if init.service != "fake" {
                return Err(format!("unknown session service {}", init.service).into());
            }
            let mut fake = Self {
                next: 1,
                current: init.kernel[0],
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

        fn console_tail(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
            Ok(b"tail".to_vec())
        }

        fn telemetry_counters(&self) -> Vec<(String, u64)> {
            vec![("runs".to_owned(), self.runs)]
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
        ) -> Result<(), Box<dyn Error>> {
            Err("a served session branches from an input".into())
        }

        fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>> {
            if deadline == HANG {
                return Err(SessionError::Hung(Duration::from_secs(3)).into());
            }
            self.current = self.current.wrapping_add(1);
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
            Ok(vec![(5, 7, vec![self.current])])
        }

        fn abandoned(&self) -> bool {
            false
        }
    }

    impl ServedSession for Fake {
        fn export_delta(
            &self,
            _: Option<SnapId>,
            target: SnapId,
        ) -> Result<Vec<u8>, Box<dyn Error>> {
            let fill = self.state(target)?;
            let mut out = vec![0; extent_len(1, 0, 1).unwrap()];
            write_extent(&mut out, &[(0, &[fill; 32], &[fill; 4096])], &[], &[fill])?;
            Ok(out)
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
            self.snapshot()
        }

        fn branch_input(&mut self, snapshot: SnapId, _: &InputSpec) -> Result<(), Box<dyn Error>> {
            self.current = self.state(snapshot)?;
            Ok(())
        }
    }

    fn config() -> SessionConfig {
        SessionConfig {
            wall_limit: Some(Duration::from_secs(9)),
            identity_tag: "worker-test".to_owned(),
            ..SessionConfig::default()
        }
    }

    fn threaded(start: u8) -> (WorkerSession, std::thread::JoinHandle<()>) {
        let (parent, child) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || serve(&child, &mut Fake::open).unwrap());
        let session =
            WorkerSession::connect(parent, None, &[start], b"i", &config(), &[], "fake").unwrap();
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
        serve(&inherited_stream().unwrap(), &mut Fake::open).unwrap();
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
        assert_eq!(session.telemetry_counters(), vec![("runs".to_owned(), 1)]);
        assert_eq!(session.snapshot_owned_pages(snap), Some(1));
        assert_eq!(session.snapshot_owned_pages(SnapId(9)), None);
        let held = session.store_bytes().expect("store bytes");
        assert!(held > 0);
        assert_eq!(session.console_tail().unwrap(), b"tail");
        session
            .branch_with_service(SnapId(1), ServiceConfig::default(), Vec::new(), Vec::new())
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
            session.telemetry_counters(),
            vec![("runs".to_owned(), 1)],
            "the last counters outlive the worker"
        );
        assert_eq!(session.store_bytes(), None);
        drop(session);
        server.join().unwrap();
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
    fn a_worker_that_dies_before_acknowledging_a_write_publishes_nothing() {
        let (parent, child) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut served = Served::default();
            loop {
                let (body, fds) = recv(&child).unwrap();
                let reply = status(served.handle(&body, &fds, &mut Fake::open));
                if body[0] == WRITE_EXTENT {
                    return;
                }
                send(&child, &reply, &[]).unwrap();
            }
        });
        let mut session =
            WorkerSession::connect(parent, None, &[3], b"i", &config(), &[], "fake").unwrap();
        let index = LocalIndex::new(1 << 20);
        let (snap, _) = session.snapshot().unwrap();
        let error = session
            .publish_snapshot(&index, namespace(), b"key", None, snap, 1)
            .unwrap_err();
        assert!(error.to_string().contains("session worker"), "{error}");
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
    fn killed_worker_processes_release_their_leases_and_keep_the_cache_consistent() {
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
