// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    error::Error,
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
};

use serde::{Deserialize, Serialize};

use super::StateHashEncoding;

/// Version tag for the per-replay endpoint event sidecar.
pub const FORMAT: &str = "harmony-replay-events-v1";

/// One raw SDK report in the same shape used by investigation evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SdkEventRecord {
    /// Position in the guest event stream, starting at zero.
    pub position: u64,
    /// Virtual time at which the guest published the event.
    pub virtual_time: u64,
    /// Event id.
    pub event: u32,
    /// Raw event payload as lowercase hexadecimal.
    pub payload: String,
}

/// Complete, untruncated event evidence for one replay run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayEventEvidence {
    /// Always [`FORMAT`].
    pub format: String,
    /// One-based replay run ordinal.
    pub run: u32,
    /// Exact virtual time of the replay endpoint.
    pub virtual_time: u64,
    /// Direct engine state digest, lowercase hexadecimal.
    pub state_hash: String,
    /// Encoding of [`Self::state_hash`].
    pub state_hash_encoding: StateHashEncoding,
    /// Every SDK event published through the endpoint, in stream order.
    pub events: Vec<SdkEventRecord>,
}

impl ReplayEventEvidence {
    /// Build and validate one sidecar from a raw SDK stream.
    pub fn from_raw(
        run: u32,
        virtual_time: u64,
        state_hash: [u8; 32],
        raw_events: &[(u64, u32, Vec<u8>)],
    ) -> Result<Self, String> {
        let events = records_from_raw(raw_events)?;
        let state_hash = hex(&state_hash);
        Self::new(run, virtual_time, state_hash, events)
    }

    /// Build one sidecar from already encoded event records.
    pub fn new(
        run: u32,
        virtual_time: u64,
        state_hash: String,
        events: Vec<SdkEventRecord>,
    ) -> Result<Self, String> {
        let evidence = Self {
            format: FORMAT.to_owned(),
            run,
            virtual_time,
            state_hash,
            state_hash_encoding: StateHashEncoding::EngineDigest,
            events,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    /// Validate the version, endpoint identity, and complete stream.
    pub fn validate(&self) -> Result<(), String> {
        if self.format != FORMAT {
            return Err(format!(
                "replay event evidence has format {:?}, not {FORMAT:?}",
                self.format
            ));
        }
        if self.run == 0 {
            return Err("replay event evidence run must be positive".to_owned());
        }
        if self.state_hash_encoding != StateHashEncoding::EngineDigest {
            return Err(
                "replay event evidence must use the direct engine digest encoding".to_owned(),
            );
        }
        if !is_lower_hex(&self.state_hash, 32) {
            return Err(
                "replay event evidence state hash is not 64 lowercase hex digits".to_owned(),
            );
        }
        for (index, event) in self.events.iter().enumerate() {
            let position = u64::try_from(index)
                .map_err(|_| "replay event evidence position overflows".to_owned())?;
            if event.position != position {
                return Err(format!(
                    "replay event at index {position} has position {}, expected {position}",
                    event.position
                ));
            }
            if event.virtual_time > self.virtual_time {
                return Err(format!(
                    "replay event {} occurs at {}, after endpoint {}",
                    event.position, event.virtual_time, self.virtual_time
                ));
            }
            if !is_lower_hex(&event.payload, event.payload.len() / 2)
                || !event.payload.len().is_multiple_of(2)
            {
                return Err(format!(
                    "replay event {} payload is not lowercase hexadecimal",
                    event.position
                ));
            }
        }
        Ok(())
    }

    /// Encode the validated sidecar as JSON bytes.
    pub fn encode(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        self.validate()?;
        Ok(serde_json::to_vec_pretty(self)?)
    }

    /// Decode and validate sidecar bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, Box<dyn Error>> {
        let evidence: Self = serde_json::from_slice(bytes)?;
        evidence.validate()?;
        Ok(evidence)
    }

    /// The stable output filename for this replay ordinal.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("replay-{}-events.json", self.run)
    }

    /// Write and flush the sidecar before its corresponding report is
    /// published.
    pub fn write(&self, directory: &Path) -> Result<(), Box<dyn Error>> {
        let bytes = self.encode()?;
        fs::create_dir_all(directory)?;
        let path = directory.join(self.file_name());
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        File::open(directory)?.sync_all()?;
        Ok(())
    }
}

/// Convert the raw client event tuples without dropping or truncating one.
pub fn records_from_raw(raw_events: &[(u64, u32, Vec<u8>)]) -> Result<Vec<SdkEventRecord>, String> {
    raw_events
        .iter()
        .enumerate()
        .map(|(index, (virtual_time, event, payload))| {
            Ok(SdkEventRecord {
                position: u64::try_from(index)
                    .map_err(|_| "replay event position overflows".to_owned())?,
                virtual_time: *virtual_time,
                event: *event,
                payload: hex(payload),
            })
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_lower_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes.saturating_mul(2)
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ReplayEventEvidence {
        ReplayEventEvidence::from_raw(
            2,
            50,
            [0x42; 32],
            &[(7, 0x1001, vec![0, 1]), (50, 0x2002, vec![])],
        )
        .expect("sample sidecar")
    }

    #[test]
    fn sidecar_round_trips_with_complete_ordered_events() {
        let evidence = sample();
        assert_eq!(evidence.file_name(), "replay-2-events.json");
        assert_eq!(evidence.events.len(), 2);
        let decoded =
            ReplayEventEvidence::decode(&evidence.encode().expect("encode")).expect("decode");
        assert_eq!(decoded, evidence);
    }

    #[test]
    fn sidecar_rejects_identity_and_stream_inconsistency() {
        let mut evidence = sample();
        evidence.format = "old".to_owned();
        assert!(evidence.validate().is_err());

        let mut evidence = sample();
        evidence.events[1].position = 4;
        assert!(evidence.validate().is_err());

        let mut evidence = sample();
        evidence.events[0].virtual_time = 51;
        assert!(evidence.validate().is_err());

        let mut evidence = sample();
        evidence.state_hash = "42".repeat(31);
        assert!(evidence.validate().is_err());
    }

    #[test]
    fn raw_event_conversion_never_truncates_payloads() {
        let payload = vec![0xde, 0xad, 0xbe, 0xef];
        let records = records_from_raw(&[(1, 7, payload)]).expect("records");
        assert_eq!(records[0].position, 0);
        assert_eq!(records[0].payload, "deadbeef");
    }

    #[test]
    fn sidecar_write_is_durable_and_does_not_overwrite_a_run() {
        let directory = tempfile::tempdir().expect("temporary output directory");
        let evidence = sample();
        evidence.write(directory.path()).expect("write sidecar");

        let path = directory.path().join(evidence.file_name());
        let bytes = std::fs::read(&path).expect("read sidecar");
        assert_eq!(
            ReplayEventEvidence::decode(&bytes).expect("decode sidecar"),
            evidence
        );
        assert!(evidence.write(directory.path()).is_err());
    }
}
