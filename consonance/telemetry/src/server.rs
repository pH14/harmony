// SPDX-License-Identifier: AGPL-3.0-or-later

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

pub const INDEX_HTML: &str = include_str!("../assets/index.html");

const SSE_CLIENT_BACKLOG: usize = 16384;

const POLL: Duration = Duration::from_millis(5);

const KEEPALIVE_EVERY: u32 = 600;

const MAX_REQUEST_BYTES: u64 = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Live,
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

#[derive(Clone, Debug)]
pub struct ServerOptions {
    pub recording: Option<PathBuf>,
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

#[derive(Debug)]
struct Subscriber {
    queue: Mutex<VecDeque<Event>>,
}

impl Subscriber {
    fn push(&self, ev: &Event) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if q.len() >= SSE_CLIENT_BACKLOG {
            q.pop_front();
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

#[derive(Debug)]
pub struct RunningServer {
    local_addr: SocketAddr,
    running: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    pump: Option<JoinHandle<()>>,
}

impl RunningServer {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

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

pub fn serve(addr: SocketAddr, live: LiveSink, opts: ServerOptions) -> io::Result<RunningServer> {
    let listener = TcpListener::bind(addr)?;
    let local_addr = listener.local_addr()?;
    listener.set_nonblocking(true)?;

    let running = Arc::new(AtomicBool::new(true));
    let hub = Arc::new(EventHub::default());
    let opts = Arc::new(opts);

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

struct Request {
    method: String,
    path: String,
}

fn read_request(stream: &TcpStream) -> io::Result<Request> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream);
    parse_request(&mut reader)
}

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
    let path = raw_path.split('?').next().unwrap_or("/").to_string();

    let mut header = String::new();
    loop {
        header.clear();
        let n = limited.read_line(&mut header)?;
        if n == 0 {
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
    io::copy(&mut file, stream)?;
    stream.flush()
}

fn serve_events(
    mut stream: TcpStream,
    running: &Arc<AtomicBool>,
    hub: &Arc<EventHub>,
) -> io::Result<()> {
    let sub = hub.subscribe();
    let result = (|| -> io::Result<()> {
        let header = "HTTP/1.1 200 OK\r\n\
             Content-Type: text/event-stream\r\n\
             Cache-Control: no-cache\r\n\
             Connection: keep-alive\r\n\
             \r\n";
        stream.write_all(header.as_bytes())?;
        stream.flush()?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        pump_events_to(&mut stream, running, &sub)
    })();
    hub.unsubscribe(&sub);
    result
}

fn advance_idle(idle: u32) -> (u32, Option<&'static [u8]>) {
    let next = idle + 1;
    if next >= KEEPALIVE_EVERY {
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

    fn read_sse_data_frame<R: Read>(stream: &mut R) -> String {
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
        assert_eq!(extract_data_frame(b"data: "), None);
        assert_eq!(extract_data_frame(b"data: partial"), None);
    }

    #[test]
    fn extract_data_frame_waits_when_only_comments_are_complete() {
        assert_eq!(extract_data_frame(b": keepalive\n\ndata: partial"), None);
    }

    #[test]
    fn extract_data_frame_recombines_a_marker_split_across_reads() {
        let mut acc = b": keepalive\n\ndat".to_vec();
        assert_eq!(extract_data_frame(&acc), None);
        acc.extend_from_slice(b"a: hello\n\n");
        assert_eq!(extract_data_frame(&acc).as_deref(), Some("data: hello\n\n"));
    }

    struct NeverDataReader;

    impl Read for NeverDataReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let chunk: &[u8] = b": keepalive\n\n";
            buf[..chunk.len()].copy_from_slice(chunk);
            Ok(chunk.len())
        }
    }

    #[test]
    fn read_sse_data_frame_retains_bytes_across_reads() {
        let mut r = (b": keepalive\n\ndat" as &[u8]).chain(b"a: hello\n\n" as &[u8]);
        assert_eq!(read_sse_data_frame(&mut r), "data: hello\n\n");
    }

    #[test]
    #[should_panic(expected = "keepalive")]
    fn read_sse_data_frame_panics_with_accumulated_bytes_on_budget_exhaustion() {
        let mut r = NeverDataReader;
        read_sse_data_frame(&mut r);
    }

    fn ev(seq: u64) -> Event {
        Event::new(seq, seq, seq, EventKind::Inject { vector: 7 })
    }

    #[test]
    fn parse_request_drains_exactly_through_the_blank_line() {
        let raw = b"GET /foo?x=1 HTTP/1.1\r\nHost: x\r\nAccept: */*\r\n\r\nBODYBYTES";
        let mut cur = std::io::Cursor::new(&raw[..]);
        let req = parse_request(&mut cur).expect("parse");
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/foo", "query string is stripped from the path");

        let mut rest = Vec::new();
        cur.read_to_end(&mut rest).expect("read remainder");
        assert_eq!(
            rest, b"BODYBYTES",
            "exactly the blank line was the stop point"
        );
    }

    #[test]
    fn parse_request_rejects_an_empty_stream() {
        let mut cur = std::io::Cursor::new(&b""[..]);
        assert!(parse_request(&mut cur).is_err());
    }

    #[test]
    fn parse_request_rejects_unterminated_oversized_headers() {
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
        assert_eq!(advance_idle(0), (1, None));
        assert_eq!(advance_idle(5), (6, None));
        assert_eq!(
            advance_idle(KEEPALIVE_EVERY - 1),
            (0, Some(b": keepalive\n\n" as &[u8]))
        );
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

        hub.publish(&ev(1));
        assert_eq!(
            s1.drain().len(),
            1,
            "subscriber 1 receives the published event"
        );
        assert_eq!(s2.drain().len(), 1, "subscriber 2 receives it too");
        assert_eq!(s1.drain().len(), 0);

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
            assert!(TcpStream::connect(a).is_ok(), "an alive server accepts");
            a
        };

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
        let head = read_until(&mut c, "\r\n\r\n");
        assert!(head.contains("\r\n\r\n"), "full header terminator drained");
        assert!(head.contains("text/event-stream"));
        assert!(head.contains("no-cache"));

        live.emit(&Event::new(
            1,
            10,
            5,
            EventKind::Console {
                text: "hello".to_string(),
            },
        ));
        c.set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set timeout");
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
