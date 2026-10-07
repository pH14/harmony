// SPDX-License-Identifier: AGPL-3.0-or-later
use execution_proto::ExecutionSpec;
use process_proto::debug::{MAX_SCRIPT, Message, Operation, Status};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
};
#[derive(Default)]
pub struct Terminal {
    acknowledged: u64,
    script: Vec<u8>,
    child: Option<Child>,
    master: Option<File>,
    pending: Vec<u8>,
    exit_code: Option<i32>,
}
impl Terminal {
    pub fn child_id(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }
    pub fn status(&self) -> Status {
        Status {
            acknowledged: self.acknowledged,
            running: self.child.is_some(),
            exit_code: self.exit_code,
        }
    }
    pub fn apply(&mut self, message: Message) -> io::Result<()> {
        if message.id <= self.acknowledged {
            return Ok(());
        }
        match message.operation {
            Operation::Append => {
                if self.child.is_some()
                    || self.script.len().saturating_add(message.data.len()) > MAX_SCRIPT
                {
                    return Err(io::Error::other(
                        "debug script is too large or a shell is active",
                    ));
                }
                self.script.extend(message.data);
                self.exit_code = None;
            }
            Operation::Execute | Operation::Shell => {
                if self.child.is_some() {
                    return Err(io::Error::other("a debug shell is already active"));
                }
                let master = rustix::pty::openpt(
                    rustix::pty::OpenptFlags::RDWR
                        | rustix::pty::OpenptFlags::NOCTTY
                        | rustix::pty::OpenptFlags::CLOEXEC,
                )?;
                rustix::fs::fcntl_setfl(
                    &master,
                    rustix::fs::fcntl_getfl(&master)? | rustix::fs::OFlags::NONBLOCK,
                )?;
                rustix::pty::grantpt(&master)?;
                rustix::pty::unlockpt(&master)?;
                let name = rustix::pty::ptsname(&master, Vec::new())?;
                let slave = rustix::fs::open(
                    name.as_c_str(),
                    rustix::fs::OFlags::RDWR
                        | rustix::fs::OFlags::NOCTTY
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                )?;
                let slave = File::from(slave);
                let mut command = Command::new(std::env::current_exe()?);
                command.arg("--debug-shell");
                if message.operation == Operation::Execute {
                    fs::create_dir_all("/run/harmony/debug")?;
                    let path = format!("/run/harmony/debug/script-{}.sh", message.id);
                    fs::write(&path, &self.script)?;
                    self.script.clear();
                    command.arg(path);
                }
                command
                    .stdin(Stdio::from(slave.try_clone()?))
                    .stdout(Stdio::from(slave.try_clone()?))
                    .stderr(Stdio::from(slave));
                self.child = Some(command.spawn()?);
                self.master = Some(File::from(master));
                self.exit_code = None;
            }
            Operation::Input => {
                if self.child.is_none() {
                    return Err(io::Error::other("no debug shell is active"));
                }
                if self.pending.len().saturating_add(message.data.len()) > MAX_SCRIPT {
                    return Err(io::Error::other("debug terminal input backlog is full"));
                }
                self.pending.extend(message.data);
            }
            Operation::Close => {
                self.pending.push(4);
            }
            Operation::Resize => {
                if message.data.len() != 4 {
                    return Err(io::Error::other("invalid terminal size"));
                }
                if let Some(master) = &self.master {
                    let rows = u16::from_le_bytes(message.data[..2].try_into().unwrap());
                    let cols = u16::from_le_bytes(message.data[2..].try_into().unwrap());
                    rustix::termios::tcsetwinsize(
                        master,
                        rustix::termios::Winsize {
                            ws_row: rows,
                            ws_col: cols,
                            ws_xpixel: 0,
                            ws_ypixel: 0,
                        },
                    )?;
                }
            }
        }
        self.acknowledged = message.id;
        Ok(())
    }
    pub fn poll(&mut self) -> io::Result<Vec<u8>> {
        let exited = if let Some(child) = &mut self.child {
            child.try_wait()?
        } else {
            None
        };
        let mut output = Vec::new();
        if let Some(master) = &mut self.master {
            while !self.pending.is_empty() {
                match master.write(&self.pending) {
                    Ok(0) => break,
                    Ok(n) => {
                        self.pending.drain(..n);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e),
                }
            }
            let mut buffer = [0u8; 3000];
            while output.len() < 65536 {
                match master.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => output.extend_from_slice(&buffer[..n]),
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.raw_os_error() == Some(libc::EIO) =>
                    {
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        if output.len() < 65536
            && let Some(status) = exited
        {
            self.exit_code = Some(crate::process::exit_code(&status).into());
            self.child = None;
            self.master = None;
            self.pending.clear();
        }
        Ok(output)
    }
}
fn reset_terminal_signals() -> io::Result<()> {
    for signal in [
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTSTP,
        libc::SIGTTIN,
        libc::SIGTTOU,
    ] {
        // SAFETY: each number names a valid terminal signal and SIG_DFL installs
        // no Rust handler. This runs in the dedicated shell helper before exec.
        if unsafe { libc::signal(signal, libc::SIG_DFL) } == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

pub fn shell(spec: &ExecutionSpec, script: Option<&str>) -> io::Result<()> {
    reset_terminal_signals()?;
    rustix::process::setsid()?;
    rustix::process::ioctl_tiocsctty(io::stdin())?;
    let mut argv = vec!["/bin/sh".into()];
    if let Some(script) = script {
        argv.push(script.into());
    } else {
        argv.push("-i".into());
    }
    if !Path::new(&argv[0]).is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the workload image must provide /bin/sh for guest debugging",
        ));
    }
    let mut command = crate::process::terminal_command(spec, &argv)?;
    command.env("TERM", "xterm-256color");
    Err(command.exec())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_terminal_child_does_not_inherit_ignored_interrupts() {
        // SAFETY: SIGINT is valid and SIG_IGN installs no handler or references.
        let previous = unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
        assert_ne!(previous, libc::SIG_ERR);
        reset_terminal_signals().unwrap();
        // SAFETY: previous is the valid disposition returned for this process.
        let replaced = unsafe { libc::signal(libc::SIGINT, previous) };
        assert_eq!(replaced, libc::SIG_DFL);
    }

    #[test]
    fn commands_are_acknowledged_once_and_buffers_are_bounded() {
        let mut terminal = Terminal::default();
        let append = Message {
            id: 1,
            operation: Operation::Append,
            data: b"echo guest".to_vec(),
        };
        terminal.apply(append.clone()).unwrap();
        terminal.apply(append).unwrap();
        assert_eq!(terminal.script, b"echo guest");
        assert_eq!(terminal.status().acknowledged, 1);
        assert!(
            terminal
                .apply(Message {
                    id: 2,
                    operation: Operation::Append,
                    data: vec![0; MAX_SCRIPT]
                })
                .is_err()
        );
        assert_eq!(terminal.status().acknowledged, 1);
        assert!(
            terminal
                .apply(Message {
                    id: 2,
                    operation: Operation::Input,
                    data: b"ignored".to_vec()
                })
                .is_err()
        );
        assert!(
            terminal
                .apply(Message {
                    id: 2,
                    operation: Operation::Resize,
                    data: vec![0; 3]
                })
                .is_err()
        );
    }
}
