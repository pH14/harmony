//! Read-only equality for independently generated snapshots from two core builds.
//! This relation never changes or imports either snapshot.
use super::{MetroidSnapshot, Result, SnapshotCheckpoint, valid_sha};
use machine::quicknes::QUICKNES_REVISION;
use serde_json::Value;

pub const POLICY: &str = "quicknes-exact-snapshot-except-verified-core-v1";

fn split(mut value: Value, core: &str) -> Result<(Value, Vec<u8>)> {
    if !valid_sha(core) {
        return Err("invalid comparison core hash".into());
    }
    let state = value
        .as_object_mut()
        .and_then(|object| object.remove("emulator_state"))
        .ok_or("snapshot has no emulator state")?;
    let bytes: Vec<u8> = serde_json::from_value(state)?;
    if bytes.len() < 120
        || &bytes[..8] != b"HQNESST2"
        || &bytes[8..48] != QUICKNES_REVISION.as_bytes()
        || &bytes[48..112] != core.as_bytes()
        || u64::from_le_bytes(bytes[112..120].try_into()?) != (bytes.len() - 120) as u64
    {
        return Err("snapshot comparison header or core identity differs".into());
    }
    Ok((value, bytes))
}

fn compare_values(
    actual: Value,
    original: Value,
    runtime_core: &str,
    original_core: &str,
) -> Result<()> {
    let (actual_metadata, actual_bytes) = split(actual, runtime_core)?;
    let (original_metadata, original_bytes) = split(original, original_core)?;
    if actual_metadata != original_metadata
        || actual_bytes[..48] != original_bytes[..48]
        || actual_bytes[112..] != original_bytes[112..]
    {
        return Err("generated snapshot differs outside the verified core identity".into());
    }
    Ok(())
}

pub fn compare(
    actual: &MetroidSnapshot,
    original: &MetroidSnapshot,
    runtime_core: &str,
    original_core: &str,
) -> Result<()> {
    compare_values(
        serde_json::to_value(actual)?,
        serde_json::to_value(original)?,
        runtime_core,
        original_core,
    )
}

pub fn compare_checkpoints(
    actual: &SnapshotCheckpoint<MetroidSnapshot>,
    original: &SnapshotCheckpoint<MetroidSnapshot>,
    runtime_core: &str,
    original_core: &str,
) -> Result<()> {
    if actual.format != original.format || actual.entries.len() != original.entries.len() {
        return Err("checkpoint format or complete entry count differs".into());
    }
    for (actual, original) in actual.entries.iter().zip(&original.entries) {
        if actual.id != original.id {
            return Err("checkpoint IDs or their order differ".into());
        }
        compare(
            &actual.snapshot,
            &original.snapshot,
            runtime_core,
            original_core,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(core: &str) -> Value {
        // Constructed bytes test the relation only; this is not a runnable state.
        let mut bytes = b"HQNESST2".to_vec();
        bytes.extend_from_slice(QUICKNES_REVISION.as_bytes());
        bytes.extend_from_slice(core.as_bytes());
        bytes.extend_from_slice(&3_u64.to_le_bytes());
        bytes.extend_from_slice(&[11, 22, 33]);
        json!({"emulator_state":bytes,"observation":{"health":79,"frame_count":117875},"failed":false})
    }

    #[test]
    fn only_the_independently_checked_core_identity_may_differ() {
        let old = "a".repeat(64);
        let new = "b".repeat(64);
        let original = fixture(&old);
        let actual = fixture(&new);
        compare_values(actual.clone(), original.clone(), &new, &old).unwrap();
        assert!(compare_values(actual.clone(), original.clone(), &old, &old).is_err());
        assert!(compare_values(actual.clone(), original.clone(), &new, &new).is_err());
        for index in [0, 8, 48, 111, 112, 119, 120, 122] {
            let mut bad = actual.clone();
            bad["emulator_state"][index] =
                json!(bad["emulator_state"][index].as_u64().unwrap() ^ 1);
            assert!(
                compare_values(bad, original.clone(), &new, &old).is_err(),
                "byte {index}"
            );
        }
        let mut extra = actual.clone();
        extra["emulator_state"]
            .as_array_mut()
            .unwrap()
            .push(json!(0));
        assert!(compare_values(extra, original.clone(), &new, &old).is_err());
        let mut truncated = actual.clone();
        truncated["emulator_state"]
            .as_array_mut()
            .unwrap()
            .truncate(119);
        assert!(compare_values(truncated, original, &new, &old).is_err());
    }

    #[test]
    fn all_observation_and_terminal_metadata_must_match() {
        let core = "a".repeat(64);
        let original = fixture(&core);
        for (field, value) in [
            ("health", json!(80)),
            ("frame_count", json!(117874)),
            ("extra", json!(false)),
        ] {
            let mut bad = original.clone();
            bad["observation"][field] = value;
            assert!(compare_values(bad, original.clone(), &core, &core).is_err());
        }
        let mut bad = original.clone();
        bad["failed"] = json!(true);
        assert!(compare_values(bad, original.clone(), &core, &core).is_err());
        let mut bad = original.clone();
        bad.as_object_mut().unwrap().remove("observation");
        assert!(compare_values(bad, original, &core, &core).is_err());
    }

    #[test]
    fn complete_checkpoint_membership_and_order_must_match() {
        use super::super::SnapshotCheckpointEntry;
        use nes_workload::metroid::target::MetroidMechanicalState;
        let checkpoint = |core: &str| {
            let mut value = fixture(core);
            value["observation"] = json!({"frame_count":0,"decoded":MetroidMechanicalState::default(),
                "boss_defeats":{"kraid":false,"ridley":false},"mother_brain_status":0,
                "tourian_events":{"mother_brain_defeated":false,"escape_started":false},
                "endpoint_boss_slots":null,"changed_indices":[],"dead":false,"log_line":""});
            let snapshot: MetroidSnapshot = serde_json::from_value(value).unwrap();
            SnapshotCheckpoint {
                format: "fixture".to_owned(),
                entries: vec![
                    SnapshotCheckpointEntry {
                        id: 0,
                        snapshot: snapshot.clone(),
                    },
                    SnapshotCheckpointEntry { id: 7, snapshot },
                ],
            }
        };
        let old = "a".repeat(64);
        let new = "b".repeat(64);
        let original = checkpoint(&old);
        compare_checkpoints(&checkpoint(&new), &original, &new, &old).unwrap();
        let mut bad = checkpoint(&new);
        bad.entries.pop();
        assert!(compare_checkpoints(&bad, &original, &new, &old).is_err());
        let mut bad = checkpoint(&new);
        bad.entries.swap(0, 1);
        assert!(compare_checkpoints(&bad, &original, &new, &old).is_err());
        let mut bad = checkpoint(&new);
        bad.entries[1].id = 8;
        assert!(compare_checkpoints(&bad, &original, &new, &old).is_err());
        let mut bad = checkpoint(&new);
        bad.format.push('x');
        assert!(compare_checkpoints(&bad, &original, &new, &old).is_err());
    }
}
