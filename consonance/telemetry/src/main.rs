// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io::{self, BufRead, BufReader};
use std::net::SocketAddr;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;

use clap::Parser;
use telemetry::{LiveSink, Mode, Observer, ServerOptions, from_ndjson, serve};

#[derive(Parser, Debug)]
#[command(name = "console", version, about)]
struct Cli {
    #[arg(long, default_value = "stdin")]
    source: String,

    #[arg(long, default_value = "127.0.0.1:8088")]
    addr: SocketAddr,

    #[arg(long, default_value_t = telemetry::DEFAULT_CAPACITY)]
    capacity: usize,
}

enum Source {
    Stdin,
    Unix(PathBuf),
    File(PathBuf),
}

fn parse_source(s: &str) -> Result<Source, String> {
    if s == "stdin" || s == "-" {
        Ok(Source::Stdin)
    } else if let Some(p) = s.strip_prefix("unix:") {
        Ok(Source::Unix(PathBuf::from(p)))
    } else if let Some(p) = s.strip_prefix("file:") {
        Ok(Source::File(PathBuf::from(p)))
    } else {
        Err(format!(
            "unknown --source '{s}' (want: stdin | unix:<path> | file:<path>)"
        ))
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let source = match parse_source(&cli.source) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("console: {e}");
            return ExitCode::FAILURE;
        }
    };

    match run(cli, source) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("console: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli, source: Source) -> io::Result<()> {
    let live = LiveSink::new(cli.capacity);

    match source {
        Source::File(path) => {
            let opts = ServerOptions {
                recording: Some(path),
                mode: Mode::Replay,
            };
            let server = serve(cli.addr, live, opts)?;
            eprintln!(
                "console: replay on http://{}/ (Ctrl-C to exit)",
                server.local_addr()
            );
            park_forever();
        }
        Source::Stdin => {
            let server = serve(cli.addr, live.clone(), ServerOptions::default())?;
            eprintln!(
                "console: live on http://{}/ (reading NDJSON from stdin)",
                server.local_addr()
            );
            read_ndjson(BufReader::new(io::stdin().lock()), live);
            eprintln!("console: stdin closed; still serving — Ctrl-C to exit");
            park_forever();
        }
        Source::Unix(path) => {
            let server = serve(cli.addr, live.clone(), ServerOptions::default())?;
            eprintln!(
                "console: live on http://{}/ (accepting NDJSON on unix:{})",
                server.local_addr(),
                path.display()
            );
            accept_unix(&path, live)?;
            park_forever();
        }
    }
}

fn read_ndjson<R: BufRead>(reader: R, mut live: LiveSink) {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(ev) = from_ndjson(trimmed) {
            live.emit(&ev);
        }
    }
}

fn accept_unix(path: &Path, live: LiveSink) -> io::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => read_ndjson(BufReader::new(stream), live.clone()),
            Err(e) => {
                eprintln!("console: unix accept error: {e}");
            }
        }
    }
    Ok(())
}

fn park_forever() -> ! {
    loop {
        thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_source_forms() {
        assert!(matches!(parse_source("stdin"), Ok(Source::Stdin)));
        assert!(matches!(parse_source("-"), Ok(Source::Stdin)));
        assert!(matches!(
            parse_source("unix:/tmp/x.sock"),
            Ok(Source::Unix(_))
        ));
        assert!(matches!(
            parse_source("file:/tmp/run.ndjson"),
            Ok(Source::File(_))
        ));
        assert!(parse_source("http://nope").is_err());
    }
}
