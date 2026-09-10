// SPDX-License-Identifier: AGPL-3.0-or-later

//! A cold-restorable machine checkpoint together with its execution plan.
//!
//! A raw [`Checkpoint`](crate::checkpoint::Checkpoint) carries the machine
//! image, but the image alone cannot identify which recorded action should run
//! next. This envelope retains the action windows and actions beside that
//! image so a cold restore has an explicit, validated continuation plan.

use std::error::Error;

use serde::{Deserialize, Serialize};

use crate::{
    action_execution::ActionExecution,
    checkpoint::Checkpoint,
    target::{ActionWindows, FaultAction},
};

/// The bytes every retained execution envelope starts with.
pub const MAGIC: &[u8; 8] = b"HARMEXEC";
/// The retained execution envelope version this build writes and reads.
pub const VERSION: u32 = 1;

const HEADER_SIZE: usize = MAGIC.len() + std::mem::size_of::<u32>() + 8 + 8;

/// A machine checkpoint and the recorded plan needed to continue it cold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedContinuation {
    /// The captured machine state, including its explicit action cursor.
    pub checkpoint: Checkpoint,
    /// The action-window tiling used by the recorded execution.
    pub windows: ActionWindows,
    /// The recorded actions in execution order.
    pub actions: Vec<FaultAction>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    windows: ActionWindows,
    actions: Vec<FaultAction>,
}

impl RetainedContinuation {
    /// Validate the retained machine and its action plan without running it.
    ///
    /// # Errors
    ///
    /// Returns an error when the machine has no explicit action cursor or the
    /// cursor does not describe a valid position for this action list and
    /// endpoint.
    pub fn validate(&self) -> Result<(), Box<dyn Error>> {
        let cursor = self.checkpoint.cursor.ok_or(
            "retained machine checkpoint has no action cursor and no retained execution plan",
        )?;
        ActionExecution::new(self.windows, cursor).validate(self.checkpoint.at, self.actions.len())
    }

    /// Encode the retained machine and execution plan.
    ///
    /// # Errors
    ///
    /// Returns an error when the retained state is invalid, a length cannot be
    /// represented in the envelope, metadata serialization fails, or output
    /// allocation fails.
    pub fn encode(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        self.validate()?;
        let metadata = serde_json::to_vec(&Metadata {
            windows: self.windows,
            actions: self.actions.clone(),
        })?;
        let machine = self.checkpoint.encode()?;
        let metadata_length = u64::try_from(metadata.len())?;
        let machine_length = u64::try_from(machine.len())?;
        let capacity = HEADER_SIZE
            .checked_add(metadata.len())
            .and_then(|length| length.checked_add(machine.len()))
            .ok_or("retained execution encoded length overflows")?;

        let mut bytes = Vec::new();
        bytes.try_reserve_exact(capacity)?;
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&metadata_length.to_le_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.extend_from_slice(&machine_length.to_le_bytes());
        bytes.extend_from_slice(&machine);
        Ok(bytes)
    }

    /// Decode a retained machine and execution plan.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-envelope blob, an unsupported version,
    /// malformed lengths or metadata, an invalid machine checkpoint, a plan
    /// mismatch, or trailing bytes. A legacy raw machine checkpoint is
    /// rejected explicitly because it has no retained execution plan.
    pub fn decode(bytes: &[u8]) -> Result<Self, Box<dyn Error>> {
        if bytes.starts_with(crate::checkpoint::MAGIC) {
            return Err(
                "raw machine checkpoint has no retained execution plan; use a retained envelope"
                    .into(),
            );
        }

        let mut reader = Reader { bytes, at: 0 };
        if reader.take(MAGIC.len())? != MAGIC {
            return Err("this blob is not a retained Harmony execution".into());
        }
        let version = reader.u32()?;
        if version != VERSION {
            return Err(format!(
                "retained execution version {version} was written by another build; this one reads {VERSION}"
            )
            .into());
        }

        let metadata_length = usize::try_from(reader.u64()?)?;
        let metadata_bytes = reader.take(metadata_length)?;
        let machine_length = usize::try_from(reader.u64()?)?;
        let machine_bytes = reader.take(machine_length)?;
        reader.finish()?;

        let metadata: Metadata = serde_json::from_slice(metadata_bytes)?;
        let checkpoint = Checkpoint::decode(machine_bytes)?;
        let retained = Self {
            checkpoint,
            windows: metadata.windows,
            actions: metadata.actions,
        };
        retained.validate()?;
        Ok(retained)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], Box<dyn Error>> {
        let end = self
            .at
            .checked_add(length)
            .ok_or("retained execution length overflows")?;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or("retained execution ends before its declared contents")?;
        self.at = end;
        Ok(slice)
    }

    fn finish(&self) -> Result<(), Box<dyn Error>> {
        let remaining = self
            .bytes
            .len()
            .checked_sub(self.at)
            .ok_or("retained execution reader advanced past input")?;
        if remaining != 0 {
            return Err(format!("retained execution has {remaining} trailing bytes").into());
        }
        Ok(())
    }

    fn u32(&mut self) -> Result<u32, Box<dyn Error>> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }

    fn u64(&mut self) -> Result<u64, Box<dyn Error>> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::ActionCursor;

    fn active_checkpoint() -> RetainedContinuation {
        let windows = ActionWindows {
            root_seal: 1_000,
            horizon_nanos: 100,
        };
        let mut cursor = ActionCursor::from_parts(0, false).expect("valid cursor");
        cursor.activate(0).expect("activate first action");
        RetainedContinuation {
            checkpoint: Checkpoint {
                setup: 7,
                at: 1_000,
                image_identity: [0x5a; 32],
                pages: vec![(0x1234, vec![0xa5; 4096])],
                sidecar: b"opaque device state".to_vec(),
                cursor: Some(cursor),
            },
            windows,
            actions: vec![FaultAction::Wait, FaultAction::Interrupt(0x30)],
        }
    }

    fn empty_checkpoint() -> RetainedContinuation {
        RetainedContinuation {
            checkpoint: Checkpoint {
                at: 7,
                cursor: Some(ActionCursor::default()),
                ..Checkpoint::default()
            },
            windows: ActionWindows {
                root_seal: 7,
                horizon_nanos: 100,
            },
            actions: Vec::new(),
        }
    }

    fn envelope(metadata: &[u8], machine: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(
            &u64::try_from(metadata.len())
                .expect("test metadata length fits in a u64")
                .to_le_bytes(),
        );
        bytes.extend_from_slice(metadata);
        bytes.extend_from_slice(
            &u64::try_from(machine.len())
                .expect("test machine length fits in a u64")
                .to_le_bytes(),
        );
        bytes.extend_from_slice(machine);
        bytes
    }

    fn metadata_for(windows: ActionWindows, actions: &[FaultAction]) -> Vec<u8> {
        serde_json::to_vec(&Metadata {
            windows,
            actions: actions.to_vec(),
        })
        .expect("serialize test metadata")
    }

    #[test]
    fn an_active_plan_round_trips_with_machine_bytes_and_opaque_sidecar() {
        let retained = active_checkpoint();
        let bytes = retained.encode().expect("encode retained execution");
        assert_eq!(&bytes[..MAGIC.len()], MAGIC);
        assert_eq!(
            RetainedContinuation::decode(&bytes).expect("decode"),
            retained
        );
    }

    #[test]
    fn an_empty_inactive_plan_is_valid() {
        let retained = empty_checkpoint();
        let bytes = retained.encode().expect("encode empty retained execution");
        assert_eq!(
            RetainedContinuation::decode(&bytes).expect("decode"),
            retained
        );
    }

    #[test]
    fn bad_magic_and_version_are_rejected() {
        let bytes = active_checkpoint().encode().expect("encode");

        let mut bad_magic = bytes.clone();
        bad_magic[0] ^= 1;
        let error = RetainedContinuation::decode(&bad_magic).expect_err("bad magic");
        assert!(error.to_string().contains("not a retained"), "{error}");

        let mut bad_version = bytes;
        bad_version[MAGIC.len()] = 99;
        let error = RetainedContinuation::decode(&bad_version).expect_err("bad version");
        assert!(error.to_string().contains("reads 1"), "{error}");
    }

    #[test]
    fn truncation_huge_lengths_and_trailing_bytes_are_rejected() {
        let bytes = active_checkpoint().encode().expect("encode");
        for cut in [
            0,
            1,
            MAGIC.len(),
            MAGIC.len() + 4,
            HEADER_SIZE,
            bytes.len() / 2,
            bytes.len() - 1,
        ] {
            assert!(
                RetainedContinuation::decode(&bytes[..cut]).is_err(),
                "truncated at {cut}"
            );
        }

        let mut huge_metadata = bytes.clone();
        huge_metadata[MAGIC.len() + 4..MAGIC.len() + 12].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(RetainedContinuation::decode(&huge_metadata).is_err());

        let metadata_length = usize::try_from(u64::from_le_bytes(
            bytes[MAGIC.len() + 4..MAGIC.len() + 12]
                .try_into()
                .expect("metadata length bytes"),
        ))
        .expect("test metadata length");
        let machine_length_at = MAGIC.len() + 4 + 8 + metadata_length;
        let mut huge_machine = bytes.clone();
        huge_machine[machine_length_at..machine_length_at + 8]
            .copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(RetainedContinuation::decode(&huge_machine).is_err());

        let mut trailing = bytes;
        trailing.push(0xa5);
        let error = RetainedContinuation::decode(&trailing).expect_err("trailing bytes");
        assert!(error.to_string().contains("trailing"), "{error}");
    }

    #[test]
    fn raw_legacy_machine_checkpoint_has_no_retained_plan() {
        let machine = active_checkpoint().checkpoint;
        let bytes = Checkpoint {
            cursor: None,
            ..machine
        }
        .encode()
        .expect("encode legacy machine checkpoint");
        let error = RetainedContinuation::decode(&bytes).expect_err("raw machine checkpoint");
        assert!(
            error.to_string().contains("no retained execution plan"),
            "{error}"
        );
    }

    #[test]
    fn missing_cursor_and_plan_mismatches_fail_before_runtime_use() {
        let retained = active_checkpoint();
        let machine = Checkpoint {
            cursor: None,
            ..retained.checkpoint.clone()
        };
        let bytes = envelope(
            &metadata_for(retained.windows, &retained.actions),
            &machine.encode().expect("encode legacy machine"),
        );
        let error = RetainedContinuation::decode(&bytes).expect_err("missing cursor");
        assert!(error.to_string().contains("no action cursor"), "{error}");

        let mut count_mismatch = active_checkpoint();
        count_mismatch.actions.clear();
        let bytes = count_mismatch.encode();
        assert!(bytes.is_err(), "encoder validates action count");

        let mut invalid_machine = active_checkpoint().checkpoint;
        invalid_machine.cursor = Some(ActionCursor::from_parts(2, false).expect("cursor"));
        let bytes = envelope(
            &metadata_for(active_checkpoint().windows, &[FaultAction::Wait]),
            &invalid_machine.encode().expect("encode mismatched machine"),
        );
        let error = RetainedContinuation::decode(&bytes).expect_err("count mismatch");
        assert!(
            error.to_string().contains("exceeds action count"),
            "{error}"
        );

        let windows = ActionWindows {
            root_seal: 1_000,
            horizon_nanos: 100,
        };
        let time_machine = Checkpoint {
            at: 1_000,
            cursor: Some(ActionCursor::from_parts(1, false).expect("cursor")),
            ..Checkpoint::default()
        };
        let bytes = envelope(
            &metadata_for(windows, &[FaultAction::Wait, FaultAction::Wait]),
            &time_machine.encode().expect("encode time mismatch machine"),
        );
        let error = RetainedContinuation::decode(&bytes).expect_err("time mismatch");
        assert!(error.to_string().contains("precedes"), "{error}");
    }

    #[test]
    fn metadata_unknown_fields_are_rejected() {
        let retained = empty_checkpoint();
        let machine = retained.checkpoint.encode().expect("encode machine");
        let metadata =
            br#"{"windows":{"root_seal":7,"horizon_nanos":100},"actions":[],"extra":true}"#;
        let bytes = envelope(metadata, &machine);
        assert!(RetainedContinuation::decode(&bytes).is_err());
    }
}
