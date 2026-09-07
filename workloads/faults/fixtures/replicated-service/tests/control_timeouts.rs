// SPDX-License-Identifier: AGPL-3.0-or-later
//! Control replies remain available when a peer or client stops responding.

use std::{
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use fault_replicated_fixture::{CONTROL_TIMEOUT, command_line, request};

struct Primary(Child);

impl Drop for Primary {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn unused_address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

#[test]
fn stalled_replication_and_client_leave_primary_available() {
    let peer = TcpListener::bind("127.0.0.1:0").unwrap();
    let peer_addr = peer.local_addr().unwrap();
    let address = unused_address();
    let mut primary = Primary(
        Command::new(env!("CARGO_BIN_EXE_fault-replica"))
            .args([
                "--role",
                "primary",
                "--listen",
                &address.to_string(),
                "--peer",
                &peer_addr.to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut ready = false;
    for _ in 0..200 {
        if request(address, "PING").is_ok_and(|reply| reply == "PONG") {
            ready = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(ready, "primary did not become ready");

    let (release, wait) = mpsc::channel();
    let peer_thread = thread::spawn(move || {
        let (mut stream, _) = peer.accept().unwrap();
        assert_eq!(command_line(&mut stream).unwrap(), "REPL 1");
        // Keep the peer connected without a response until the primary's
        // shorter replication deadline expires and its control reply arrives.
        wait.recv_timeout(CONTROL_TIMEOUT).unwrap();
    });
    assert_eq!(request(address, "WRITE").unwrap(), "VALUE 1");
    release.send(()).unwrap();
    peer_thread.join().unwrap();

    // An incomplete control request must end only that connection. The next
    // request waits behind it and must still receive a response.
    let silent_client = TcpStream::connect(address).unwrap();
    assert_eq!(request(address, "PING").unwrap(), "PONG");
    assert!(primary.0.try_wait().unwrap().is_none());
    drop(silent_client);
}
