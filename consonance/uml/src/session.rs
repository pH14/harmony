// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::process::Command;
use std::time::{Duration, Instant};

use environment::channel::ServiceHandler;
use environment::input_spec::ServiceFactory;
use hypercall_proto::MAX_FRAME;
use sha2::{Digest, Sha256};
use snapshot_store::{PAGE_SIZE, PageHash};
use tempfile::TempDir;

use crate::bridge::{self, Event, Reader, STAMP_LEN, Services, Signal};
use crate::launch::{Exit, ExitReason, Guest, Launch, LaunchError, PHYSMEM_FD};
use crate::memory::{
    Checkpoints, GuestMemory, IMAGE_PAGES, MemoryError, Snapshot, data_extents, image_state,
};
use crate::profile::VerifiedProfile;

const CONTROL_MAGIC: u32 = 0x3143_5548;
const CAPTURE: u32 = 1;
const RESTORE: u32 = 2;
const MEMORY: u32 = 4;
const CONTINUE: u32 = 5;
const MEMORY_ALL: i64 = 1;
const MEMORY_CHUNK: usize = 65536;
const OMIT_HOST_MEMORY: u64 = 1;
const CONTROL_LEN: usize = 16;
const POLL: Duration = Duration::from_millis(5);
const RESTORE_ARGUMENT: &str = "harmony_restore";
const SIDECAR_MAGIC: &[u8; 8] = b"HUMLCKP2";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    Event,
    Deadline(u64),
    SnapshotPoint(u64),
    Violation { moment: u64, id: u32, data: Vec<u8> },
    Exhausted(u64),
    Closed,
    Stopped(ExitReason),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Until {
    Events(usize),
    Moment(u64),
    SnapshotPoint(u64),
    End,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capture {
    pub omit_host_memory: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Launch(#[from] LaunchError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error("a session needs a bridge")]
    NoBridge,
    #[error("the guest is not paused at a request")]
    NotPaused,
    #[error("the guest sent no request for {0:?} of host time")]
    Hung(Duration),
    #[error("event {0} is not ahead of the guest")]
    Behind(usize),
    #[error("the guest stopped: {0:?}")]
    Stopped(ExitReason),
    #[error("bridge protocol: {0}")]
    Protocol(String),
    #[error("the guest refused the {operation}: errno {errno}")]
    Refused { operation: &'static str, errno: i64 },
    #[error("restoring at boot failed: {error}; console: {console}")]
    Boot {
        error: Box<SessionError>,
        console: String,
    },
}

#[derive(Debug)]
pub struct Checkpoint {
    memory: Snapshot,
    services: Services,
    request: Vec<u8>,
}

enum Ack {
    Done(i64),
    Memory(Report),
}

enum Report {
    All,
    Dirty(Vec<u64>),
}

pub struct Session {
    guest: Guest,
    socket: OwnedFd,
    services: Services,
    pending: Option<Vec<u8>>,
    failure: Option<String>,
    progress_limit: Option<Duration>,
    request: Vec<u8>,
    response: Vec<u8>,
    memory: GuestMemory,
    checkpoints: Checkpoints,
    memory_at: Option<Snapshot>,
    image_at: Option<Snapshot>,
    audit: bool,
    missed: Vec<u64>,
}

impl Checkpoint {
    pub fn events(&self) -> &[Event] {
        &self.services.events
    }

    pub fn bytes(&self) -> u64 {
        self.memory.image_bytes()
    }

    pub fn owned_pages(&self) -> u64 {
        self.memory.owned_pages()
    }

    pub fn moment(&self) -> u64 {
        stamp(&self.request).unwrap_or(self.services.latest)
    }

    pub fn with_bridge_of(&self, other: &Checkpoint) -> Checkpoint {
        Checkpoint {
            memory: self.memory.share(),
            services: other.services.clone(),
            request: other.request.clone(),
        }
    }

    pub fn digest(&self) -> Result<[u8; 32], SessionError> {
        let mut digest = Sha256::new();
        digest.update(b"harmony-uml-checkpoint-v1\0");
        self.memory.digest(&mut digest)?;
        digest.update(self.sidecar()?);
        Ok(digest.finalize().into())
    }

    pub fn sidecar(&self) -> Result<Vec<u8>, SessionError> {
        let services = self.services.encode().map_err(SessionError::Protocol)?;
        let mut out =
            Vec::with_capacity(SIDECAR_MAGIC.len() + 24 + services.len() + self.request.len());
        out.extend_from_slice(SIDECAR_MAGIC);
        out.extend_from_slice(&self.bytes().to_le_bytes());
        out.extend_from_slice(&(services.len() as u64).to_le_bytes());
        out.extend_from_slice(&services);
        out.extend_from_slice(&(self.request.len() as u64).to_le_bytes());
        out.extend_from_slice(&self.request);
        Ok(out)
    }
}

impl Checkpoints {
    pub fn delta<R>(
        &self,
        base: &Checkpoint,
        parent: Option<&Checkpoint>,
        target: &Checkpoint,
        use_delta: impl FnOnce(&[(u64, &PageHash, &[u8; PAGE_SIZE])], &[u64]) -> R,
    ) -> Result<R, SessionError> {
        Ok(self.delta_pages(
            &base.memory,
            parent.map(|parent| &parent.memory),
            &target.memory,
            |delta| use_delta(&delta.changed, &delta.reverted),
        )?)
    }

    pub fn import(
        &self,
        base: &Checkpoint,
        near: &Checkpoint,
        pages: &[(u64, &PageHash, &[u8; PAGE_SIZE])],
        sidecar: &[u8],
        factory: &ServiceFactory,
    ) -> Result<Checkpoint, SessionError> {
        let malformed = |what: &str| SessionError::Protocol(format!("checkpoint sidecar: {what}"));
        let mut reader = Reader(
            sidecar
                .strip_prefix(SIDECAR_MAGIC)
                .ok_or_else(|| malformed("magic"))?,
        );
        let length = |reader: &mut Reader<'_>| {
            reader
                .u64()
                .and_then(|length| usize::try_from(length).map_err(|error| error.to_string()))
                .map_err(SessionError::Protocol)
        };
        let image_bytes = reader.u64().map_err(SessionError::Protocol)?;
        let services = length(&mut reader)
            .and_then(|services| reader.take(services).map_err(SessionError::Protocol))?;
        let services = Services::decode(services, factory).map_err(SessionError::Protocol)?;
        let request = length(&mut reader)
            .and_then(|request| reader.take(request).map_err(SessionError::Protocol))?
            .to_vec();
        let memory = self.import_pages(&base.memory, &near.memory, pages, image_bytes)?;
        Ok(Checkpoint {
            memory,
            services,
            request,
        })
    }
}

impl Session {
    pub fn spawn(
        launch: &Launch,
        profile: &VerifiedProfile,
        checkpoints: &Checkpoints,
        restore: Option<&Checkpoint>,
    ) -> Result<Self, SessionError> {
        let work = launch.work_directory()?;
        let command = launch.command(profile, work.path())?;
        Self::start(command, work, launch, checkpoints, restore)
    }

    pub fn start(
        mut command: Command,
        work: TempDir,
        launch: &Launch,
        checkpoints: &Checkpoints,
        restore: Option<&Checkpoint>,
    ) -> Result<Self, SessionError> {
        let bridge = launch.bridge.ok_or(SessionError::NoBridge)?;
        let memory = GuestMemory::new()?;
        command.arg(format!("harmony_physmem={PHYSMEM_FD}"));
        if restore.is_some() {
            command.arg(RESTORE_ARGUMENT);
        }
        let (guest, socket) = Guest::start_bridged(
            command,
            work,
            launch,
            false,
            Some(memory.physmem.try_clone()?),
        )?;
        let socket = socket.ok_or(SessionError::NoBridge)?;
        let mut session = Self {
            guest,
            socket,
            services: Services::new(bridge.seed),
            pending: None,
            failure: None,
            progress_limit: None,
            request: vec![0; STAMP_LEN + MAX_FRAME],
            response: vec![0; MAX_FRAME],
            memory,
            checkpoints: checkpoints.clone(),
            memory_at: None,
            image_at: None,
            audit: false,
            missed: Vec::new(),
        };
        if let Some(checkpoint) = restore
            && let Err(error) = session.restore_at_boot(checkpoint)
        {
            return Err(SessionError::Boot {
                error: Box::new(error),
                console: session.console_tail(),
            });
        }
        Ok(session)
    }

    fn restore_at_boot(&mut self, checkpoint: &Checkpoint) -> Result<(), SessionError> {
        let length = self.receive()?;
        if length != STAMP_LEN {
            return Err(SessionError::Protocol(format!(
                "expected the restore request, received {length} bytes"
            )));
        }
        self.restore_from(checkpoint)
    }

    pub fn console_tail(&self) -> String {
        let tail = self.guest.console_tail();
        String::from_utf8_lossy(&tail[tail.len().saturating_sub(4096)..]).into_owned()
    }

    pub fn events(&self) -> &[Event] {
        &self.services.events
    }

    pub fn moment(&self) -> u64 {
        self.pending
            .as_deref()
            .and_then(stamp)
            .unwrap_or(self.services.latest)
    }

    pub fn set_progress_limit(&mut self, limit: Option<Duration>) {
        self.progress_limit = limit;
    }

    pub fn branch(
        &mut self,
        seed: u64,
        handler: Box<dyn ServiceHandler>,
        payloads: Vec<Vec<u8>>,
    ) -> Result<(), SessionError> {
        self.services
            .branch(seed, handler, payloads)
            .map_err(SessionError::Protocol)
    }

    pub fn run_to(&mut self, events: usize) -> Result<Stop, SessionError> {
        if events <= self.services.events.len() {
            return Err(SessionError::Behind(events));
        }
        self.serve(Until::Events(events))
    }

    pub fn run_until(&mut self, deadline: u64) -> Result<Stop, SessionError> {
        self.serve(Until::Moment(deadline))
    }

    pub fn run_to_snapshot_point(&mut self, deadline: u64) -> Result<Stop, SessionError> {
        self.serve(Until::SnapshotPoint(deadline))
    }

    pub fn paused(&self) -> bool {
        self.pending.is_some()
    }

    pub fn state_hash(&self) -> Result<[u8; 32], SessionError> {
        let mut digest = Sha256::new();
        digest.update(b"harmony-uml-session-state-v1\0");
        digest.update(self.services.encode().map_err(SessionError::Protocol)?);
        if let Some(request) = &self.pending {
            digest.update(request);
        }
        Ok(digest.finalize().into())
    }

    pub fn run(mut self) -> Result<Exit, SessionError> {
        let reason = match self.serve(Until::End)? {
            Stop::Stopped(reason) => reason,
            _ => loop {
                if let Some(reason) = self.guest.stopped()? {
                    break reason;
                }
                std::thread::sleep(POLL);
            },
        };
        self.finish(reason)
    }

    pub fn kill(mut self) -> Result<Exit, SessionError> {
        self.finish(ExitReason::Killed)
    }

    pub fn capture(&mut self, capture: Capture) -> Result<Checkpoint, SessionError> {
        let request = self.pending.clone().ok_or(SessionError::NotPaused)?;
        let flags = if capture.omit_host_memory {
            OMIT_HOST_MEMORY
        } else {
            0
        };
        self.send_control(CAPTURE, flags, true)?;
        let Ack::Memory(report) = self.acknowledgement(CAPTURE, "capture")? else {
            return Err(SessionError::Protocol(
                "the guest captured without reporting its memory".into(),
            ));
        };
        let previous = self.memory_at.take();
        let previous_image = self.image_at.take();
        let physmem_bytes = self.memory.map()?;
        let checkpoints = self.checkpoints.clone();
        let mut pages = checkpoints.pages(physmem_bytes)?;
        let parent = match (&report, &previous) {
            (Report::Dirty(_), Some(at)) => at.id(),
            _ => pages.root,
        };
        let parent_image = pages.image_bytes(parent)?;
        let written = self.physmem_pages(&report, physmem_bytes)?;
        let audit = if self.audit && matches!(report, Report::Dirty(_)) && previous.is_some() {
            Some(self.audit_candidates(&pages, parent, physmem_bytes)?)
        } else {
            None
        };
        // SAFETY: the guest waits for CONTINUE, so its physical memory does not
        // change, and the image file keeps its full length until the guest
        // writes the image after CONTINUE.
        let bytes = unsafe { self.memory.bytes() }.ok_or(MemoryError::Empty)?;
        if let Some(candidates) = audit {
            let missed = unexpected_pages(&pages, parent, candidates, &written, bytes)?;
            self.missed.extend(missed);
        }
        let mut builder = pages.store.derive(parent).map_err(MemoryError::from)?;
        for gfn in written {
            let at = usize::try_from(gfn).map_err(io::Error::other)? * PAGE_SIZE;
            builder
                .write_changed_page(gfn, &bytes[at..at + PAGE_SIZE])
                .map_err(MemoryError::from)?;
        }
        self.send_continue()?;
        let outcome = match self.acknowledgement(CAPTURE, "capture") {
            Ok(Ack::Done(length)) => {
                let length = length.unsigned_abs();
                self.memory
                    .restore_image_size(length)
                    .map(|()| length)
                    .map_err(SessionError::from)
            }
            Ok(Ack::Memory(_)) => Err(SessionError::Protocol(
                "the guest reported its memory twice".into(),
            )),
            Err(error) => Err(error),
        };
        let length = match outcome {
            Ok(length) => length,
            Err(error) => {
                let id = builder.seal(image_state(parent_image));
                drop(pages);
                self.memory_at = Some(checkpoints.adopt(id));
                return Err(error);
            }
        };
        let image_pages = length.max(parent_image).div_ceil(PAGE_SIZE as u64);
        // SAFETY: the guest has written the image and waits for the bridge
        // reply, so neither range changes, and the image file has its full
        // length again.
        let bytes = unsafe { self.memory.bytes() }.ok_or(MemoryError::Empty)?;
        for gfn in 0..image_pages {
            let at = usize::try_from(gfn).map_err(io::Error::other)? * PAGE_SIZE;
            builder
                .write_changed_page(gfn, &bytes[at..at + PAGE_SIZE])
                .map_err(MemoryError::from)?;
        }
        let id = builder.seal(image_state(length));
        let id = pages.shorten(id, bytes)?;
        drop(pages);
        drop(previous);
        drop(previous_image);
        let memory = checkpoints.adopt(id);
        self.memory_at = Some(memory.share());
        self.image_at = Some(memory.share());
        Ok(Checkpoint {
            memory,
            services: self.services.clone(),
            request,
        })
    }

    pub fn restore(&mut self, checkpoint: &Checkpoint) -> Result<(), SessionError> {
        if self.pending.is_none() {
            return Err(SessionError::NotPaused);
        }
        self.restore_from(checkpoint)
    }

    fn restore_from(&mut self, checkpoint: &Checkpoint) -> Result<(), SessionError> {
        let physmem_bytes = self.memory.map()?;
        let checkpoints = self.checkpoints.clone();
        let target = checkpoint.memory.id();
        let previous_image = self.image_at.take();
        {
            let pages = checkpoints.pages(physmem_bytes)?;
            let from = match &previous_image {
                Some(at) => at.id(),
                None => {
                    self.memory.clear_image()?;
                    pages.root
                }
            };
            // SAFETY: the image file has its full length and only the host
            // writes it while the guest is paused.
            let bytes = unsafe { self.memory.bytes() }.ok_or(MemoryError::Empty)?;
            let plan = pages
                .store
                .restore_pages(Some(from), target, &[])
                .map_err(MemoryError::from)?;
            for (gfn, data) in plan.into_iter().take_while(|(gfn, _)| *gfn < IMAGE_PAGES) {
                let at = usize::try_from(gfn).map_err(io::Error::other)? * PAGE_SIZE;
                bytes[at..at + PAGE_SIZE].copy_from_slice(data);
            }
        }
        drop(previous_image);
        self.image_at = Some(checkpoint.memory.share());
        self.send_control(RESTORE, 0, true)?;
        let Ack::Memory(report) = self.acknowledgement(RESTORE, "restore")? else {
            return Err(SessionError::Protocol(
                "the guest restored without reporting its memory".into(),
            ));
        };
        let previous = self.memory_at.take();
        {
            let pages = checkpoints.pages(physmem_bytes)?;
            let (from, dirty) = match (&report, &previous) {
                (Report::Dirty(_), Some(at)) => {
                    (at.id(), self.physmem_pages(&report, physmem_bytes)?)
                }
                _ => {
                    self.memory.clear_physmem()?;
                    (pages.root, Vec::new())
                }
            };
            let audit = if self.audit {
                Some(self.audit_candidates(&pages, target, physmem_bytes)?)
            } else {
                None
            };
            // SAFETY: the guest waits for CONTINUE, so only the host touches
            // its physical memory until then.
            let bytes = unsafe { self.memory.bytes() }.ok_or(MemoryError::Empty)?;
            let plan = pages
                .store
                .restore_pages(Some(from), target, &dirty)
                .map_err(MemoryError::from)?;
            for (gfn, data) in plan {
                if gfn < IMAGE_PAGES {
                    continue;
                }
                let at = usize::try_from(gfn).map_err(io::Error::other)? * PAGE_SIZE;
                bytes[at..at + PAGE_SIZE].copy_from_slice(data);
            }
            if let Some(candidates) = audit {
                let missed = unexpected_pages(&pages, target, candidates, &[], bytes)?;
                self.missed.extend(missed);
            }
        }
        drop(previous);
        self.memory_at = Some(checkpoint.memory.share());
        self.send_continue()?;
        let Ack::Done(_) = self.acknowledgement(RESTORE, "restore")? else {
            return Err(SessionError::Protocol(
                "the guest reported its memory twice".into(),
            ));
        };
        self.services = checkpoint.services.clone();
        self.pending = Some(checkpoint.request.clone());
        Ok(())
    }

    pub fn set_audit(&mut self, audit: bool) {
        self.audit = audit;
    }

    pub fn missed_writes(&self) -> &[u64] {
        &self.missed
    }

    fn audit_candidates(
        &self,
        pages: &crate::memory::Pages,
        expected: snapshot_store::SnapshotId,
        physmem_bytes: usize,
    ) -> Result<Vec<u64>, SessionError> {
        let mut candidates = self.physmem_pages(&Report::All, physmem_bytes)?;
        candidates.extend(
            pages
                .store
                .restore_pages(Some(pages.root), expected, &[])
                .map_err(MemoryError::from)?
                .into_iter()
                .map(|(gfn, _)| gfn)
                .filter(|&gfn| gfn >= IMAGE_PAGES),
        );
        candidates.sort_unstable();
        candidates.dedup();
        Ok(candidates)
    }

    fn physmem_pages(&self, report: &Report, physmem_bytes: usize) -> io::Result<Vec<u64>> {
        let frames = (physmem_bytes / PAGE_SIZE) as u64;
        Ok(match report {
            Report::Dirty(frames_written) => frames_written
                .iter()
                .filter(|&&frame| frame < frames)
                .map(|frame| IMAGE_PAGES + frame)
                .collect(),
            Report::All => data_extents(&self.memory.physmem, physmem_bytes as u64)?
                .into_iter()
                .flat_map(|(offset, length)| {
                    let first = offset / PAGE_SIZE as u64;
                    first..(offset + length).div_ceil(PAGE_SIZE as u64)
                })
                .map(|frame| IMAGE_PAGES + frame)
                .collect(),
        })
    }

    fn serve(&mut self, until: Until) -> Result<Stop, SessionError> {
        let mut held = self.pending.take();
        let mut due = None;
        let mut snapshot_point = false;
        loop {
            let length = if let Some(request) = held.take() {
                self.request[..request.len()].copy_from_slice(&request);
                request.len()
            } else {
                let length = match self.receive() {
                    Ok(0) => return Ok(due.unwrap_or(Stop::Closed)),
                    Ok(length) => length,
                    Err(SessionError::Stopped(reason)) => {
                        return Ok(due.unwrap_or(Stop::Stopped(reason)));
                    }
                    Err(error) => return Err(error),
                };
                let moment = stamp(&self.request[..length])
                    .ok_or_else(|| SessionError::Protocol(format!("request of {length} bytes")))?;
                let stop = match until {
                    _ if due.is_some() => due.take(),
                    Until::SnapshotPoint(_) if snapshot_point => Some(Stop::SnapshotPoint(moment)),
                    Until::Moment(deadline) | Until::SnapshotPoint(deadline)
                        if moment >= deadline =>
                    {
                        Some(Stop::Deadline(moment))
                    }
                    Until::Events(count) if self.services.events.len() >= count => {
                        Some(Stop::Event)
                    }
                    _ => None,
                };
                if let Some(stop) = stop {
                    self.pending = Some(self.request[..length].to_vec());
                    return Ok(stop);
                }
                length
            };
            let reply = match self
                .services
                .exchange(&self.request[..length], &mut self.response)
            {
                Ok(reply) => reply,
                Err(failure) => {
                    self.failure = Some(failure.clone());
                    return Err(SessionError::Protocol(failure));
                }
            };
            bridge::send(&self.socket, &self.response[..reply.length])?;
            let moment = self.services.latest;
            match reply.signal {
                Some(_) if until == Until::End => {}
                Some(Signal::Violation { id, data }) => {
                    due = Some(Stop::Violation { moment, id, data });
                }
                Some(Signal::Exhausted) => due = Some(Stop::Exhausted(moment)),
                Some(Signal::SnapshotPoint) => snapshot_point = true,
                None => {}
            }
        }
    }

    fn receive(&mut self) -> Result<usize, SessionError> {
        let waited = now();
        loop {
            let mut poll = libc::pollfd {
                fd: self.socket.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout = i32::try_from(POLL.as_millis()).unwrap_or(i32::MAX);
            // SAFETY: `poll` is one live, writable pollfd for the call.
            let ready = unsafe { libc::poll(&mut poll, 1, timeout) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            if ready > 0 {
                let length = bridge::receive(&self.socket, &mut self.request)?;
                if length > self.request.len() {
                    return Err(SessionError::Protocol(format!("request of {length} bytes")));
                }
                return Ok(length);
            }
            if let Some(reason) = self.guest.stopped()? {
                return Err(SessionError::Stopped(reason));
            }
            if let Some(limit) = self.progress_limit
                && waited.elapsed() >= limit
            {
                return Err(SessionError::Hung(limit));
            }
        }
    }

    fn send_control(&mut self, command: u32, flags: u64, image: bool) -> io::Result<()> {
        let mut message = [0_u8; CONTROL_LEN];
        message[..4].copy_from_slice(&CONTROL_MAGIC.to_le_bytes());
        message[4..8].copy_from_slice(&command.to_le_bytes());
        message[8..].copy_from_slice(&flags.to_le_bytes());
        if image {
            send_with_fd(&self.socket, &message, &self.memory.image)
        } else {
            bridge::send(&self.socket, &message)
        }
    }

    fn send_continue(&mut self) -> io::Result<()> {
        self.send_control(CONTINUE, 0, false)
    }

    fn acknowledgement(
        &mut self,
        command: u32,
        operation: &'static str,
    ) -> Result<Ack, SessionError> {
        let length = self.receive()?;
        let ack = &self.request[..length];
        let (Some(magic), Some(acked), Some(result)) =
            (ack.get(..4), ack.get(4..8), ack.get(8..CONTROL_LEN))
        else {
            return Err(SessionError::Protocol(format!(
                "expected a {operation} acknowledgement, received {length} bytes"
            )));
        };
        let magic = u32::from_le_bytes(magic.try_into().unwrap_or_default());
        let acked = u32::from_le_bytes(acked.try_into().unwrap_or_default());
        let result = i64::from_le_bytes(result.try_into().unwrap_or_default());
        if length != CONTROL_LEN || magic != CONTROL_MAGIC || (acked != command && acked != MEMORY)
        {
            return Err(SessionError::Protocol(format!(
                "expected a {operation} acknowledgement, received command {acked}"
            )));
        }
        if acked == MEMORY {
            return self.memory_report(result).map(Ack::Memory);
        }
        if result < 0 {
            return Err(SessionError::Refused {
                operation,
                errno: -result,
            });
        }
        Ok(Ack::Done(result))
    }

    fn memory_report(&mut self, flags: i64) -> Result<Report, SessionError> {
        if flags & MEMORY_ALL != 0 {
            return Ok(Report::All);
        }
        let frames = self.memory.physmem_bytes()? / PAGE_SIZE;
        let bitmap = frames.div_ceil(64) * 8;
        if self.request.len() < MEMORY_CHUNK {
            self.request.resize(MEMORY_CHUNK, 0);
        }
        let mut written = Vec::new();
        let mut received = 0;
        while received < bitmap {
            let chunk = (bitmap - received).min(MEMORY_CHUNK);
            let length = self.receive()?;
            if length != chunk {
                return Err(SessionError::Protocol(format!(
                    "expected {chunk} bytes of the {bitmap}-byte memory bitmap at {received}, received {length} bytes"
                )));
            }
            let first = (received / 8) as u64;
            for (index, word) in self.request[..chunk].chunks_exact(8).enumerate() {
                let mut word = u64::from_le_bytes(word.try_into().unwrap_or_default());
                while word != 0 {
                    written.push((first + index as u64) * 64 + u64::from(word.trailing_zeros()));
                    word &= word - 1;
                }
            }
            received += chunk;
        }
        Ok(Report::Dirty(written))
    }

    fn finish(&mut self, reason: ExitReason) -> Result<Exit, SessionError> {
        let reason = if self.failure.is_some() {
            ExitReason::BridgeFailure
        } else {
            reason
        };
        let mut exit = self.guest.finish(reason)?;
        exit.events = std::mem::take(&mut self.services.events);
        exit.bridge_failure = self.failure.take();
        Ok(exit)
    }
}

fn stamp(request: &[u8]) -> Option<u64> {
    request
        .first_chunk::<STAMP_LEN>()
        .map(|stamp| u64::from_le_bytes(*stamp))
}

#[expect(
    clippy::disallowed_methods,
    reason = "the progress watchdog measures host time the guest spends without a request"
)]
fn now() -> Instant {
    Instant::now()
}

fn send_with_fd(socket: &OwnedFd, message: &[u8], fd: &OwnedFd) -> io::Result<()> {
    let raw = fd.as_raw_fd();
    // SAFETY: CMSG_SPACE only computes a size from its argument.
    let space = unsafe { libc::CMSG_SPACE(size_of_val(&raw) as u32) } as usize;
    let mut control = vec![0_u8; space];
    let mut iov = libc::iovec {
        iov_base: message.as_ptr().cast_mut().cast(),
        iov_len: message.len(),
    };
    // SAFETY: an all-zero msghdr is a valid empty header.
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    header.msg_iov = &mut iov;
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr().cast();
    header.msg_controllen = space as _;
    // SAFETY: `header` describes `control`, which has room for one cmsghdr
    // carrying one descriptor, so `CMSG_FIRSTHDR` returns a pointer into it
    // and `CMSG_DATA` points at the descriptor slot inside the same buffer.
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&header);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(size_of_val(&raw) as u32) as _;
        libc::CMSG_DATA(cmsg).cast::<i32>().write_unaligned(raw);
    }
    loop {
        // SAFETY: `header` points at `iov`, `message` and `control`, which all
        // stay live and unmoved for the call.
        let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &header, libc::MSG_NOSIGNAL) };
        if sent >= 0 {
            return if sent.unsigned_abs() == message.len() {
                Ok(())
            } else {
                Err(io::Error::other("short control message"))
            };
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn unexpected_pages(
    pages: &crate::memory::Pages,
    expected: snapshot_store::SnapshotId,
    candidates: Vec<u64>,
    skip: &[u64],
    bytes: &[u8],
) -> Result<Vec<u64>, SessionError> {
    let mut stored = [0_u8; PAGE_SIZE];
    let mut missed = Vec::new();
    for gfn in candidates {
        if skip.binary_search(&gfn).is_ok() {
            continue;
        }
        pages
            .store
            .read_page(expected, gfn, &mut stored)
            .map_err(MemoryError::from)?;
        let at = usize::try_from(gfn).map_err(io::Error::other)? * PAGE_SIZE;
        if bytes[at..at + PAGE_SIZE] != stored {
            missed.push(gfn - IMAGE_PAGES);
        }
    }
    Ok(missed)
}

#[cfg(test)]
mod tests {
    use std::os::fd::FromRawFd;

    use super::*;
    use environment::input_spec::nominal_factory;

    const PHYSMEM_BYTES: usize = 4 * PAGE_SIZE;

    fn snapshot(
        checkpoints: &Checkpoints,
        parent: Option<&Snapshot>,
        pages: &[(u64, u8)],
    ) -> Snapshot {
        let mut held = checkpoints.pages(PHYSMEM_BYTES).unwrap();
        let parent = parent.map_or(held.root, Snapshot::id);
        let mut builder = held.store.derive(parent).unwrap();
        for &(gfn, fill) in pages {
            builder.write_changed_page(gfn, &[fill; PAGE_SIZE]).unwrap();
        }
        let id = builder.seal(image_state(24));
        drop(held);
        checkpoints.adopt(id)
    }

    fn checkpoint(memory: Snapshot, seed: u64, moment: Option<u64>) -> Checkpoint {
        let mut services = Services::new(seed);
        services.latest = 90;
        services.events.push(Event {
            moment: 80,
            id: 3,
            data: vec![1, 2, 3].into(),
        });
        Checkpoint {
            memory,
            services,
            request: moment.map_or_else(Vec::new, |moment| moment.to_le_bytes().to_vec()),
        }
    }

    fn owned_delta(
        checkpoints: &Checkpoints,
        base: &Checkpoint,
        target: &Checkpoint,
    ) -> Vec<(u64, PageHash, [u8; PAGE_SIZE])> {
        checkpoints
            .delta(base, None, target, |changed, reverted| {
                assert!(reverted.is_empty());
                changed
                    .iter()
                    .map(|&(gfn, hash, data)| (gfn, *hash, *data))
                    .collect()
            })
            .unwrap()
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn checkpoints_report_their_moment_events_and_image() {
        let checkpoints = Checkpoints::new();
        let stamped = checkpoint(snapshot(&checkpoints, None, &[(1, 1)]), 5, Some(120));
        assert_eq!(stamped.moment(), 120);
        assert_eq!(stamped.bytes(), 24);
        assert_eq!(stamped.owned_pages(), 1);
        assert_eq!(stamped.events().len(), 1);
        assert_eq!(stamped.events()[0].moment, 80);

        let unstamped = checkpoint(snapshot(&checkpoints, None, &[(1, 1)]), 5, None);
        assert_eq!(unstamped.moment(), 90);
        assert_eq!(unstamped.digest().unwrap(), {
            let other = checkpoint(snapshot(&checkpoints, None, &[(1, 1)]), 5, None);
            other.digest().unwrap()
        });
        assert_ne!(unstamped.digest().unwrap(), stamped.digest().unwrap());

        let reseeded = checkpoint(snapshot(&checkpoints, None, &[(2, 2)]), 6, Some(200));
        let bridged = stamped.with_bridge_of(&reseeded);
        assert_eq!(bridged.moment(), 200);
        assert_eq!(bridged.memory.id(), stamped.memory.id());
        assert_eq!(bridged.sidecar().unwrap(), reseeded.sidecar().unwrap());
        assert_ne!(bridged.digest().unwrap(), stamped.digest().unwrap());
        assert_ne!(bridged.digest().unwrap(), reseeded.digest().unwrap());
        drop(stamped);
        assert_eq!(bridged.bytes(), 24);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn imported_checkpoints_reproduce_the_published_digest() {
        let publisher = Checkpoints::new();
        let base = checkpoint(snapshot(&publisher, None, &[(0, 1)]), 5, Some(10));
        let target = checkpoint(
            snapshot(&publisher, Some(&base.memory), &[(2, 7), (3, 8)]),
            9,
            Some(400),
        );
        let rows = owned_delta(&publisher, &base, &target);
        assert_eq!(rows.iter().map(|row| row.0).collect::<Vec<_>>(), vec![2, 3]);
        let borrowed: Vec<(u64, &PageHash, &[u8; PAGE_SIZE])> = rows
            .iter()
            .map(|(gfn, hash, data)| (*gfn, hash, data))
            .collect();

        let subscriber = Checkpoints::new();
        let subscriber_base = checkpoint(snapshot(&subscriber, None, &[(0, 1)]), 5, Some(10));
        assert_eq!(subscriber_base.digest().unwrap(), base.digest().unwrap());
        let sidecar = target.sidecar().unwrap();
        let imported = subscriber
            .import(
                &subscriber_base,
                &subscriber_base,
                &borrowed,
                &sidecar,
                &nominal_factory(),
            )
            .unwrap();
        assert_eq!(imported.digest().unwrap(), target.digest().unwrap());
        assert_eq!(imported.moment(), 400);
        assert_eq!(imported.events(), target.events());

        for malformed in [
            &b"HUMLCKP1"[..],
            &sidecar[..SIDECAR_MAGIC.len()],
            &sidecar[..SIDECAR_MAGIC.len() + 12],
            &sidecar[..sidecar.len() - 1],
        ] {
            assert!(matches!(
                subscriber.import(
                    &subscriber_base,
                    &subscriber_base,
                    &borrowed,
                    malformed,
                    &nominal_factory(),
                ),
                Err(SessionError::Protocol(_))
            ));
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn unexpected_pages_lists_unreported_physical_writes() {
        let checkpoints = Checkpoints::new();
        let first = IMAGE_PAGES;
        let stored = snapshot(
            &checkpoints,
            None,
            &[(first, 1), (first + 1, 2), (first + 2, 3)],
        );
        let held = checkpoints.held().unwrap();
        let mut bytes = vec![0_u8; (IMAGE_PAGES as usize) * PAGE_SIZE + PHYSMEM_BYTES];
        for (gfn, fill) in [
            (first, 1_u8),
            (first + 1, 9),
            (first + 2, 4),
            (first + 3, 5),
        ] {
            let at = gfn as usize * PAGE_SIZE;
            bytes[at..at + PAGE_SIZE].fill(fill);
        }
        let missed = unexpected_pages(
            &held,
            stored.id(),
            vec![first, first + 1, first + 2, first + 3],
            &[first + 2],
            &bytes,
        )
        .unwrap();
        assert_eq!(missed, vec![1, 3]);
        assert!(
            unexpected_pages(&held, stored.id(), vec![first], &[], &bytes)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn control_messages_carry_the_image_descriptor() {
        let (host, guest) = bridge::socket_pair().unwrap();
        let image = crate::memory::memfd(c"harmony-uml-test").unwrap();
        let message = [7_u8; CONTROL_LEN];
        send_with_fd(&host, &message, &image).unwrap();

        let mut received = [0_u8; CONTROL_LEN];
        let mut control = [0_u8; 64];
        let mut iov = libc::iovec {
            iov_base: received.as_mut_ptr().cast(),
            iov_len: received.len(),
        };
        // SAFETY: an all-zero msghdr is a valid empty header.
        let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
        header.msg_iov = &mut iov;
        header.msg_iovlen = 1;
        header.msg_control = control.as_mut_ptr().cast();
        header.msg_controllen = control.len() as _;
        // SAFETY: `header` points at `iov`, `received` and `control`, which
        // stay live for the call.
        let length = unsafe { libc::recvmsg(guest.as_raw_fd(), &mut header, 0) };
        assert_eq!(length, CONTROL_LEN as isize);
        assert_eq!(received, message);
        // SAFETY: recvmsg filled `control`, so the first header and its data
        // lie inside it.
        let passed = unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&header);
            assert_eq!((*cmsg).cmsg_type, libc::SCM_RIGHTS);
            OwnedFd::from_raw_fd(libc::CMSG_DATA(cmsg).cast::<i32>().read_unaligned())
        };
        let mut first = std::mem::MaybeUninit::<libc::stat>::zeroed();
        let mut second = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: both buffers are live and writable, and both descriptors are
        // open.
        let (first, second) = unsafe {
            assert_eq!(libc::fstat(image.as_raw_fd(), first.as_mut_ptr()), 0);
            assert_eq!(libc::fstat(passed.as_raw_fd(), second.as_mut_ptr()), 0);
            (first.assume_init(), second.assume_init())
        };
        assert_eq!((first.st_dev, first.st_ino), (second.st_dev, second.st_ino));
    }
}
