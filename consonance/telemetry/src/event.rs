// SPDX-License-Identifier: AGPL-3.0-or-later
//! The telemetry schema and its NDJSON wire.
//!
//! An [`Event`] is one unit of out-of-band observation: a per-run monotonic
//! `seq`, the retired-branch `work` counter read at the exit, the V-time `vns`
//! that `VClock::vns(work)` maps it to, and an [`EventKind`] payload describing
//! what the guest did. The `work`/`vns` pair is the load-bearing field: it is a
//! pure function of the run, so a recorded stream re-renders on an identical
//! timeline (the console keys everything on `vns`).
//!
//! Nothing here is ever hashed, folded into `observable_digest`, or fed back to
//! the guest — telemetry is for the human operator; the hashes remain the source
//! of truth (see `docs/INTEGRATION.md` §8).

use serde::{Deserialize, Serialize};

/// One unit of telemetry: a V-time-stamped observation of a serviced exit.
///
/// `seq` is a per-run monotonic counter (so a consumer can detect a gap even on
/// the lossy live lane); `work` is the retired-branch work counter at the exit;
/// `vns` is `VClock::vns(work)` — the deterministic virtual-time stamp the
/// console renders against. `kind` carries the per-exit detail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Per-run monotonic event counter (gap-detectable on the lossy live lane).
    pub seq: u64,
    /// Retired-branch work counter read at the exit (the V-time domain).
    pub work: u64,
    /// `VClock::vns(work)` — the deterministic virtual-time stamp (ns).
    pub vns: u64,
    /// What the guest did at this exit.
    pub kind: EventKind,
}

impl Event {
    /// Builds an event from its parts. A convenience for the frontier wiring and
    /// for tests; the fields are public, so this is purely ergonomic.
    pub fn new(seq: u64, work: u64, vns: u64, kind: EventKind) -> Event {
        Event {
            seq,
            work,
            vns,
            kind,
        }
    }
}

/// The per-exit payload. **Non-exhaustive**: the frontier wiring (and future
/// backends) may surface exit reasons this version does not name, so consumers
/// must treat unknown kinds gracefully rather than assume a closed set.
///
/// Serialized **externally tagged** (`{"Console":{"text":"…"}}`), so the browser
/// reads the variant with `Object.keys(ev.kind)[0]` — no schema library needed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EventKind {
    /// COM1 (`Uart8250`) writes, decoded UTF-8-lossy. **Display fidelity only** —
    /// the byte-exact serial capture lives in the M2 hash, never here.
    Console {
        /// The decoded console text written at this exit.
        text: String,
    },
    /// A guest-pushed event from the hypercall `Event` service (id 4): a
    /// test/coverage signal carrying an opaque id and payload.
    GuestEvent {
        /// Guest-chosen event id.
        id: u32,
        /// Opaque event payload bytes.
        data: Vec<u8>,
    },
    /// A port-I/O exit (`Exit::Io`). The report channel (`REPORT_PORT = 0x0CA2`)
    /// and the hypercall doorbell (`0x0CA1`) both surface here; the console
    /// highlights the report port specially.
    Io {
        /// I/O port.
        port: u16,
        /// Access width in bytes (1/2/4).
        size: u8,
        /// The value written (OUT) or read (IN).
        value: u64,
        /// `true` for OUT, `false` for IN.
        write: bool,
    },
    /// An MMIO exit (`Exit::Mmio`) — e.g. the userspace xAPIC page.
    Mmio {
        /// Guest-physical address of the access.
        addr: u64,
        /// Access width in bytes.
        size: u8,
        /// The value stored or loaded.
        value: u64,
        /// `true` for a store, `false` for a load.
        write: bool,
    },
    /// A serviced hypercall (`Exit::Hypercall`) on the patched backend: the
    /// dispatched service id, opcode, and resulting status.
    Hypercall {
        /// Hypercall service id.
        service: u8,
        /// Service-specific opcode.
        opcode: u16,
        /// Resulting status word.
        status: u16,
    },
    /// A filtered MSR access (`Exit::Rdmsr`/`Exit::Wrmsr`).
    Msr {
        /// MSR index.
        index: u32,
        /// The value read or written.
        value: u64,
        /// `true` for a write, `false` for a read.
        write: bool,
    },
    /// A `RDTSC`/`RDTSCP` resolved against V-time (patched backend).
    Tsc {
        /// The V-time TSC value delivered to the guest.
        value: u64,
    },
    /// A `RDRAND`/`RDSEED` resolved from the seeded entropy stream (patched
    /// backend).
    Rng {
        /// The seeded random word delivered to the guest.
        value: u64,
    },
    /// A `CPUID` exit (patched backend) — the leaf/subleaf queried.
    Cpuid {
        /// CPUID leaf (`EAX`).
        leaf: u32,
        /// CPUID subleaf (`ECX`).
        subleaf: u32,
    },
    /// An interrupt the `InjectionPlanner` delivered at this V-time.
    Inject {
        /// The 8-bit vector injected.
        vector: u8,
    },
    /// A periodic full-state checkpoint: the `state_hash()` at this V-time. The
    /// console renders the hash with a `vns`→wall-clock readout.
    Checkpoint {
        /// The 32-byte canonical state hash at this checkpoint.
        state_hash: [u8; 32],
    },
    /// A periodic snapshot of the per-reason exit tally (drives the live
    /// exit-rate counters and graph). [`ExitCounts`] is a **local mirror** of
    /// vmm-backend's tally (this is a leaf crate; no sibling import).
    Counts(ExitCounts),
    /// The run ended — the reason (test passed/failed, guest halted, hung).
    Terminal {
        /// Human-readable terminal reason.
        reason: String,
    },
    /// **Telemetry-internal, additive:** surfaced by [`crate::LiveSink`] when the
    /// lossy live queue overflowed and dropped events, so the operator sees the
    /// gap rather than silently missing it. Never produced by the frontier
    /// wiring and never present in a lossless [`crate::NdjsonRecorder`] stream.
    Dropped {
        /// How many events the live lane dropped since the last surfaced notice.
        count: u64,
    },
}

/// A local mirror of `vmm-backend`'s `ExitCounts` — the per-reason trap tally,
/// carried in [`EventKind::Counts`] to drive the console's exit-rate counters.
///
/// Defined here (not imported) because `telemetry` is a **leaf** crate with no
/// sibling deps (conventions rule 2); the frontier seam maps vmm-backend's counts
/// into this struct. Field order matches vmm-backend's for a faithful mirror.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitCounts {
    /// Port-I/O exits.
    pub io: u64,
    /// MMIO exits.
    pub mmio: u64,
    /// MSR-read exits.
    pub rdmsr: u64,
    /// MSR-write exits.
    pub wrmsr: u64,
    /// Hypercall exits.
    pub hypercall: u64,
    /// CPUID exits.
    pub cpuid: u64,
    /// `RDTSC` exits.
    pub rdtsc: u64,
    /// `RDTSCP` exits.
    pub rdtscp: u64,
    /// `RDRAND` exits.
    pub rdrand: u64,
    /// `RDSEED` exits.
    pub rdseed: u64,
    /// `HLT` exits.
    pub hlt: u64,
    /// Shutdown exits.
    pub shutdown: u64,
    /// `run_until` deadline exits.
    pub deadline: u64,
}

impl ExitCounts {
    /// Total trapped exits — the **saturating** sum of every per-reason counter
    /// (the fields are public and saturate individually, so a plain sum could
    /// overflow). Mirrors vmm-backend's discipline.
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
            self.deadline,
        ]
        .into_iter()
        .fold(0u64, u64::saturating_add)
    }
}

/// Failure decoding an NDJSON line back into an [`Event`].
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// The line was not valid `serde_json` for an [`Event`].
    #[error("malformed telemetry NDJSON line: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Encodes one event as a single NDJSON line **without** the trailing newline
/// (callers frame it: the recorder appends `\n`, SSE appends `\n\n`).
///
/// Infallible in practice — an [`Event`] contains no map with non-string keys
/// and no floats, the only `serde_json` serialize failures — but returns a
/// `Result` rather than panicking, per the no-panic library rule.
pub fn to_ndjson(ev: &Event) -> Result<String, WireError> {
    Ok(serde_json::to_string(ev)?)
}

/// Decodes one NDJSON line (newline already stripped) back into an [`Event`].
///
/// # Errors
///
/// [`WireError::Decode`] if the line is not valid JSON for an [`Event`].
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
