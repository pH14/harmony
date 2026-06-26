// SPDX-License-Identifier: AGPL-3.0-or-later
//! The std-only web console server.
//!
//! No async runtime, no framework, no npm, no build step: a
//! [`std::net::TcpListener`] with a thread per connection. Routes:
//!
//! - `GET /` — the embedded vanilla-JS UI ([`INDEX_HTML`], `include_str!`'d).
//! - `GET /events` — **Server-Sent Events**: each event drained from the
//!   [`LiveSink`] is forwarded as `data: <ndjson>\n\n`. The browser reads it with
//!   `new EventSource('/events')`.
//! - `GET /recording` — streams a persisted [`crate::NdjsonRecorder`] file for
//!   **replay** (the page `fetch`es it and scrubs the V-time timeline entirely
//!   client-side, same renderer as live).
//! - `GET /config` — a one-line JSON `{mode, hasRecording}` so the static page
//!   knows whether to open the live stream or load the recording.
//!
//! A single **pump** thread drains the `LiveSink` and fans each event out to the
//! per-connection SSE queues (an [`EventHub`]); a slow browser drops its own
//! oldest buffered events rather than stalling the pump. The server uses **no
//! wall-clock** (V-time stamps come from the events; the wall-clock readout is
//! drawn browser-side), so it trips none of the determinism lints.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::event::{Event, to_ndjson};
use crate::sink::LiveSink;

/// The embedded UI, compiled in at build time — no CDN, no npm, works offline.
pub const INDEX_HTML: &str = include_str!("../assets/index.html");

/// Per-connection SSE backlog cap: a browser that falls this far behind drops
/// its own oldest buffered events (it is tailing a live stream; the newest
/// matters). The lossless history lives in the recording, not here.
const SSE_CLIENT_BACKLOG: usize = 16384;

/// Poll cadence for the accept loop, the pump, and idle SSE writers. Small
/// enough for a live feel, large enough not to busy-spin.
const POLL: Duration = Duration::from_millis(5);

/// Idle SSE writer iterations between `: keepalive` comments (≈ every 3 s at the
/// [`POLL`] cadence) — keeps proxies open and surfaces a dead client promptly.
const KEEPALIVE_EVERY: u32 = 600;

/// Which source the page should render: a live stream or a recorded file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Live: the page opens `EventSource('/events')`.
    Live,
    /// Replay: the page `fetch`es `/recording` and scrubs it client-side.
    Replay,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Live => "live",
            Mode::Replay => "replay",
        }
    }
}

/// Server configuration.
#[derive(Clone, Debug)]
pub struct ServerOptions {
    /// File served at `/recording` (the replay source of truth). `None` ⇒ 404.
    pub recording: Option<PathBuf>,
    /// The mode advertised at `/config`.
    pub mode: Mode,
}

impl Default for ServerOptions {
    fn default() -> Self {
        ServerOptions {
            recording: None,
            mode: Mode::Live,
        }
    }
}

/// A bounded SSE backlog for one connected client.
#[derive(Debug)]
struct Subscriber {
    queue: Mutex<VecDeque<Event>>,
}

impl Subscriber {
    fn push(&self, ev: &Event) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if q.len() >= SSE_CLIENT_BACKLOG {
            q.pop_front(); // drop this client's oldest; keep it tailing the live edge
        }
        q.push_back(ev.clone());
    }

    fn drain(&self) -> Vec<Event> {
        self.queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }
}

/// Fan-out from the single pump to every connected SSE client.
#[derive(Debug, Default)]
struct EventHub {
    subs: Mutex<Vec<Arc<Subscriber>>>,
}

impl EventHub {
    fn publish(&self, ev: &Event) {
        let subs = self.subs.lock().unwrap_or_else(|e| e.into_inner());
        for s in subs.iter() {
            s.push(ev);
        }
    }

    fn subscribe(&self) -> Arc<Subscriber> {
        let s = Arc::new(Subscriber {
            queue: Mutex::new(VecDeque::new()),
        });
        self.subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::clone(&s));
        s
    }

    fn unsubscribe(&self, s: &Arc<Subscriber>) {
        self.subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|x| !Arc::ptr_eq(x, s));
    }
}

/// A running console server. Drop or [`RunningServer::shutdown`] to stop it.
#[derive(Debug)]
pub struct RunningServer {
    local_addr: SocketAddr,
    running: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    pump: Option<JoinHandle<()>>,
}

impl RunningServer {
    /// The actually-bound address (resolves a `:0` port for tests).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stops the accept loop and the pump, joining both. Connection threads
    /// observe the flag and exit on their own within one [`POLL`].
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
        if let Some(h) = self.pump.take() {
            let _ = h.join();
        }
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Binds `addr`, starts the pump and accept loops, and returns the running
/// server (with its resolved [`RunningServer::local_addr`]).
///
/// # Errors
///
/// Propagates the [`TcpListener::bind`] / non-blocking-mode I/O error.
pub fn serve(addr: SocketAddr, live: LiveSink, opts: ServerOptions) -> io::Result<RunningServer> {
    let listener = TcpListener::bind(addr)?;
    let local_addr = listener.local_addr()?;
    listener.set_nonblocking(true)?;

    let running = Arc::new(AtomicBool::new(true));
    let hub = Arc::new(EventHub::default());
    let opts = Arc::new(opts);

    // Pump: drain the lossy live lane and fan it out to SSE clients.
    let pump = {
        let running = Arc::clone(&running);
        let hub = Arc::clone(&hub);
        thread::spawn(move || {
            while running.load(Ordering::SeqCst) {
                let batch = live.drain();
                if batch.is_empty() {
                    thread::sleep(POLL);
                    continue;
                }
                for ev in &batch {
                    hub.publish(ev);
                }
            }
        })
    };

    // Accept: one detached thread per connection.
    let accept = {
        let running = Arc::clone(&running);
        thread::spawn(move || {
            while running.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _peer)) => {
                        let running = Arc::clone(&running);
                        let hub = Arc::clone(&hub);
                        let opts = Arc::clone(&opts);
                        thread::spawn(move || {
                            let _ = handle_conn(stream, &running, &hub, &opts);
                        });
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(POLL);
                    }
                    Err(_) => {
                        // Transient accept error; back off and keep serving.
                        thread::sleep(POLL);
                    }
                }
            }
        })
    };

    Ok(RunningServer {
        local_addr,
        running,
        accept: Some(accept),
        pump: Some(pump),
    })
}

/// Parsed first line of an HTTP request — all this server needs.
struct Request {
    method: String,
    path: String,
}

/// Reads and parses the request line, then drains the remaining headers.
fn read_request(stream: &TcpStream) -> io::Result<Request> {
    // The listener is non-blocking; some platforms (macOS/BSD) let an accepted
    // socket inherit that, which would make a read racing ahead of the client's
    // request bytes return `WouldBlock` and abort the connection. Force blocking
    // mode so reads honor the timeout below uniformly on Linux and macOS.
    stream.set_nonblocking(false)?;
    // A header read should not hang a thread forever on a stalled client.
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream);

    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "empty request",
        ));
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let raw_path = parts.next().unwrap_or("/").to_string();
    // Strip any query string; this server routes on the path only.
    let path = raw_path.split('?').next().unwrap_or("/").to_string();

    // Drain headers up to the blank line (we need none of them).
    let mut header = String::new();
    loop {
        header.clear();
        let n = reader.read_line(&mut header)?;
        if n == 0 || header == "\r\n" || header == "\n" {
            break;
        }
    }
    Ok(Request { method, path })
}

fn handle_conn(
    mut stream: TcpStream,
    running: &Arc<AtomicBool>,
    hub: &Arc<EventHub>,
    opts: &Arc<ServerOptions>,
) -> io::Result<()> {
    let req = read_request(&stream)?;
    if req.method != "GET" {
        return write_simple(&mut stream, 405, "Method Not Allowed", "text/plain", b"405");
    }
    match req.path.as_str() {
        "/" => write_simple(
            &mut stream,
            200,
            "OK",
            "text/html; charset=utf-8",
            INDEX_HTML.as_bytes(),
        ),
        "/config" => {
            let body = format!(
                "{{\"mode\":\"{}\",\"hasRecording\":{}}}",
                opts.mode.as_str(),
                opts.recording.is_some()
            );
            write_simple(&mut stream, 200, "OK", "application/json", body.as_bytes())
        }
        "/recording" => serve_recording(&mut stream, opts),
        "/events" => serve_events(stream, running, hub),
        _ => write_simple(&mut stream, 404, "Not Found", "text/plain", b"404"),
    }
}

/// Writes a complete, `Content-Length`-framed response.
fn write_simple(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-cache\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// Streams the configured recording file for replay, or 404 if none.
fn serve_recording(stream: &mut TcpStream, opts: &Arc<ServerOptions>) -> io::Result<()> {
    let Some(path) = opts.recording.as_ref() else {
        return write_simple(stream, 404, "Not Found", "text/plain", b"no recording");
    };
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => {
            return write_simple(
                stream,
                404,
                "Not Found",
                "text/plain",
                b"recording unavailable",
            );
        }
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let header = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/x-ndjson\r\n\
         Content-Length: {len}\r\n\
         Cache-Control: no-cache\r\n\
         Connection: close\r\n\
         \r\n"
    );
    stream.write_all(header.as_bytes())?;
    // Stream in chunks so a large box recording never balloons memory.
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        stream.write_all(&buf[..n])?;
    }
    stream.flush()
}

/// The SSE handler: subscribe, then forward `data: <ndjson>\n\n` frames until the
/// client disconnects or the server stops.
fn serve_events(
    mut stream: TcpStream,
    running: &Arc<AtomicBool>,
    hub: &Arc<EventHub>,
) -> io::Result<()> {
    let header = "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Cache-Control: no-cache\r\n\
         Connection: keep-alive\r\n\
         \r\n";
    stream.write_all(header.as_bytes())?;
    stream.flush()?;
    // Writes should fail fast (not hang) once a client goes away.
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    let sub = hub.subscribe();
    let result = pump_events_to(&mut stream, running, &sub);
    hub.unsubscribe(&sub);
    result
}

fn pump_events_to(
    stream: &mut TcpStream,
    running: &Arc<AtomicBool>,
    sub: &Arc<Subscriber>,
) -> io::Result<()> {
    let mut idle: u32 = 0;
    while running.load(Ordering::SeqCst) {
        let batch = sub.drain();
        if batch.is_empty() {
            idle += 1;
            if idle >= KEEPALIVE_EVERY {
                idle = 0;
                // An SSE comment line: ignored by EventSource, but a write here
                // detects a vanished client and keeps proxies from timing out.
                stream.write_all(b": keepalive\n\n")?;
                stream.flush()?;
            }
            thread::sleep(POLL);
            continue;
        }
        idle = 0;
        for ev in &batch {
            // `to_ndjson` is infallible for a well-formed Event; treat a wire
            // error as a broken connection rather than panicking.
            let line = to_ndjson(ev).map_err(io::Error::other)?;
            stream.write_all(b"data: ")?;
            stream.write_all(line.as_bytes())?;
            stream.write_all(b"\n\n")?;
        }
        stream.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventKind;
    use crate::observer::Observer;
    use std::net::TcpStream;

    fn loopback() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 0))
    }

    /// Reads from a stream until `needle` appears or it would block, with a
    /// bounded number of attempts (keeps the unit test from hanging).
    fn read_until(stream: &mut TcpStream, needle: &str) -> String {
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set timeout");
        let mut acc = Vec::new();
        let mut buf = [0u8; 4096];
        for _ in 0..50 {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    acc.extend_from_slice(&buf[..n]);
                    if String::from_utf8_lossy(&acc).contains(needle) {
                        break;
                    }
                }
                Err(ref e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut => {}
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&acc).into_owned()
    }

    #[test]
    fn serves_the_embedded_html_at_root() {
        let live = LiveSink::new(64);
        let server = serve(loopback(), live, ServerOptions::default()).expect("serve");
        let mut c = TcpStream::connect(server.local_addr()).expect("connect");
        c.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .expect("req");
        let resp = read_until(&mut c, "</html>");
        assert!(resp.starts_with("HTTP/1.1 200"));
        assert!(resp.contains("text/html"));
        assert!(resp.contains("EventSource"), "UI wires up SSE");
        server.shutdown();
    }

    #[test]
    fn streams_events_as_sse_frames() {
        let mut live = LiveSink::new(64);
        let server = serve(loopback(), live.clone(), ServerOptions::default()).expect("serve");

        let mut c = TcpStream::connect(server.local_addr()).expect("connect");
        c.write_all(b"GET /events HTTP/1.1\r\nHost: x\r\n\r\n")
            .expect("req");
        // Read past the SSE response header first.
        let head = read_until(&mut c, "text/event-stream");
        assert!(head.contains("text/event-stream"));
        assert!(head.contains("no-cache"));

        // Now drive a scripted event through the LiveSink; the pump forwards it.
        live.emit(&Event::new(
            1,
            10,
            5,
            EventKind::Console {
                text: "hello".to_string(),
            },
        ));
        let frame = read_until(&mut c, "\n\n");
        assert!(frame.contains("data: "), "SSE data prefix: {frame:?}");
        assert!(frame.contains("\"Console\""));
        assert!(frame.contains("hello"));
        assert!(frame.contains("\n\n"), "SSE frame terminator");
        server.shutdown();
    }

    #[test]
    fn config_reports_mode_and_recording() {
        let live = LiveSink::new(8);
        let opts = ServerOptions {
            recording: None,
            mode: Mode::Replay,
        };
        let server = serve(loopback(), live, opts).expect("serve");
        let mut c = TcpStream::connect(server.local_addr()).expect("connect");
        c.write_all(b"GET /config HTTP/1.1\r\nHost: x\r\n\r\n")
            .expect("req");
        let resp = read_until(&mut c, "}");
        assert!(resp.contains("\"mode\":\"replay\""));
        assert!(resp.contains("\"hasRecording\":false"));
        server.shutdown();
    }

    #[test]
    fn recording_404s_when_absent() {
        let live = LiveSink::new(8);
        let server = serve(loopback(), live, ServerOptions::default()).expect("serve");
        let mut c = TcpStream::connect(server.local_addr()).expect("connect");
        c.write_all(b"GET /recording HTTP/1.1\r\nHost: x\r\n\r\n")
            .expect("req");
        let resp = read_until(&mut c, "404");
        assert!(resp.starts_with("HTTP/1.1 404"));
        server.shutdown();
    }
}
