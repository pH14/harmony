// SPDX-License-Identifier: AGPL-3.0-or-later
//! Validate every committed fixture against the committed JSON Schemas.
//!
//! The drift test in `accept_reject.rs` proves the fixtures match the Rust
//! *generator*; it says nothing about the JSON Schemas under `schemas/`, which are
//! hand-written and can rot or drift from the Rust structs while the generator test
//! stays green. This test closes that: it loads `run-set.schema.json` and
//! `run-record.schema.json` and structurally validates every fixture manifest and
//! every JSONL record against them.
//!
//! No JSON-Schema validator is on the dependency whitelist, so this is a compact
//! validator for the subset the two schemas actually use — `type`, `properties`,
//! `required`, `additionalProperties: false`, `$ref` into `#/$defs`, arrays with
//! `items`, `const`, `enum`, and `oneOf` (for the nullable fields). It is deliberately
//! *strict* about unknown keys, because the whole point of validating against the
//! schema (rather than just round-tripping the Rust type) is to catch a schema that
//! promises `additionalProperties: false` but has drifted from what the records
//! actually carry — the schema_version bump and the two new overflow fields being the
//! immediate example.

use std::path::PathBuf;

use serde_json::Value;

fn schemas_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn fixtures_dir() -> PathBuf {
    schemas_dir().join("fixtures")
}

fn load(path: &std::path::Path) -> Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

/// The root schema plus its `$defs`, so `$ref` can be resolved.
struct Schema {
    root: Value,
    defs: Value,
}

impl Schema {
    fn from_file(name: &str) -> Schema {
        let doc = load(&schemas_dir().join(name));
        let defs = doc.get("$defs").cloned().unwrap_or(Value::Null);
        Schema { root: doc, defs }
    }

    /// Resolve a `#/$defs/<name>` reference.
    fn resolve<'a>(&'a self, reference: &str) -> &'a Value {
        let name = reference
            .strip_prefix("#/$defs/")
            .unwrap_or_else(|| panic!("only #/$defs refs are supported, got {reference}"));
        self.defs
            .get(name)
            .unwrap_or_else(|| panic!("unresolved $ref {reference}"))
    }

    /// Validate `value` against `schema`, collecting failures under `path`.
    fn validate(&self, schema: &Value, value: &Value, path: &str, errs: &mut Vec<String>) {
        // $ref: resolve and recurse.
        if let Some(Value::String(r)) = schema.get("$ref") {
            let target = self.resolve(r);
            self.validate(target, value, path, errs);
            return;
        }

        // const.
        if let Some(expected) = schema.get("const")
            && value != expected
        {
            errs.push(format!("{path}: expected const {expected}, found {value}"));
        }

        // enum.
        if let Some(Value::Array(options)) = schema.get("enum")
            && !options.contains(value)
        {
            errs.push(format!("{path}: {value} is not one of {options:?}"));
        }

        // oneOf: exactly one branch must validate (used for the nullable fields).
        if let Some(Value::Array(branches)) = schema.get("oneOf") {
            let matches = branches
                .iter()
                .filter(|b| {
                    let mut local = Vec::new();
                    self.validate(b, value, path, &mut local);
                    local.is_empty()
                })
                .count();
            if matches != 1 {
                errs.push(format!(
                    "{path}: {matches} of {} oneOf branches matched (want exactly 1)",
                    branches.len()
                ));
            }
        }

        // type.
        if let Some(Value::String(ty)) = schema.get("type") {
            let ok = match ty.as_str() {
                "object" => value.is_object(),
                "array" => value.is_array(),
                "string" => value.is_string(),
                // serde_json distinguishes int/float; the schemas use "integer".
                "integer" => value.is_u64() || value.is_i64(),
                "number" => value.is_number(),
                "boolean" => value.is_boolean(),
                "null" => value.is_null(),
                other => panic!("unhandled schema type {other}"),
            };
            if !ok {
                errs.push(format!("{path}: expected type {ty}, found {value}"));
                return;
            }
        }

        // object: required, properties, additionalProperties: false.
        if let Some(obj) = value.as_object()
            && schema.get("type").and_then(Value::as_str) == Some("object")
        {
            let props = schema.get("properties").and_then(Value::as_object);
            if let Some(Value::Array(required)) = schema.get("required") {
                for req in required {
                    let key = req.as_str().unwrap();
                    if !obj.contains_key(key) {
                        errs.push(format!("{path}: missing required key `{key}`"));
                    }
                }
            }
            let additional = schema
                .get("additionalProperties")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            for (k, v) in obj {
                match props.and_then(|p| p.get(k)) {
                    Some(sub) => self.validate(sub, v, &format!("{path}.{k}"), errs),
                    None if !additional => {
                        errs.push(format!(
                            "{path}: unexpected key `{k}` (additionalProperties=false)"
                        ));
                    }
                    None => {}
                }
            }
        }

        // array: items.
        if let Some(arr) = value.as_array()
            && let Some(items) = schema.get("items")
        {
            for (i, v) in arr.iter().enumerate() {
                self.validate(items, v, &format!("{path}[{i}]"), errs);
            }
        }
    }

    fn check(&self, value: &Value, what: &str) {
        let mut errs = Vec::new();
        self.validate(&self.root, value, what, &mut errs);
        assert!(
            errs.is_empty(),
            "{what} does not conform to the committed schema:\n  {}",
            errs.join("\n  ")
        );
    }
}

#[test]
fn every_fixture_conforms_to_the_committed_json_schemas() {
    let run_set_schema = Schema::from_file("run-set.schema.json");
    let record_schema = Schema::from_file("run-record.schema.json");

    let mut checked_manifests = 0;
    let mut checked_records = 0;

    let entries = std::fs::read_dir(fixtures_dir()).expect("fixtures dir");
    for entry in entries {
        let dir = entry.expect("dir entry").path();
        if !dir.is_dir() {
            continue;
        }
        let name = dir.file_name().unwrap().to_string_lossy().to_string();

        let manifest = load(&dir.join("run-set.json"));
        run_set_schema.check(&manifest, &format!("{name}/run-set.json"));
        checked_manifests += 1;

        let records_text = std::fs::read_to_string(dir.join("records.jsonl"))
            .unwrap_or_else(|e| panic!("reading {name}/records.jsonl: {e}"));
        for (i, line) in records_text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let record: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("{name}/records.jsonl line {}: {e}", i + 1));
            record_schema.check(&record, &format!("{name}/records.jsonl:{}", i + 1));
            checked_records += 1;
        }
    }

    // The gate is only meaningful if it actually validated something.
    assert!(
        checked_manifests >= 19,
        "expected ≥19 fixture manifests, saw {checked_manifests}"
    );
    assert!(checked_records > 0, "no records were validated");
}

#[test]
fn the_schema_version_const_matches_the_rust_constant() {
    // The drift the finding warned about, made a direct assertion: the JSON schema's
    // pinned schema_version must equal the Rust SCHEMA_VERSION, or a v2 record would
    // validate against a v1 schema (or vice versa) and the two would silently part.
    let schema = load(&schemas_dir().join("run-set.schema.json"));
    let pinned = schema["properties"]["schema_version"]["const"]
        .as_u64()
        .expect("schema_version const is an integer");
    assert_eq!(
        pinned,
        u64::from(arm_harness::evidence::SCHEMA_VERSION),
        "run-set.schema.json pins schema_version {pinned} but the Rust SCHEMA_VERSION is {}",
        arm_harness::evidence::SCHEMA_VERSION
    );
}
