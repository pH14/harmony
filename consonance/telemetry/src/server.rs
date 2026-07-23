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

/// Upper bound on the bytes [`parse_request`] reads for the request line plus
/// headers. A client that never sends the blank-line terminator (or streams
/// endless headers / one giant unterminated line) is rejected after this many
/// bytes rather than driving an unbounded read — a real robustness bound, and
/// the reason the header drain can never loop forever. (Plain literal, not
/// `64 * 1024`, so no arithmetic operator can be mutated to an equivalent bound.)
const MAX_REQUEST_BYTES: u64 = 65_536;

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
                    // `WouldBlock` (no pending connection on the non-blocking
                    // listener) and any transient accept error are handled the
                    // same way — back off one poll interval and try again — so
                    // there is no behavioral distinction to draw between them.
                    Err(_) => {
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
    parse_request(&mut reader)
}

/// Parses the request line (method + path, query stripped) and then drains the
/// remaining headers up to and **including** the blank line, leaving the body
/// (if any) unconsumed. Split out from [`read_request`] over a generic
/// [`BufRead`] so the exact stop point — drain through the blank line, no
/// further — is unit-testable against a `Cursor` with no socket.
///
/// The reader is wrapped in [`Read::take`] at [`MAX_REQUEST_BYTES`], so the total
/// request line + header bytes are bounded: a client that never sends the
/// blank-line terminator hits the bound (the inner `read_line` returns `0`) and
/// is rejected, never read unboundedly. EOF or the bound *before* the blank line
/// is a malformed/oversized request and fails closed — which also means the
/// drain loop can never spin on a terminator condition that is never satisfied.
fn parse_request<R: BufRead>(reader: &mut R) -> io::Result<Request> {
    let mut limited = reader.take(MAX_REQUEST_BYTES);

    let mut line = String::new();
    if limited.read_line(&mut line)? == 0 {
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

    // Drain headers up to the blank line (we need none of them, but a server that
    // closes with the request unread can trigger an RST that truncates the
    // response, so the bytes must be consumed).
    let mut header = String::new();
    loop {
        header.clear();
        let n = limited.read_line(&mut header)?;
        if n == 0 {
            // EOF or the byte bound reached before the blank line: the request is
            // unterminated or oversized. Fail closed instead of looping.
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request headers unterminated or exceed the size bound",
            ));
        }
        if header == "\r\n" || header == "\n" {
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
    // Stream the body straight through; `io::copy` uses a fixed internal buffer,
    // so a large box recording never balloons memory.
    io::copy(&mut file, stream)?;
    stream.flush()
}

/// The SSE handler: **subscribe before announcing the stream**, then forward
/// `data: <ndjson>\n\n` frames until the client disconnects or the server stops.
///
/// The `hub.subscribe()` **must** precede the response header, not follow it. A
/// client treats receipt of the header as "the stream is open" and only then
/// begins emitting events; if the header were flushed first and the subscribe ran
/// second, the pump could [`EventHub::publish`] an event in that window to a
/// subscriber list that does not yet contain this connection — dropping it on the
/// floor (published to nobody) rather than merely delivering it late. Registering
/// first makes "the client can observe the header" imply "the client is
/// subscribed": the flush is a release that the client's header read acquires, so
/// the subscribe happens-before any event the client emits in response. That
/// happens-before is what closes the `streams_events_as_sse_frames` race (see
/// `IMPLEMENTATION.md`). The reorder changes no wire bytes — the header and every
/// frame are byte-identical.
fn serve_events(
    mut stream: TcpStream,
    running: &Arc<AtomicBool>,
    hub: &Arc<EventHub>,
) -> io::Result<()> {
    // Register with the hub before a single response byte is written, so the
    // subscription is live the instant the client can observe the stream.
    let sub = hub.subscribe();
    // Bracket the whole streaming lifetime — including the header write — so
    // the unsubscribe runs on every exit path, even a header-write error.
    let result = (|| -> io::Result<()> {
        let header = "HTTP/1.1 200 OK\r\n\
             Content-Type: text/event-stream\r\n\
             Cache-Control: no-cache\r\n\
             Connection: keep-alive\r\n\
             \r\n";
        stream.write_all(header.as_bytes())?;
        stream.flush()?;
        // Writes should fail fast (not hang) once a client goes away.
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        pump_events_to(&mut stream, running, &sub)
    })();
    hub.unsubscribe(&sub);
    result
}

/// Advances the idle-iteration counter and decides whether a keepalive is due.
/// Returns the next counter value and, when the [`KEEPALIVE_EVERY`] cadence is
/// reached, the SSE comment bytes to send (resetting the counter to 0). Pure, so
/// the off-by-one cadence is unit-testable without waiting real time.
fn advance_idle(idle: u32) -> (u32, Option<&'static [u8]>) {
    let next = idle + 1;
    if next >= KEEPALIVE_EVERY {
        // An SSE comment line: ignored by EventSource, but writing it detects a
        // vanished client and keeps proxies from timing out.
        (0, Some(b": keepalive\n\n"))
    } else {
        (next, None)
    }
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
            let (next, keepalive) = advance_idle(idle);
            idle = next;
            if let Some(msg) = keepalive {
                stream.write_all(msg)?;
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

    /// Scans `buf` for the first complete SSE **data** frame, transparently
    /// skipping any number of leading non-`data:` frames (e.g. a `:
    /// keepalive\n\n` comment) that are already fully terminated. Returns
    /// `None` when no complete data frame is present yet — either every frame
    /// seen so far was a comment, or the buffer ends mid-frame (including
    /// right after the `data: ` marker, with no terminator) — so the caller
    /// keeps accumulating instead of mistaking a partial frame for a complete
    /// one (F1c) or losing bytes that arrive after a terminator split across
    /// reads (F1b): `buf` is never truncated here, only re-scanned from the
    /// start on each call.
    fn extract_data_frame(buf: &[u8]) -> Option<String> {
        let mut start = 0;
        loop {
            let rel = buf[start..].windows(2).position(|w| w == b"\n\n")?;
            let end = start + rel + 2;
            let frame = &buf[start..end];
            if frame.starts_with(b"data: ") {
                return Some(String::from_utf8_lossy(frame).into_owned());
            }
            start = end;
        }
    }

    /// Reads cumulatively from `stream` until a complete `data: …\n\n` SSE
    /// frame arrives, skipping any `: keepalive\n\n` comment frames along the
    /// way. One bounded attempt budget covers the whole wait — bytes
    /// accumulate across attempts rather than being discarded between them —
    /// so a stalled regression fails fast instead of hanging or hot-spinning
    /// (F1a), and on exhausting the budget this panics with the accumulated
    /// bytes so a stuck run is diagnosable (hm-3r2k, hm-38kv).
    fn read_sse_data_frame(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set timeout");
        let mut acc = Vec::new();
        let mut buf = [0u8; 4096];
        for _ in 0..50 {
            if let Some(frame) = extract_data_frame(&acc) {
                return frame;
            }
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => acc.extend_from_slice(&buf[..n]),
                Err(ref e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut => {}
                Err(_) => break,
            }
        }
        extract_data_frame(&acc).unwrap_or_else(|| {
            panic!(
                "timed out waiting for a complete SSE data frame; accumulated: {:?}",
                String::from_utf8_lossy(&acc)
            )
        })
    }

    #[test]
    fn extract_data_frame_finds_an_immediate_frame() {
        assert_eq!(
            extract_data_frame(b"data: hello\n\n").as_deref(),
            Some("data: hello\n\n")
        );
    }

    #[test]
    fn extract_data_frame_skips_a_leading_keepalive() {
        // F2: deterministically exercises the skip-a-comment-frame path — no
        // real server, no keepalive cadence, no wall-clock wait.
        assert_eq!(
            extract_data_frame(b": keepalive\n\ndata: hello\n\n").as_deref(),
            Some("data: hello\n\n")
        );
    }

    #[test]
    fn extract_data_frame_skips_several_leading_keepalives() {
        assert_eq!(
            extract_data_frame(b": keepalive\n\n: keepalive\n\n: keepalive\n\ndata: x\n\n")
                .as_deref(),
            Some("data: x\n\n")
        );
    }

    #[test]
    fn extract_data_frame_waits_on_an_unterminated_marker() {
        // F1c: the `data: ` marker alone must not read as a complete frame.
        assert_eq!(extract_data_frame(b"data: "), None);
        assert_eq!(extract_data_frame(b"data: partial"), None);
    }

    #[test]
    fn extract_data_frame_waits_when_only_comments_are_complete() {
        assert_eq!(extract_data_frame(b": keepalive\n\ndata: partial"), None);
    }

    #[test]
    fn extract_data_frame_recombines_a_marker_split_across_reads() {
        // F1b: a read boundary lands as `: keepalive\n\ndat` — the marker is
        // split mid-word. The first accumulation has no complete data frame
        // yet (must return None, not silently drop the `dat` tail); once the
        // rest arrives and is appended (never replacing prior bytes) the full
        // frame is found.
        let mut acc = b": keepalive\n\ndat".to_vec();
        assert_eq!(extract_data_frame(&acc), None);
        acc.extend_from_slice(b"a: hello\n\n");
        assert_eq!(extract_data_frame(&acc).as_deref(), Some("data: hello\n\n"));
    }

    fn ev(seq: u64) -> Event {
        Event::new(seq, seq, seq, EventKind::Inject { vector: 7 })
    }

    #[test]
    fn parse_request_drains_exactly_through_the_blank_line() {
        // Request line + two headers + blank line + a body. `parse_request` must
        // consume up to and including the blank line and leave the body intact.
        let raw = b"GET /foo?x=1 HTTP/1.1\r\nHost: x\r\nAccept: */*\r\n\r\nBODYBYTES";
        let mut cur = std::io::Cursor::new(&raw[..]);
        let req = parse_request(&mut cur).expect("parse");
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/foo", "query string is stripped from the path");

        // The drain loop must stop exactly on the blank line. If the EOF check
        // `n == 0` flips to `!=`, or either `header == "…"` flips to `!=`, the
        // loop breaks one line early and leaves the headers in the body. If the
        // terminator `||` flips to `&&` (the CI timeout mutant), the loop never
        // accepts the blank line, runs to the EOF guard, and returns `Err` — so
        // `.expect("parse")` panics here. Every one of those is caught fast.
        let mut rest = Vec::new();
        cur.read_to_end(&mut rest).expect("read remainder");
        assert_eq!(
            rest, b"BODYBYTES",
            "exactly the blank line was the stop point"
        );
    }

    #[test]
    fn parse_request_rejects_an_empty_stream() {
        // No request line at all → the `read_line == 0` guard must fail closed.
        let mut cur = std::io::Cursor::new(&b""[..]);
        assert!(parse_request(&mut cur).is_err());
    }

    #[test]
    fn parse_request_rejects_unterminated_oversized_headers() {
        // A request whose headers never end with a blank line must be rejected
        // after the size bound — never read unboundedly, and never looped on. The
        // `take(MAX_REQUEST_BYTES)` bound turns this into a fast `Err`.
        let mut raw = b"GET / HTTP/1.1\r\n".to_vec();
        while (raw.len() as u64) <= MAX_REQUEST_BYTES {
            raw.extend_from_slice(b"X-Pad: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n");
        }
        let mut cur = std::io::Cursor::new(raw);
        assert!(
            parse_request(&mut cur).is_err(),
            "unterminated headers past the bound are rejected, not read forever"
        );
    }

    #[test]
    fn advance_idle_increments_then_fires_on_cadence() {
        // Below the threshold: count up by exactly one, no keepalive.
        assert_eq!(advance_idle(0), (1, None));
        assert_eq!(advance_idle(5), (6, None));
        // At the cadence (next == KEEPALIVE_EVERY): reset to 0 and a keepalive is
        // due. Pins both the `+ 1` step and the `>=` comparison.
        assert_eq!(
            advance_idle(KEEPALIVE_EVERY - 1),
            (0, Some(b": keepalive\n\n" as &[u8]))
        );
        // Past the cadence (next > KEEPALIVE_EVERY): still due — distinguishes
        // `>=` from `==`.
        assert_eq!(
            advance_idle(KEEPALIVE_EVERY),
            (0, Some(b": keepalive\n\n" as &[u8]))
        );
    }

    #[test]
    fn hub_fans_out_and_unsubscribe_stops_delivery() {
        let hub = EventHub::default();
        let s1 = hub.subscribe();
        let s2 = hub.subscribe();

        // publish must push the event onto every subscriber's queue.
        hub.publish(&ev(1));
        assert_eq!(
            s1.drain().len(),
            1,
            "subscriber 1 receives the published event"
        );
        assert_eq!(s2.drain().len(), 1, "subscriber 2 receives it too");
        // drain emptied the queues.
        assert_eq!(s1.drain().len(), 0);

        // After unsubscribe, s1 must stop receiving while s2 keeps receiving —
        // this pins both `unsubscribe` doing something and the `!Arc::ptr_eq`
        // (remove the matched one, keep the rest, not the inverse).
        hub.unsubscribe(&s1);
        hub.publish(&ev(2));
        assert_eq!(
            s1.drain().len(),
            0,
            "the unsubscribed client receives nothing"
        );
        assert_eq!(
            s2.drain().len(),
            1,
            "the still-subscribed client still receives"
        );
    }

    #[test]
    fn dropping_the_server_stops_the_listener() {
        let addr = {
            let live = LiveSink::new(8);
            let server = serve(loopback(), live, ServerOptions::default()).expect("serve");
            let a = server.local_addr();
            // While alive, the port accepts connections.
            assert!(TcpStream::connect(a).is_ok(), "an alive server accepts");
            a
        }; // server dropped here → Drop → stop() → accept thread joined, port closed.

        // A dropped/stopped server must stop accepting — `stop`/`drop` becoming a
        // no-op would leave the accept thread (and the listener) alive.
        assert!(
            TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_err(),
            "a dropped server stops accepting connections"
        );
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
        // Anchor phase 2 on the real event payload marker: a periodic
        // `: keepalive\n\n` comment frame also satisfies a bare "\n\n" wait, so
        // this is one bounded, cumulative wait for a complete `data: …\n\n`
        // frame — skipping any keepalive frames along the way — rather than a
        // retry that discards bytes between attempts (hm-3r2k, hm-38kv).
        let frame = read_sse_data_frame(&mut c);
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
