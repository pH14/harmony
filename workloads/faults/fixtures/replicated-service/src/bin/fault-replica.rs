// SPDX-License-Identifier: AGPL-3.0-or-later
//! One deliberately small primary or replica process.

use std::{
    fs,
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
};

use std::os::unix::net::{UnixListener, UnixStream};

use fault_replicated_fixture::{IO_TIMEOUT, command_line, parse_addr, replication_request, reply};

#[derive(Default)]
struct ReplicaState {
    value: u64,
    pending: Option<u64>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("fault-replica: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    let role = argument(&args, "--role")?;
    let listen = parse_addr(&argument(&args, "--listen")?)?;
    let peer = argument(&args, "--peer")
        .ok()
        .map(|value| parse_addr(&value))
        .transpose()?;
    let control_socket = argument(&args, "--control-socket").ok().map(PathBuf::from);
    let primary = role == "primary";
    if !primary && role != "replica" {
        return Err(format!("role must be primary or replica, got {role:?}"));
    }
    let listener = TcpListener::bind(listen).map_err(|error| format!("bind {listen}: {error}"))?;
    let value = Arc::new(Mutex::new(ReplicaState::default()));
    if let Some(path) = control_socket {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("remove control socket {}: {error}", path.display())),
        }
        let control = UnixListener::bind(&path)
            .map_err(|error| format!("bind control socket {}: {error}", path.display()))?;
        let value = Arc::clone(&value);
        thread::Builder::new()
            .name("fault-replica-control".into())
            .spawn(move || {
                for stream in control.incoming() {
                    match stream {
                        Ok(stream) => {
                            if let Err(error) = handle_control(stream, &value) {
                                eprintln!("fault-replica control: {error}");
                            }
                        }
                        Err(error) => {
                            eprintln!("fault-replica control accept: {error}");
                            break;
                        }
                    }
                }
            })
            .map_err(|error| format!("start control thread: {error}"))?;
    }
    for stream in listener.incoming() {
        let stream = stream.map_err(|error| format!("accept {listen}: {error}"))?;
        let value = Arc::clone(&value);
        // A stalled or disconnected client only ends its own request. The
        // replica must remain available for the recovery command afterward.
        if let Err(error) = handle(stream, primary, peer, value) {
            eprintln!("fault-replica request: {error}");
        }
    }
    Ok(())
}

fn handle_control(mut stream: UnixStream, value: &Arc<Mutex<ReplicaState>>) -> Result<(), String> {
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|error| format!("set control read timeout: {error}"))?;
    let command =
        command_line(&mut stream).map_err(|error| format!("read control command: {error}"))?;
    let response = match command.as_str() {
        "GET" => format!(
            "VALUE {}",
            value.lock().map_err(|_| "state poisoned")?.value
        ),
        "STATUS" => status(value)?,
        "PING" => "PONG".to_owned(),
        _ => "ERROR unsupported-command".to_owned(),
    };
    reply(stream, &response).map_err(|error| format!("write control response: {error}"))
}

fn handle(
    mut stream: TcpStream,
    primary: bool,
    peer: Option<std::net::SocketAddr>,
    value: Arc<Mutex<ReplicaState>>,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|error| format!("set read timeout: {error}"))?;
    let command = command_line(&mut stream).map_err(|error| format!("read command: {error}"))?;
    let response = match command.as_str() {
        "GET" => format!(
            "VALUE {}",
            value.lock().map_err(|_| "state poisoned")?.value
        ),
        "WRITE" if primary => {
            let next = {
                let mut current = value.lock().map_err(|_| "state poisoned")?;
                current.value = current.value.saturating_add(1);
                current.value
            };
            if let Some(peer) = peer {
                let _ = replication_request(peer, &format!("REPL {next}"));
            }
            format!("VALUE {next}")
        }
        "PENDING" if primary => {
            let next = {
                let mut current = value.lock().map_err(|_| "state poisoned")?;
                current.value = current.value.saturating_add(1);
                current.value
            };
            let Some(peer) = peer else {
                return Err("primary has no peer".into());
            };
            match replication_request(peer, &format!("REPL_PENDING {next}")) {
                Ok(response) if response == "QUEUED" => format!("PENDING {next}"),
                Ok(response) => format!("ERROR {response}"),
                Err(error) => format!("ERROR {error}"),
            }
        }
        "SYNC" if primary => {
            let current = value.lock().map_err(|_| "state poisoned")?.value;
            let Some(peer) = peer else {
                return Err("primary has no peer".into());
            };
            match replication_request(peer, &format!("REPL {current}")) {
                Ok(response) if response == "OK" => "SYNCED".to_owned(),
                Ok(response) => format!("ERROR {response}"),
                Err(error) => format!("ERROR {error}"),
            }
        }
        command if !primary && command.strip_prefix("REPL_PENDING ").is_some() => {
            let next = command
                .strip_prefix("REPL_PENDING ")
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| format!("invalid pending replication command {command:?}"))?;
            let mut current = value.lock().map_err(|_| "state poisoned")?;
            current.pending = Some(current.pending.unwrap_or(0).max(next));
            "QUEUED".to_owned()
        }
        command if !primary && command.strip_prefix("REPL ").is_some() => {
            let next = command
                .strip_prefix("REPL ")
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| format!("invalid replication command {command:?}"))?;
            let mut current = value.lock().map_err(|_| "state poisoned")?;
            current.value = current.value.max(next);
            if current
                .pending
                .is_some_and(|pending| pending <= current.value)
            {
                current.pending = None;
            }
            "OK".to_owned()
        }
        "PING" => "PONG".to_owned(),
        _ => "ERROR unsupported-command".to_owned(),
    };
    reply(stream, &response).map_err(|error| format!("write response: {error}"))
}

fn status(value: &Arc<Mutex<ReplicaState>>) -> Result<String, String> {
    let state = value.lock().map_err(|_| "state poisoned")?;
    Ok(format!(
        "STATUS VALUE={} PENDING={}",
        state.value,
        state
            .pending
            .map_or_else(|| "NONE".to_owned(), |pending| pending.to_string())
    ))
}

fn argument(args: &[String], flag: &str) -> Result<String, String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .ok_or_else(|| format!("missing {flag}"))
}
