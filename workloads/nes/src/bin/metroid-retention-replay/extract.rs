// SPDX-License-Identifier: AGPL-3.0-or-later
//! Export one prospectively selected complete record. No emulator is constructed.
use super::{
    CaptureIdentity, CaptureReader, Path, Pinned, Result, fs, json, pinned, sha, valid_sha, write,
};
use nes_workload::metroid::retention_capture::RetentionSnapshotRecord;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    capture: Pinned,
    identity: CaptureIdentity,
    expected_records: u64,
    ordinal: u64,
    execution: u64,
    incumbent_id: u64,
    replaces: bool,
    candidate_snapshot_sha256: String,
    incumbent_snapshot_sha256: String,
    candidate_input_sha256: String,
    incumbent_input_sha256: String,
}

fn capture_hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = [0; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > 512 * 1024 * 1024 {
            return Err("capture exceeds 512 MiB".into());
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn selected(q: &Request, record: &RetentionSnapshotRecord) -> Result<[Vec<u8>; 4]> {
    let snapshots = [
        postcard::to_allocvec(&record.candidate_snapshot)?,
        postcard::to_allocvec(
            record
                .incumbent_snapshot
                .as_ref()
                .ok_or("missing incumbent")?,
        )?,
        serde_json::to_vec(&record.candidate_input)?,
        serde_json::to_vec(&record.incumbent_input)?,
    ];
    if (
        record.ordinal,
        record.execution,
        record.incumbent_id,
        record.replaces,
    ) != (q.ordinal, q.execution, q.incumbent_id, q.replaces)
        || snapshots.iter().map(|b| sha(b)).collect::<Vec<_>>()
            != [
                q.candidate_snapshot_sha256.as_str(),
                q.incumbent_snapshot_sha256.as_str(),
                q.candidate_input_sha256.as_str(),
                q.incumbent_input_sha256.as_str(),
            ]
    {
        return Err("selected competition identity differs".into());
    }
    Ok(snapshots)
}

pub(super) fn run(request: &Path, output: &Path) -> Result<()> {
    let bytes = super::read(request)?;
    let q: Request = serde_json::from_slice(&bytes)?;
    if !(1..=5000).contains(&q.expected_records)
        || !(1..=q.expected_records).contains(&q.ordinal)
        || !valid_sha(&q.capture.sha256)
        || capture_hash(&q.capture.path)? != q.capture.sha256
    {
        return Err("invalid or changed capture".into());
    }
    let mut reader = CaptureReader::open(&q.capture.path)?;
    if reader.identity() != &q.identity {
        return Err("capture stream/origin identity differs".into());
    }
    let mut count = 0;
    let mut record = None;
    // Continue through terminal None: a selected prefix is not a complete capture.
    while let Some(row) = reader.next_competition()? {
        count += 1;
        if row.ordinal == q.ordinal {
            if record.is_some() {
                return Err("duplicate ordinal".into());
            }
            record = Some(row);
        }
    }
    if count != q.expected_records || capture_hash(&q.capture.path)? != q.capture.sha256 {
        return Err("capture count or identity changed".into());
    }
    let record = record.ok_or("selected record missing")?;
    let payloads = selected(&q, &record)?;
    fs::create_dir(output)?;
    let mut files = Vec::new();
    for (name, payload) in [
        "candidate.snapshot.bin",
        "incumbent.snapshot.bin",
        "candidate.input.json",
        "incumbent.input.json",
    ]
    .into_iter()
    .zip(payloads)
    {
        let path = output.join(name);
        use std::io::Write;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?
            .write_all(&payload)?;
        // Check serialized files using the same bounded reader used by replay.
        pinned(&Pinned {
            path,
            sha256: sha(&payload),
        })?;
        files.push(json!({"file":name,"sha256":sha(&payload),"bytes":payload.len()}));
    }
    write(&output.join("record.json"), &record)?;
    write(
        &output.join("manifest.json"),
        &json!({
            "format":"metroid-retention-extract-v1", "request_sha256":sha(&bytes),
            "capture_sha256":q.capture.sha256, "identity":q.identity,
            "verified_complete_records":count,"selected_ordinal":q.ordinal,"files":files,
            "physical_frames":0,"emulator_constructed":false,
            "scope":"Exact full snapshots; inputs are checkpoint-local, not boot-replay witnesses."
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_frozen_record_must_match_all_four_identities() {
        let q: Request = serde_json::from_str(include_str!(
            "../../../../../benchmarks/search/continuation-reassessment/pc01-extract-request.json"
        ))
        .unwrap();
        // Published RR02 full record permits checking selection without emulation.
        let record: RetentionSnapshotRecord = serde_json::from_str(include_str!(
            "../../../../../benchmarks/search/continuation-reassessment/pc01-pair/record.json"
        ))
        .unwrap();
        selected(&q, &record).unwrap();
        let mut wrong = record.clone();
        wrong.replaces = !wrong.replaces;
        assert!(selected(&q, &wrong).is_err());
        let mut wrong = record.clone();
        wrong.incumbent_snapshot = None;
        assert!(selected(&q, &wrong).is_err());
        let mut wrong = record;
        wrong.candidate_input.actions.clear();
        assert!(selected(&q, &wrong).is_err());
    }
}
