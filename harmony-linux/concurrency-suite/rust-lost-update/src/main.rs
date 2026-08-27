// SPDX-License-Identifier: AGPL-3.0-or-later

//! Instrumented cooperative lost-update payload for M6.

use std::io::{self, Read, Write};

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

#[derive(Clone, Copy, Default)]
struct Actor {
    step: u8,
    observed: u64,
}

fn runnable(actors: &[Actor; 2]) -> Vec<usize> {
    actors
        .iter()
        .enumerate()
        .filter_map(|(index, actor)| (actor.step < 2).then_some(index))
        .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(StdioTransport);
    let mut actors = [Actor::default(); 2];
    let mut local = [0_u64; 2];
    let mut shared = 0_u64;

    let (_, first) = client
        .coverage_yield(BOOTSTRAP_THREAD, 1, 2)
        .map_err(|error| io::Error::other(format!("{error}")))?;
    let mut selected = usize::try_from(first)?;
    loop {
        let ready = runnable(&actors);
        let actor_id = *ready
            .get(selected)
            .ok_or("host selected no runnable actor")?;
        let observed = {
            let actor = &mut actors[actor_id];
            match actor.step {
                0 => local[actor_id] = shared,
                1 => shared = local[actor_id] + 1,
                _ => return Err("actor stepped after completion".into()),
            }
            actor.step += 1;
            actor.observed += 1;
            actor.observed
        };

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

    // A lost update leaves one increment where two completed increments are
    // required. Report only protocol data; stdout is reserved for framed I/O.
    client
        .event_emit(RESULT_EVENT, &[u8::from(shared != 2), shared as u8])
        .map_err(|error| io::Error::other(format!("{error}")))?;
    Ok(())
}
