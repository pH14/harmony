// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recovery command: synchronize the primary's current value, then verify it.

use std::{path::PathBuf, thread, time::Duration};

use fault_replicated_fixture::{parse_addr, parse_status, parse_value, request, unix_request};

const RECOVERY_ATTEMPTS: usize = 16;
const RECOVERY_RETRY_DELAY: Duration = Duration::from_millis(10);

fn main() {
    if let Err(error) = run() {
        eprintln!("fault-recovery: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    let primary = parse_addr(&argument(&args, "--primary")?)?;
    let replica_socket = PathBuf::from(argument(&args, "--replica-socket")?);
    retry_recovery(|| recover_once(primary, &replica_socket), thread::sleep)
}

fn recover_once(
    primary: std::net::SocketAddr,
    replica_socket: &std::path::Path,
) -> Result<(), String> {
    let synced = request(primary, "SYNC\n")?;
    if synced != "SYNCED" {
        return Err(format!("replica synchronization failed: {synced}"));
    }
    let primary_value = parse_value(&request(primary, "GET\n")?)?;
    let replica_status = parse_status(&unix_request(replica_socket, "STATUS\n")?)?;
    if replica_status.pending.is_some() || primary_value != replica_status.value {
        return Err(format!(
            "recovery left undelivered state: primary={primary_value} replica={replica_status:?}"
        ));
    }
    Ok(())
}

fn retry_recovery<Attempt, Sleep>(mut attempt: Attempt, mut sleep: Sleep) -> Result<(), String>
where
    Attempt: FnMut() -> Result<(), String>,
    Sleep: FnMut(Duration),
{
    let mut last_error = String::from("recovery did not run");
    for index in 0..RECOVERY_ATTEMPTS {
        match attempt() {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = error;
                if index + 1 < RECOVERY_ATTEMPTS {
                    sleep(RECOVERY_RETRY_DELAY);
                }
            }
        }
    }
    Err(last_error)
}

fn argument(args: &[String], flag: &str) -> Result<String, String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .ok_or_else(|| format!("missing {flag}"))
}

#[cfg(test)]
mod tests {
    use super::{RECOVERY_ATTEMPTS, RECOVERY_RETRY_DELAY, retry_recovery};
    use std::time::Duration;

    #[test]
    fn retry_recovery_retries_transient_failures_then_verifies_success() {
        let mut attempts = 0;
        let mut sleeps = Vec::new();
        retry_recovery(
            || {
                attempts += 1;
                if attempts < 4 {
                    Err(format!("transient attempt {attempts}"))
                } else {
                    Ok(())
                }
            },
            |delay| sleeps.push(delay),
        )
        .expect("a later verified recovery attempt should succeed");

        assert_eq!(attempts, 4);
        assert_eq!(sleeps, vec![RECOVERY_RETRY_DELAY; 3]);
    }

    #[test]
    fn retry_recovery_is_bounded_and_preserves_the_final_error() {
        let mut attempts = 0;
        let mut sleeps = Vec::new();
        let result = retry_recovery(
            || {
                attempts += 1;
                Err(format!("final failure {attempts}"))
            },
            |delay| sleeps.push(delay),
        );

        assert_eq!(attempts, RECOVERY_ATTEMPTS);
        assert_eq!(
            sleeps,
            vec![Duration::from_millis(10); RECOVERY_ATTEMPTS - 1]
        );
        assert_eq!(
            result.unwrap_err(),
            format!("final failure {RECOVERY_ATTEMPTS}")
        );
    }
}
