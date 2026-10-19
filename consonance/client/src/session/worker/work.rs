// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy)]
pub(super) enum Group {
    Hello,
    Setup,
    Run,
    Observe,
    Snapshot,
    Identity,
    Replay,
    Export,
    Import,
    Telemetry,
    Unknown,
}
impl Group {
    const ALL: [Self; 11] = [
        Self::Hello,
        Self::Setup,
        Self::Run,
        Self::Observe,
        Self::Snapshot,
        Self::Identity,
        Self::Replay,
        Self::Export,
        Self::Import,
        Self::Telemetry,
        Self::Unknown,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::Setup => "setup",
            Self::Run => "run",
            Self::Observe => "observe",
            Self::Snapshot => "snapshot",
            Self::Identity => "identity",
            Self::Replay => "replay",
            Self::Export => "export",
            Self::Import => "import",
            Self::Telemetry => "telemetry",
            Self::Unknown => "unknown",
        }
    }
    pub(super) fn request(body: &[u8]) -> Self {
        use super::*;
        match body.first().copied() {
            Some(INIT) => Self::Setup,
            Some(RUN_UNTIL | RUN_RESOLVING) => Self::Run,
            Some(SDK_EVENTS | CONSOLE_TAIL) => Self::Observe,
            Some(SNAPSHOT | DROP_SNAPSHOT | OWNED_PAGES | STORE_BYTES) => Self::Snapshot,
            Some(STATE_HASH | EVENT_HISTORY) => Self::Identity,
            Some(REPLAY | BRANCH) => Self::Replay,
            Some(EXPORT | NEED_EXTENT | EXTENT_READY | EXTENT_DENIED) => Self::Export,
            Some(IMPORT) => Self::Import,
            Some(TELEMETRY) => Self::Telemetry,
            _ => Self::Unknown,
        }
    }
}
#[derive(Default)]
pub(super) struct Delta {
    pub header: u64,
    pub body: u64,
    pub fds: u64,
}
#[derive(Debug)]
pub(super) struct Work {
    values: [[[AtomicU64; 6]; 2]; 11],
}
impl Default for Work {
    fn default() -> Self {
        Self {
            values: std::array::from_fn(|_| {
                std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0)))
            }),
        }
    }
}
impl Work {
    pub(super) fn record(&self, group: Group, receive: bool, delta: Delta, complete: bool) {
        for (counter, value) in self.values[group as usize][usize::from(receive)]
            .iter()
            .zip([
                1,
                u64::from(complete),
                u64::from(!complete),
                delta.header,
                delta.body,
                delta.fds,
            ])
        {
            let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                Some(old.saturating_add(value))
            });
        }
    }
    pub(super) fn counters(&self, prefix: &str) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for group in Group::ALL {
            for (direction, receive) in [("send", false), ("receive", true)] {
                let values = &self.values[group as usize][usize::from(receive)];
                if values[0].load(Ordering::Relaxed) == 0 {
                    continue;
                }
                for (name, value) in [
                    "attempts",
                    "completed",
                    "errors",
                    "header_bytes",
                    "body_bytes",
                    "fd_handles",
                ]
                .into_iter()
                .zip(values)
                {
                    out.push((
                        format!("{prefix}.{}.{direction}.{name}", group.name()),
                        value.load(Ordering::Relaxed),
                    ));
                }
            }
        }
        out
    }
}

pub(super) fn write_all_counted(
    mut bytes: &[u8],
    counter: &mut u64,
    mut write: impl FnMut(&[u8]) -> std::io::Result<usize>,
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        match write(bytes) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                *counter = counter.saturating_add(n as u64);
                bytes = &bytes[n..];
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
#[cfg(any(not(target_os = "macos"), test))]
pub(super) fn read_exact_counted(
    mut bytes: &mut [u8],
    counter: &mut u64,
    retry_interrupted: bool,
    mut read: impl FnMut(&mut [u8]) -> std::io::Result<usize>,
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        match read(bytes) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                *counter = counter.saturating_add(n as u64);
                bytes = &mut bytes[n..];
            }
            Err(e) if retry_interrupted && e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, io};
    #[test]
    fn partial_transfers_keep_progress_before_errors_and_preserve_retry_policy() {
        let mut count = 0;
        let mut writes = VecDeque::from([
            Ok(2),
            Err(io::ErrorKind::Interrupted),
            Ok(3),
            Err(io::ErrorKind::BrokenPipe),
        ]);
        let error = write_all_counted(&[0; 8], &mut count, |_| {
            writes.pop_front().unwrap().map_err(Into::into)
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(count, 5);
        assert!(writes.is_empty());
        let mut reads = VecDeque::from([Ok(2), Err(io::ErrorKind::Interrupted), Ok(1), Ok(0)]);
        count = 0;
        let error = read_exact_counted(&mut [0; 8], &mut count, true, |_| {
            reads.pop_front().unwrap().map_err(Into::into)
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(count, 3);
        count = 0;
        let error = read_exact_counted(&mut [0; 1], &mut count, false, |_| {
            Err(io::ErrorKind::Interrupted.into())
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(count, 0);
        assert_eq!(
            write_all_counted(&[0], &mut count, |_| Ok(0))
                .unwrap_err()
                .kind(),
            io::ErrorKind::WriteZero
        );
        assert_eq!(count, 0);
        assert_eq!(
            read_exact_counted(&mut [0], &mut count, false, |_| Err(
                io::ErrorKind::TimedOut.into()
            ))
            .unwrap_err()
            .kind(),
            io::ErrorKind::TimedOut
        );
    }
    #[test]
    fn all_request_opcodes_have_bounded_meaningful_groups() {
        use super::super::*;
        for (ops, expected) in [
            (vec![INIT], "setup"),
            (vec![RUN_UNTIL, RUN_RESOLVING], "run"),
            (vec![SDK_EVENTS, CONSOLE_TAIL], "observe"),
            (
                vec![SNAPSHOT, DROP_SNAPSHOT, OWNED_PAGES, STORE_BYTES],
                "snapshot",
            ),
            (vec![STATE_HASH, EVENT_HISTORY], "identity"),
            (vec![REPLAY, BRANCH], "replay"),
            (
                vec![EXPORT, NEED_EXTENT, EXTENT_READY, EXTENT_DENIED],
                "export",
            ),
            (vec![IMPORT], "import"),
            (vec![TELEMETRY], "telemetry"),
            (vec![255], "unknown"),
        ] {
            for op in ops {
                assert_eq!(Group::request(&[op]).name(), expected);
            }
        }
        assert_eq!(Group::request(&[]).name(), "unknown");
    }
}
