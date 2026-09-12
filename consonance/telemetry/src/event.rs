// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub exit_count: u64,
    pub vns: u64,
    pub kind: EventKind,
}

impl Event {
    pub fn new(seq: u64, exit_count: u64, vns: u64, kind: EventKind) -> Event {
        Event {
            seq,
            exit_count,
            vns,
            kind,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EventKind {
    Console {
        text: String,
    },
    GuestEvent {
        id: u32,
        data: Vec<u8>,
    },
    Io {
        port: u16,
        size: u8,
        value: u64,
        write: bool,
    },
    Mmio {
        addr: u64,
        size: u8,
        value: u64,
        write: bool,
    },
    Hypercall {
        service: u8,
        opcode: u16,
        status: u16,
    },
    Msr {
        index: u32,
        value: u64,
        write: bool,
    },
    Cpuid {
        leaf: u32,
        subleaf: u32,
    },
    Inject {
        vector: u8,
    },
    Checkpoint {
        state_hash: [u8; 32],
    },
    Counts(ExitCounts),
    Terminal {
        reason: String,
    },
    Dropped {
        count: u64,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitCounts {
    pub io: u64,
    pub mmio: u64,
    pub rdmsr: u64,
    pub wrmsr: u64,
    pub hypercall: u64,
    pub cpuid: u64,
    pub rdtsc: u64,
    pub rdtscp: u64,
    pub rdrand: u64,
    pub rdseed: u64,
    pub hlt: u64,
    pub shutdown: u64,
}

impl ExitCounts {
    pub fn total(&self) -> u64 {
        [
            self.io,
            self.mmio,
            self.rdmsr,
            self.wrmsr,
            self.hypercall,
            self.cpuid,
            self.rdtsc,
            self.rdtscp,
            self.rdrand,
            self.rdseed,
            self.hlt,
            self.shutdown,
        ]
        .into_iter()
        .fold(0u64, u64::saturating_add)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("malformed telemetry NDJSON line: {0}")]
    Decode(#[from] serde_json::Error),
}

pub fn to_ndjson(ev: &Event) -> Result<String, WireError> {
    Ok(serde_json::to_string(ev)?)
}

pub fn from_ndjson(line: &str) -> Result<Event, WireError> {
    Ok(serde_json::from_str(line)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_roundtrips_through_ndjson() {
        let ev = Event::new(
            1,
            42,
            21,
            EventKind::Console {
                text: "postgres ready\n".to_string(),
            },
        );
        let line = to_ndjson(&ev).expect("encode");
        assert!(!line.contains('\n'), "line must be single-line");
        assert_eq!(from_ndjson(&line).expect("decode"), ev);
    }

    #[test]
    fn checkpoint_hash_roundtrips() {
        let mut h = [0u8; 32];
        for (i, b) in h.iter_mut().enumerate() {
            *b = i as u8;
        }
        let ev = Event::new(7, 1000, 500, EventKind::Checkpoint { state_hash: h });
        let line = to_ndjson(&ev).expect("encode");
        assert_eq!(from_ndjson(&line).expect("decode"), ev);
    }

    #[test]
    fn counts_total_saturates() {
        let c = ExitCounts {
            io: u64::MAX,
            hlt: 5,
            ..ExitCounts::default()
        };
        assert_eq!(c.total(), u64::MAX);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(from_ndjson("not json at all").is_err());
    }
}
