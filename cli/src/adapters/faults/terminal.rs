// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::config::Result;
use faults_workload::consonance::FaultTarget;
use process_proto::debug::{MAX_DATA, MAX_SCRIPT, Message, Operation, Status};
use std::{
    fs::File,
    io::{self, IsTerminal, Read, Write},
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};
struct InputMode(Option<rustix::termios::Termios>);
impl InputMode {
    fn enter() -> Result<Self> {
        let original = if io::stdin().is_terminal() {
            let original = rustix::termios::tcgetattr(io::stdin())?;
            let mut raw = original.clone();
            raw.make_raw();
            rustix::termios::tcsetattr(io::stdin(), rustix::termios::OptionalActions::Now, &raw)?;
            Some(original)
        } else {
            None
        };
        Ok(Self(original))
    }
}
impl Drop for InputMode {
    fn drop(&mut self) {
        if let Some(original) = &self.0 {
            let _ = rustix::termios::tcsetattr(
                io::stdin(),
                rustix::termios::OptionalActions::Now,
                original,
            );
        }
    }
}
struct Connection<'a> {
    target: &'a mut FaultTarget,
    cursor: usize,
    sequence: u64,
    status: Option<Status>,
    transcript: File,
    inputs: File,
}
impl<'a> Connection<'a> {
    fn new(target: &'a mut FaultTarget, out: &Path) -> Result<Self> {
        let (cursor, sequence) = target.debug_position()?;
        Ok(Self {
            target,
            cursor,
            sequence,
            status: None,
            transcript: File::create(out.join("terminal.log"))?,
            inputs: File::create(out.join("terminal-input.jsonl"))?,
        })
    }
    fn step(&mut self, message: Option<&Message>) -> Result<()> {
        let progress = self.target.debug_step(message, &mut self.cursor)?;
        if let Some(status) = progress.status {
            self.status = Some(status);
        }
        self.transcript.write_all(&progress.output)?;
        self.transcript.flush()?;
        io::stdout().write_all(&progress.output)?;
        io::stdout().flush()?;
        Ok(())
    }
    #[allow(clippy::disallowed_methods)]
    fn send(&mut self, operation: Operation, data: Vec<u8>) -> Result<()> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("terminal sequence overflow")?;
        let message = Message {
            id: self.sequence,
            operation,
            data,
        };
        writeln!(
            self.inputs,
            "{}",
            serde_json::json!({"sequence":message.id,"operation":format!("{:?}",operation),"bytes":message.data})
        )?;
        self.inputs.flush()?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.step(Some(&message))?;
            if self.status.is_some_and(|s| s.acknowledged >= message.id) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("guest command channel did not respond; prepare the workload with the current runtime".into());
            }
        }
    }
    #[allow(clippy::disallowed_methods)]
    fn wait(&mut self) -> Result<i32> {
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            if let Some(code) = self
                .status
                .and_then(|s| (!s.running).then_some(s.exit_code).flatten())
            {
                return Ok(code);
            }
            self.step(None)?;
            if Instant::now() >= deadline {
                return Err("guest command exceeded its five-minute host limit".into());
            }
        }
    }
}
pub fn execute(
    target: &mut FaultTarget,
    out: &Path,
    script: Option<&[u8]>,
    interactive: bool,
) -> Result<i32> {
    let mut connection = Connection::new(target, out)?;
    if let Some(script) = script {
        if script.len() > MAX_SCRIPT {
            return Err("guest script exceeds 1 MiB".into());
        }
        for chunk in script.chunks(MAX_DATA) {
            connection.send(Operation::Append, chunk.to_vec())?;
        }
        connection.send(Operation::Execute, Vec::new())?;
        return connection.wait();
    }
    if !interactive {
        return Ok(0);
    }
    let _input_mode = InputMode::enter()?;
    connection.send(Operation::Shell, Vec::new())?;
    if let Ok(size) = rustix::termios::tcgetwinsize(io::stdin()) {
        let mut bytes = size.ws_row.to_le_bytes().to_vec();
        bytes.extend(size.ws_col.to_le_bytes());
        connection.send(Operation::Resize, bytes)?;
    }
    let (sender, receiver) = mpsc::sync_channel(16);
    std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        let mut bytes = [0u8; 1024];
        loop {
            match input.read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => {
                    if sender.send(bytes[..n].to_vec()).is_err() {
                        return;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
    loop {
        if let Some(code) = connection
            .status
            .and_then(|s| (!s.running).then_some(s.exit_code).flatten())
        {
            return Ok(code);
        }
        match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(bytes) => connection.send(Operation::Input, bytes)?,
            Err(mpsc::RecvTimeoutError::Timeout) => connection.step(None)?,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                connection.send(Operation::Close, Vec::new())?;
                return connection.wait();
            }
        }
    }
}
