// SPDX-License-Identifier: AGPL-3.0-or-later
//! Offline comparison of recorded numeric endpoints under the compiled key.

use nes_workload::{
    metroid::{
        archive::{KEY_POLICY_IDENTIFIER, MetroidArchiveKey, archive_key},
        target::MetroidMechanicalState,
    },
    search::archive::ArchiveKey,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{error::Error, fs, path::Path};

#[derive(Deserialize)]
struct Pair {
    pair: usize,
    execution: u64,
    discarded: Value,
    survivor: Value,
}
#[derive(Deserialize)]
struct Audit {
    audit_sha256: String,
    pairs: Vec<Pair>,
}

fn key(mut endpoint: Value) -> Result<MetroidArchiveKey, Box<dyn Error>> {
    let object = endpoint
        .as_object_mut()
        .ok_or("endpoint must be an object")?;
    // The numeric export omits these fields because neither enters the key.
    object.entry("mode").or_insert(json!(0));
    object.entry("ending").or_insert(json!(false));
    let state: MetroidMechanicalState = serde_json::from_value(endpoint)?;
    Ok(archive_key(state))
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 1 {
        return Err("usage: metroid-key-audit NUMERIC-ENDPOINTS.json".into());
    }
    let path = Path::new(&args[0]);
    if fs::metadata(path)?.len() > 1_048_576 {
        return Err("numeric audit exceeds 1 MiB".into());
    }
    let bytes = fs::read(path)?;
    let audit: Audit = serde_json::from_slice(&bytes)?;
    if audit.pairs.is_empty() || audit.pairs.len() > 256 {
        return Err("numeric audit must contain 1..256 pairs".into());
    }
    let mut rows = Vec::new();
    for pair in audit.pairs {
        let (a, b) = (key(pair.discarded)?, key(pair.survivor)?);
        rows.push(json!({"pair":pair.pair,"execution":pair.execution,
            "retention_split":a.group(0)!=b.group(0),
            "coarser_groups_equal":(1..MetroidArchiveKey::groups()).all(|d|a.group(d)==b.group(d))}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "format":"metroid-compiled-key-audit-v1","key_policy":KEY_POLICY_IDENTIFIER,
            "input_sha256":format!("{:x}",Sha256::digest(bytes)),"source_audit_sha256":audit.audit_sha256,
            "pairs":rows.len(),"retention_splits":rows.iter().filter(|r|r["retention_split"]==true).count(),
            "all_coarser_groups_equal":rows.iter().all(|r|r["coarser_groups_equal"]==true),"rows":rows,
            "scope":"offline numeric endpoint partition; no emulator execution or future-equivalence claim"
        }))?
    );
    Ok(())
}
