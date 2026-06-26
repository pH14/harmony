// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end web-server test over an in-process loopback (no KVM).
//!
//! Binds `127.0.0.1:0`, drives a scripted event vector through a `LiveSink`, and
//! drives the replay path off a recorded NDJSON file — asserting the served HTML,
//! the `data: …\n\n` SSE framing (in order), and the byte-exact `/recording`
//! body. This is the gate's "tested over an in-process loopback" requirement at
//! integration scope, using only the public API.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use telemetry::{
    Event, EventKind, LiveSink, Mode, NdjsonRecorder, Observer, ServerOptions, serve, to_ndjson,
};

fn loopback() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

/// Reads from a stream until `needle` appears or attempts run out (bounded, so a
/// test never hangs).
fn read_until(stream: &mut TcpStream, needle: &str) -> String {
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .expect("set timeout");
    let mut acc = Vec::new();
    let mut buf = [0u8; 8192];
    for _ in 0..100 {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                acc.extend_from_slice(&buf[..n]);
                if String::from_utf8_lossy(&acc).contains(needle) {
                    break;
                }
            }
            Err(ref e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&acc).into_owned()
}

fn get(addr: SocketAddr, path: &str) -> TcpStream {
    let mut c = TcpStream::connect(addr).expect("connect");
    c.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
        .expect("request");
    c
}

#[test]
fn serves_html_and_streams_a_scripted_run_in_order() {
    let mut live = LiveSink::new(256);
    let server = serve(loopback(), live.clone(), ServerOptions::default()).expect("serve");
    let addr = server.local_addr();

    // GET / → the embedded UI.
    let mut root = get(addr, "/");
    let html = read_until(&mut root, "</html>");
    assert!(html.starts_with("HTTP/1.1 200"));
    assert!(html.contains("text/html"));
    assert!(html.contains("telemetry"));

    // Open the SSE stream, then script a run through the LiveSink.
    let mut sse = get(addr, "/events");
    let head = read_until(&mut sse, "text/event-stream");
    assert!(head.contains("text/event-stream"));

    let script = vec![
        Event::new(
            1,
            10,
            5,
            EventKind::Console {
                text: "PostgreSQL init\n".to_string(),
            },
        ),
        Event::new(2, 20, 10, EventKind::Inject { vector: 32 }),
        Event::new(
            3,
            30,
            15,
            EventKind::Io {
                port: 0x0CA2,
                size: 4,
                value: 0xDEAD_BEEF,
                write: true,
            },
        ),
        Event::new(
            4,
            40,
            20,
            EventKind::Checkpoint {
                state_hash: [7u8; 32],
            },
        ),
    ];
    for ev in &script {
        live.emit(ev);
    }

    // Each event arrives as a `data: <ndjson>\n\n` frame, in order.
    let body = read_until(&mut sse, "Checkpoint");
    let frames: Vec<&str> = body
        .split("\n\n")
        .filter(|f| f.contains("data: "))
        .collect();
    assert!(frames.len() >= 4, "expected ≥4 SSE frames, got: {body:?}");

    // The data after each `data: ` prefix decodes back to the scripted event.
    let mut decoded = Vec::new();
    for frame in &frames {
        if let Some(idx) = frame.find("data: ") {
            let json = frame[idx + 6..].trim();
            if let Ok(ev) = telemetry::from_ndjson(json) {
                decoded.push(ev);
            }
        }
    }
    assert_eq!(&decoded[..4], &script[..], "SSE relayed the run verbatim");

    server.shutdown();
}

#[test]
fn replay_serves_a_recorded_file_byte_for_byte() {
    // A recording produced by the lossless recorder is the replay source.
    let mut bytes = Vec::new();
    {
        let mut rec = NdjsonRecorder::new(&mut bytes);
        rec.emit(&Event::new(
            1,
            100,
            50,
            EventKind::Console {
                text: "row: (1, 'alice')\n".to_string(),
            },
        ));
        rec.emit(&Event::new(
            2,
            200,
            100,
            EventKind::Terminal {
                reason: "guest halted".to_string(),
            },
        ));
        assert!(rec.error().is_none());
    }

    let dir = tempdir_like();
    let path = dir.join("run.ndjson");
    std::fs::write(&path, &bytes).expect("write recording");

    let live = LiveSink::new(16);
    let opts = ServerOptions {
        recording: Some(path.clone()),
        mode: Mode::Replay,
    };
    let server = serve(loopback(), live, opts).expect("serve");
    let addr = server.local_addr();

    // /config tells the static page it is a replay.
    let mut cfg = get(addr, "/config");
    let cfg_body = read_until(&mut cfg, "}");
    assert!(cfg_body.contains("\"mode\":\"replay\""));
    assert!(cfg_body.contains("\"hasRecording\":true"));

    // /recording streams the exact file bytes after the header.
    let mut rec_conn = get(addr, "/recording");
    let resp = read_until(&mut rec_conn, "guest halted");
    assert!(resp.starts_with("HTTP/1.1 200"));
    assert!(resp.contains("application/x-ndjson"));
    let served_body = resp.split("\r\n\r\n").nth(1).unwrap_or("");
    assert_eq!(
        served_body.as_bytes(),
        &bytes[..],
        "replay body is byte-exact"
    );

    // And it decodes back to the same events the recorder wrote.
    let evs: Vec<Event> = served_body
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| telemetry::from_ndjson(l).expect("decode"))
        .collect();
    assert_eq!(evs.len(), 2);
    assert_eq!(evs[0].vns, 50);
    assert_eq!(to_ndjson(&evs[1]).unwrap(), to_ndjson(&evs[1]).unwrap());

    server.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A scratch directory under the system temp dir, unique per test process. Avoids
/// pulling `tempfile` into the dependency set for a single integration test.
fn tempdir_like() -> std::path::PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("telemetry-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir scratch");
    dir
}
