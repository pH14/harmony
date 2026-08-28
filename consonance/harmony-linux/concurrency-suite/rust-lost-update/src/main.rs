// SPDX-License-Identifier: AGPL-3.0-or-later

//! Instrumented cooperative lost-update payload for M6.

use std::{
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

use hypercall_proto::{Client, Transport};

const RESULT_EVENT: u32 = 0x0600_0001;
const BOOTSTRAP_THREAD: u32 = u32::MAX;

struct StdioTransport;

impl Transport for StdioTransport {
    type Error = io::Error;

    fn exchange(&mut self, request: &[u8], response: &mut [u8]) -> io::Result<usize> {
        let request_len = u32::try_from(request.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "request too large"))?;
        let mut stdout = io::stdout().lock();
        stdout.write_all(&request_len.to_le_bytes())?;
        stdout.write_all(request)?;
        stdout.flush()?;

        let mut len = [0_u8; 4];
        io::stdin().lock().read_exact(&mut len)?;
        let len = usize::try_from(u32::from_le_bytes(len))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "response length"))?;
        if len > response.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response too large",
            ));
        }
        io::stdin().lock().read_exact(&mut response[..len])?;
        Ok(len)
    }
}

#[derive(Clone, Copy)]
struct ActorState {
    step: u8,
    observed: u64,
}

fn runnable(actors: &[ActorState; 2]) -> Vec<usize> {
    actors
        .iter()
        .enumerate()
        .filter_map(|(index, actor)| (actor.step < 2).then_some(index))
        .collect()
}

enum Command {
    Step,
    Stop,
}

struct StepResult {
    actor_id: usize,
    step: u8,
}

fn actor_thread(
    actor_id: usize,
    shared: Arc<AtomicU64>,
    commands: mpsc::Receiver<Command>,
    completed: mpsc::Sender<StepResult>,
) -> io::Result<()> {
    let mut local = 0_u64;
    let mut step = 0_u8;
    loop {
        match commands
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "scheduler stopped"))?
        {
            Command::Step => {
                match step {
                    0 => local = shared.load(Ordering::SeqCst),
                    1 => shared.store(local + 1, Ordering::SeqCst),
                    _ => return Err(io::Error::other("actor stepped after completion")),
                }
                step += 1;
                completed
                    .send(StepResult { actor_id, step })
                    .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "scheduler stopped"))?;
            }
            Command::Stop => return Ok(()),
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(StdioTransport);
    let mut actors = [
        ActorState {
            step: 0,
            observed: 0,
        },
        ActorState {
            step: 0,
            observed: 0,
        },
    ];
    let shared = Arc::new(AtomicU64::new(0));
    let (completed_tx, completed_rx) = mpsc::channel();
    let mut command_txs = Vec::with_capacity(2);
    let mut handles = Vec::with_capacity(2);
    for actor_id in 0..2 {
        let (command_tx, command_rx) = mpsc::channel();
        command_txs.push(command_tx);
        let actor_shared = Arc::clone(&shared);
        let actor_completed = completed_tx.clone();
        handles.push(thread::spawn(move || {
            actor_thread(actor_id, actor_shared, command_rx, actor_completed)
        }));
    }
    drop(completed_tx);

    let (_, first) = client
        .coverage_yield(BOOTSTRAP_THREAD, 1, 2)
        .map_err(|error| io::Error::other(format!("{error}")))?;
    let mut selected = usize::try_from(first)?;
    loop {
        let ready = runnable(&actors);
        let actor_id = *ready
            .get(selected)
            .ok_or("host selected no runnable actor")?;
        command_txs[actor_id]
            .send(Command::Step)
            .map_err(|_| "selected actor stopped before its step")?;
        let completed = completed_rx.recv()?;
        if completed.actor_id != actor_id || completed.step != actors[actor_id].step + 1 {
            return Err("actor completion did not match the selected step".into());
        }
        let actor = &mut actors[actor_id];
        actor.step = completed.step;
        actor.observed += 1;
        let observed = actor.observed;

        let ready = runnable(&actors);
        if ready.is_empty() {
            break;
        }
        let ready_count = u32::try_from(ready.len())?;
        let (_, next) = client
            .coverage_yield(u32::try_from(actor_id)?, observed, ready_count)
            .map_err(|error| io::Error::other(format!("{error}")))?;
        selected = usize::try_from(next)?;
    }

    for command in &command_txs {
        command
            .send(Command::Stop)
            .map_err(|_| "actor stopped before scheduler shutdown")?;
    }
    for handle in handles {
        handle
            .join()
            .map_err(|_| "actor thread panicked")?
            .map_err(|error| format!("actor thread failed: {error}"))?;
    }

    // A lost update leaves one increment where two completed increments are
    // required. Report only protocol data; stdout is reserved for framed I/O.
    let shared = shared.load(Ordering::SeqCst);
    client
        .event_emit(RESULT_EVENT, &[u8::from(shared != 2), shared as u8])
        .map_err(|error| io::Error::other(format!("{error}")))?;
    Ok(())
}
