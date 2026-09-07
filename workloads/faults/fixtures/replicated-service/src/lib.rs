// SPDX-License-Identifier: AGPL-3.0-or-later
//! Protocol helpers shared by the fixture's replica and check commands.

use std::{
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    path::Path,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

pub const IO_TIMEOUT: Duration = Duration::from_millis(100);
/// Control callers allow the primary to finish a failed replication attempt
/// and report the accepted write. Equal deadlines race that response and can
/// turn the intended stale-read assertion into a control timeout.
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);

pub fn parse_addr(value: &str) -> Result<SocketAddr, String> {
    value
        .parse()
        .map_err(|error| format!("invalid socket address {value:?}: {error}"))
}

pub fn request(address: SocketAddr, command: &str) -> Result<String, String> {
    request_with_timeout(address, command, CONTROL_TIMEOUT)
}

pub fn replication_request(address: SocketAddr, command: &str) -> Result<String, String> {
    request_with_timeout(address, command, IO_TIMEOUT)
}

fn request_with_timeout(
    address: SocketAddr,
    command: &str,
    timeout: Duration,
) -> Result<String, String> {
    let mut stream = TcpStream::connect_timeout(&address, timeout)
        .map_err(|error| format!("connect {address}: {error}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| format!("set read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| format!("set write timeout: {error}"))?;
    stream
        .write_all(command.as_bytes())
        .map_err(|error| format!("write {address}: {error}"))?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| format!("finish request {address}: {error}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| format!("read {address}: {error}"))?;
    Ok(response.trim().to_owned())
}

/// Read-only control requests use a Unix socket so an assertion can inspect
/// the replica while the primary-to-replica data path is partitioned.
#[cfg(unix)]
pub fn unix_request(path: &Path, command: &str) -> Result<String, String> {
    let mut stream = UnixStream::connect(path)
        .map_err(|error| format!("connect {}: {error}", path.display()))?;
    stream
        .set_read_timeout(Some(CONTROL_TIMEOUT))
        .map_err(|error| format!("set read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(CONTROL_TIMEOUT))
        .map_err(|error| format!("set write timeout: {error}"))?;
    stream
        .write_all(command.as_bytes())
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|error| format!("finish request {}: {error}", path.display()))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    Ok(response.trim().to_owned())
}

pub fn parse_value(response: &str) -> Result<u64, String> {
    response
        .strip_prefix("VALUE ")
        .ok_or_else(|| format!("fixture response is not a value: {response:?}"))?
        .parse()
        .map_err(|error| format!("invalid fixture value {response:?}: {error}"))
}

/// Application-level replication state reported by the replica control
/// socket. `pending` is acknowledged by the replica but has not become
/// visible to GET; it is the state captured by the pending-work fixture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplicaStatus {
    pub value: u64,
    pub pending: Option<u64>,
}

pub fn parse_status(response: &str) -> Result<ReplicaStatus, String> {
    let mut fields = response.split_whitespace();
    if fields.next() != Some("STATUS") {
        return Err(format!("fixture response is not status: {response:?}"));
    }
    let value = fields
        .next()
        .and_then(|field| field.strip_prefix("VALUE="))
        .ok_or_else(|| format!("status has no value: {response:?}"))?
        .parse()
        .map_err(|error| format!("invalid status value {response:?}: {error}"))?;
    let pending = fields
        .next()
        .and_then(|field| field.strip_prefix("PENDING="))
        .ok_or_else(|| format!("status has no pending field: {response:?}"))?;
    let pending = if pending == "NONE" {
        None
    } else {
        Some(
            pending
                .parse()
                .map_err(|error| format!("invalid pending value {response:?}: {error}"))?,
        )
    };
    if fields.next().is_some() {
        return Err(format!("status has unexpected fields: {response:?}"));
    }
    Ok(ReplicaStatus { value, pending })
}

pub fn command_line(mut stream: impl Read) -> io::Result<String> {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
}

pub fn reply(mut stream: impl Write, message: &str) -> io::Result<()> {
    stream.write_all(message.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::parse_status;

    #[test]
    fn status_round_trips_visible_and_pending_values() {
        assert_eq!(
            parse_status("STATUS VALUE=7 PENDING=9").unwrap(),
            super::ReplicaStatus {
                value: 7,
                pending: Some(9),
            }
        );
        assert_eq!(
            parse_status("STATUS VALUE=9 PENDING=NONE").unwrap(),
            super::ReplicaStatus {
                value: 9,
                pending: None,
            }
        );
    }

    #[test]
    fn status_rejects_missing_or_extra_fields() {
        assert!(parse_status("STATUS VALUE=7").is_err());
        assert!(parse_status("STATUS VALUE=7 PENDING=NONE EXTRA").is_err());
        assert!(parse_status("VALUE=7 PENDING=NONE").is_err());
    }
}
