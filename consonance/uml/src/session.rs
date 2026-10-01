// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Command;
use std::time::Duration;

use hypercall_proto::MAX_FRAME;
use tempfile::TempDir;

use crate::bridge::{self, Event, STAMP_LEN, Services};
use crate::launch::{Exit, ExitReason, Guest, Launch, LaunchError};
use crate::profile::VerifiedProfile;

const CONTROL_MAGIC: u32 = 0x3143_5548;
const CAPTURE: u32 = 1;
const RESTORE: u32 = 2;
const OMIT_HOST_MEMORY: u64 = 1;
const CONTROL_LEN: usize = 16;
const POLL: Duration = Duration::from_millis(5);
const RESTORE_ARGUMENT: &str = "harmony_restore";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Event,
    Closed,
    Stopped(ExitReason),
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
    #[error("a session needs a bridge")]
    NoBridge,
    #[error("the guest is not paused at an event")]
    NotPaused,
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
    image: OwnedFd,
    bytes: u64,
    services: Services,
    answer: Vec<u8>,
}

pub struct Session {
    guest: Guest,
    socket: OwnedFd,
    services: Services,
    pending: Option<Vec<u8>>,
    failure: Option<String>,
    request: Vec<u8>,
    response: Vec<u8>,
}

impl Checkpoint {
    pub fn events(&self) -> &[Event] {
        &self.services.events
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn allocated_bytes(&self) -> io::Result<u64> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `stat` points to a live, writable stat buffer for the call
        // and `image` is an open descriptor owned by this checkpoint.
        if unsafe { libc::fstat(self.image.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstat(2) succeeded, so it initialized the buffer.
        let stat = unsafe { stat.assume_init() };
        Ok(u64::try_from(stat.st_blocks).unwrap_or(0) * 512)
    }

    pub fn with_bridge_of(&self, other: &Checkpoint) -> io::Result<Checkpoint> {
        Ok(Checkpoint {
            image: self.image.try_clone()?,
            bytes: self.bytes,
            services: other.services.clone(),
            answer: other.answer.clone(),
        })
    }
}

impl Session {
    pub fn spawn(
        launch: &Launch,
        profile: &VerifiedProfile,
        restore: Option<&Checkpoint>,
    ) -> Result<Self, SessionError> {
        let work = launch.work_directory()?;
        let command = launch.command(profile, work.path())?;
        Self::start(command, work, launch, restore)
    }

    pub fn start(
        mut command: Command,
        work: TempDir,
        launch: &Launch,
        restore: Option<&Checkpoint>,
    ) -> Result<Self, SessionError> {
        let bridge = launch.bridge.ok_or(SessionError::NoBridge)?;
        if restore.is_some() {
            command.arg(RESTORE_ARGUMENT);
        }
        let (guest, socket) = Guest::start_bridged(command, work, launch, false)?;
        let socket = socket.ok_or(SessionError::NoBridge)?;
        let mut session = Self {
            guest,
            socket,
            services: Services::new(bridge.seed),
            pending: None,
            failure: None,
            request: vec![0; STAMP_LEN + MAX_FRAME],
            response: vec![0; MAX_FRAME],
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

    pub fn run_to(&mut self, events: usize) -> Result<Stop, SessionError> {
        if events <= self.services.events.len() {
            return Err(SessionError::Behind(events));
        }
        self.serve(Some(events))
    }

    pub fn run(mut self) -> Result<Exit, SessionError> {
        let reason = match self.serve(None)? {
            Stop::Stopped(reason) => reason,
            Stop::Event | Stop::Closed => loop {
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
        let answer = self.pending.clone().ok_or(SessionError::NotPaused)?;
        let image = memfd()?;
        let flags = if capture.omit_host_memory {
            OMIT_HOST_MEMORY
        } else {
            0
        };
        let bytes = self.control(CAPTURE, flags, &image, "capture")?;
        Ok(Checkpoint {
            image,
            bytes: bytes.unsigned_abs(),
            services: self.services.clone(),
            answer,
        })
    }

    pub fn restore(&mut self, checkpoint: &Checkpoint) -> Result<(), SessionError> {
        if self.pending.is_none() {
            return Err(SessionError::NotPaused);
        }
        self.restore_from(checkpoint)
    }

    fn restore_from(&mut self, checkpoint: &Checkpoint) -> Result<(), SessionError> {
        self.control(RESTORE, 0, &checkpoint.image, "restore")?;
        self.services = checkpoint.services.clone();
        self.pending = Some(checkpoint.answer.clone());
        Ok(())
    }

    fn serve(&mut self, target: Option<usize>) -> Result<Stop, SessionError> {
        if let Some(answer) = self.pending.take() {
            bridge::send(&self.socket, &answer)?;
        }
        loop {
            let length = match self.receive() {
                Ok(0) => return Ok(Stop::Closed),
                Ok(length) => length,
                Err(SessionError::Stopped(reason)) => return Ok(Stop::Stopped(reason)),
                Err(error) => return Err(error),
            };
            let recorded = self.services.events.len();
            let answer = match self
                .services
                .exchange(&self.request[..length], &mut self.response)
            {
                Ok(answer) => answer,
                Err(failure) => {
                    self.failure = Some(failure.clone());
                    return Err(SessionError::Protocol(failure));
                }
            };
            let count = self.services.events.len();
            if count > recorded && Some(count) == target {
                self.pending = Some(self.response[..answer].to_vec());
                return Ok(Stop::Event);
            }
            bridge::send(&self.socket, &self.response[..answer])?;
        }
    }

    fn receive(&mut self) -> Result<usize, SessionError> {
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
        }
    }

    fn control(
        &mut self,
        command: u32,
        flags: u64,
        image: &OwnedFd,
        operation: &'static str,
    ) -> Result<i64, SessionError> {
        let mut message = [0_u8; CONTROL_LEN];
        message[..4].copy_from_slice(&CONTROL_MAGIC.to_le_bytes());
        message[4..8].copy_from_slice(&command.to_le_bytes());
        message[8..].copy_from_slice(&flags.to_le_bytes());
        send_with_fd(&self.socket, &message, image)?;
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
        if length != CONTROL_LEN || magic != CONTROL_MAGIC || acked != command {
            return Err(SessionError::Protocol(format!(
                "expected a {operation} acknowledgement, received command {acked}"
            )));
        }
        if result < 0 {
            return Err(SessionError::Refused {
                operation,
                errno: -result,
            });
        }
        Ok(result)
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

fn memfd() -> io::Result<OwnedFd> {
    // SAFETY: the name is a NUL-terminated string that outlives the call.
    let fd = unsafe { libc::memfd_create(c"harmony-uml-checkpoint".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: memfd_create(2) succeeded, so `fd` is open and owned by nothing
    // else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
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
    // carrying one descriptor, so CMSG_FIRSTHDR returns a pointer into it and
    // CMSG_DATA points at the descriptor slot inside the same buffer.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn control_messages_carry_the_image_descriptor() {
        let (host, guest) = bridge::socket_pair().unwrap();
        let image = memfd().unwrap();
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
