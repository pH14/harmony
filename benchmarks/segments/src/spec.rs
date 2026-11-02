// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub name: String,
    pub game: String,
    pub rom_sha256: String,
    pub cartridge_ram: bool,
    pub start: Start,
    pub milestone: Vec<Predicate>,
    pub work_budget: u64,
    pub workers: u32,
    pub memory_limit: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Start {
    pub tape: PathBuf,
    pub tape_sha256: String,
    pub core_state_sha256: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Predicate {
    pub address: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ne: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ge: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub le: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bits: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<u16>,
}

impl Predicate {
    fn comparisons(&self) -> usize {
        [
            self.eq.is_some(),
            self.ne.is_some(),
            self.ge.is_some(),
            self.le.is_some(),
            self.bits.is_some(),
            self.bucket.is_some(),
        ]
        .into_iter()
        .filter(|set| *set)
        .count()
    }

    pub fn holds(&self, value: u8, start: u8) -> bool {
        if let Some(x) = self.eq {
            value == x
        } else if let Some(x) = self.ne {
            value != x
        } else if let Some(x) = self.ge {
            value >= x
        } else if let Some(x) = self.le {
            value <= x
        } else if let Some(mask) = self.bits {
            value & mask == mask
        } else if let Some(width) = self.bucket {
            u16::from(value) / width != u16::from(start) / width
        } else {
            false
        }
    }
}

pub struct Loaded {
    pub spec: Spec,
    pub tape: PathBuf,
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Spec {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_name(&self.name) {
            return Err("name must be lower case letters, digits and hyphens".into());
        }
        if self.game.is_empty() {
            return Err("game is empty".into());
        }
        for (field, value) in [
            ("rom_sha256", &self.rom_sha256),
            ("start.tape_sha256", &self.start.tape_sha256),
            ("start.core_state_sha256", &self.start.core_state_sha256),
        ] {
            if !is_sha256(value) {
                return Err(format!("{field} must be 64 lower case hexadecimal digits"));
            }
        }
        if self.milestone.is_empty() {
            return Err("milestone needs at least one predicate".into());
        }
        for predicate in &self.milestone {
            let address = predicate.address;
            let system = address <= 0x07ff;
            let cartridge = (0x6000..=0x7fff).contains(&address);
            if !system && !cartridge {
                return Err(format!(
                    "address {address:#06x} is outside $0000-$07FF and $6000-$7FFF"
                ));
            }
            if cartridge && !self.cartridge_ram {
                return Err(format!(
                    "address {address:#06x} reads cartridge RAM but cartridge_ram is false"
                ));
            }
            let count = predicate.comparisons();
            if count != 1 {
                return Err(format!(
                    "predicate on {address:#06x} has {count} comparisons; it needs exactly one of eq, ne, ge, le, bits and bucket"
                ));
            }
            if predicate.bits == Some(0) {
                return Err(format!(
                    "predicate on {address:#06x} has an empty bits mask"
                ));
            }
            if predicate.bucket.is_some_and(|w| w == 0 || w > 255) {
                return Err(format!(
                    "predicate on {address:#06x} has a bucket width outside 1-255"
                ));
            }
        }
        if self.work_budget == 0 {
            return Err("work_budget is zero".into());
        }
        if self.workers == 0 {
            return Err("workers is zero".into());
        }
        if self.memory_limit == 0 {
            return Err("memory_limit is zero".into());
        }
        Ok(())
    }

    pub fn request(&self, tape: &Path, seed: u64, workers: Option<u32>) -> Result<Value, String> {
        let mut value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        value["start"]["tape"] = Value::String(tape.display().to_string());
        value["seed"] = Value::from(seed);
        if let Some(workers) = workers {
            value["workers"] = Value::from(workers);
        }
        Ok(value)
    }
}

pub fn load(path: &Path) -> Result<Loaded, String> {
    let text = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec: Spec =
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    spec.validate()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let tape = fs::canonicalize(dir.join(&spec.start.tape)).map_err(|e| {
        format!(
            "{}: tape {}: {e}",
            path.display(),
            spec.start.tape.display()
        )
    })?;
    let bytes = fs::read(&tape).map_err(|e| format!("{}: {e}", tape.display()))?;
    if sha256_hex(&bytes) != spec.start.tape_sha256 {
        return Err(format!(
            "{}: tape {} does not match tape_sha256",
            path.display(),
            tape.display()
        ));
    }
    Ok(Loaded { spec, tape })
}

pub fn load_all(path: &Path) -> Result<Vec<Loaded>, String> {
    let mut files = Vec::new();
    if path.is_dir() {
        for entry in fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))? {
            let file = entry.map_err(|e| e.to_string())?.path();
            if file.extension().is_some_and(|x| x == "json") {
                files.push(file);
            }
        }
        files.sort();
    } else {
        files.push(path.to_path_buf());
    }
    let loaded = files
        .iter()
        .map(|file| load(file))
        .collect::<Result<Vec<_>, _>>()?;
    let mut names: Vec<&str> = loaded.iter().map(|l| l.spec.name.as_str()).collect();
    names.sort_unstable();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("two specs share a name".into());
    }
    if loaded.is_empty() {
        return Err(format!("no specs in {}", path.display()));
    }
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(milestone: Value, cartridge_ram: bool) -> Result<Spec, String> {
        let value = serde_json::json!({
            "name": "smb-8-4",
            "game": "smb",
            "rom_sha256": "a".repeat(64),
            "cartridge_ram": cartridge_ram,
            "start": {"tape": "t.tape", "tape_sha256": "b".repeat(64), "core_state_sha256": "c".repeat(64)},
            "milestone": milestone,
            "work_budget": 1024,
            "workers": 2,
            "memory_limit": 1 << 30,
        });
        let spec: Spec = serde_json::from_value(value).map_err(|e| e.to_string())?;
        spec.validate()?;
        Ok(spec)
    }

    #[test]
    fn predicates_validate() {
        assert!(
            spec(
                serde_json::json!([
                    {"address": 1904, "eq": 2},
                    {"address": 0, "ne": 1},
                    {"address": 2047, "ge": 3},
                    {"address": 5, "le": 4},
                    {"address": 6, "bits": 129},
                    {"address": 7, "bucket": 16}
                ]),
                false
            )
            .is_ok()
        );
        assert!(spec(serde_json::json!([{"address": 24576, "eq": 1}]), true).is_ok());
        for bad in [
            serde_json::json!([]),
            serde_json::json!([{"address": 2048, "eq": 1}]),
            serde_json::json!([{"address": 24576, "eq": 1}]),
            serde_json::json!([{"address": 32768, "eq": 1}]),
            serde_json::json!([{"address": 1, "eq": 1, "ne": 2}]),
            serde_json::json!([{"address": 1}]),
            serde_json::json!([{"address": 1, "eq": 256}]),
            serde_json::json!([{"address": 1, "bits": 0}]),
            serde_json::json!([{"address": 1, "bucket": 0}]),
            serde_json::json!([{"address": 1, "bucket": 256}]),
            serde_json::json!([{"address": 1, "outside": [1, 2]}]),
        ] {
            assert!(spec(bad.clone(), false).is_err(), "{bad} validated");
        }
    }

    #[test]
    fn predicates_hold() {
        let p = |v: Value| serde_json::from_value::<Predicate>(v).unwrap();
        assert!(p(serde_json::json!({"address": 1, "eq": 2})).holds(2, 0));
        assert!(!p(serde_json::json!({"address": 1, "ne": 2})).holds(2, 0));
        assert!(p(serde_json::json!({"address": 1, "ge": 2})).holds(3, 0));
        assert!(!p(serde_json::json!({"address": 1, "le": 2})).holds(3, 0));
        assert!(p(serde_json::json!({"address": 1, "bits": 5})).holds(7, 0));
        assert!(!p(serde_json::json!({"address": 1, "bits": 5})).holds(6, 0));
        let bucket = p(serde_json::json!({"address": 1, "bucket": 32}));
        assert!(!bucket.holds(63, 40));
        assert!(bucket.holds(64, 40));
        assert!(bucket.holds(31, 40));
    }

    #[test]
    fn names_and_hashes_validate() {
        let mut good = spec(serde_json::json!([{"address": 1, "eq": 2}]), false).unwrap();
        good.name = "Bad Name".into();
        assert!(good.validate().is_err());
        good.name = "ok-1".into();
        good.rom_sha256 = "A".repeat(64);
        assert!(good.validate().is_err());
    }

    #[test]
    fn request_adds_seed_and_absolute_tape() {
        let spec = spec(serde_json::json!([{"address": 1, "eq": 2}]), false).unwrap();
        let request = spec.request(Path::new("/abs/t.tape"), 7, Some(3)).unwrap();
        assert_eq!(request["seed"], 7);
        assert_eq!(request["workers"], 3);
        assert_eq!(request["start"]["tape"], "/abs/t.tape");
        assert_eq!(request["milestone"][0]["eq"], 2);
    }
}
