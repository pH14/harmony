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

        // pattern. No regex crate is on the whitelist, so this enforces exactly the
        // patterns the committed schemas use, keyed by their literal text — and PANICS
        // on any pattern it does not recognise, so a new one cannot slip in unchecked
        // (which is the whole failure this test exists to catch). A `pattern` is only
        // meaningful on a string.
        if let Some(Value::String(pat)) = schema.get("pattern")
            && let Some(s) = value.as_str()
            && !pattern_matches(pat, s)
        {
            errs.push(format!("{path}: {s:?} does not match pattern {pat}"));
        }

        // minLength.
        if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
            && let Some(s) = value.as_str()
            && (s.chars().count() as u64) < min
        {
            errs.push(format!("{path}: string shorter than minLength {min}"));
        }

        // minimum.
        if let Some(min) = schema.get("minimum").and_then(Value::as_i64)
            && let Some(n) = value.as_i64()
            && n < min
        {
            errs.push(format!("{path}: {n} is below minimum {min}"));
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

    /// Validate, returning the errors rather than asserting — for negative tests.
    fn errors(&self, value: &Value) -> Vec<String> {
        let mut errs = Vec::new();
        self.validate(&self.root, value, "$", &mut errs);
        errs
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

    // `reject-malformed-hash` is INTENTIONALLY schema-invalid (its empty sha256 is the
    // very thing the checker's well-formed gate catches, which serde's type check does
    // not), so it is the one fixture that must NOT conform. Every other fixture must.
    const INTENTIONALLY_SCHEMA_INVALID: &[&str] = &["reject-malformed-hash"];

    let entries = std::fs::read_dir(fixtures_dir()).expect("fixtures dir");
    for entry in entries {
        let dir = entry.expect("dir entry").path();
        if !dir.is_dir() {
            continue;
        }
        let name = dir.file_name().unwrap().to_string_lossy().to_string();

        let manifest = load(&dir.join("run-set.json"));
        if INTENTIONALLY_SCHEMA_INVALID.contains(&name.as_str()) {
            // Confirm it really is invalid (so the skip cannot mask a fixture that
            // silently became valid), then move on.
            assert!(
                !run_set_schema.errors(&manifest).is_empty(),
                "{name} is on the intentionally-invalid list but conforms to the schema"
            );
            continue;
        }
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
        checked_manifests >= 23,
        "expected ≥23 conforming fixture manifests, saw {checked_manifests}"
    );
    assert!(checked_records > 0, "no records were validated");
}

#[test]
fn the_validator_enforces_pattern_and_minimum_not_just_structure() {
    // The finding: a structural-only validator leaves malformed values (an empty MD5,
    // a zero sampling period) green, though the schemas constrain both with `pattern`
    // and `minimum`. These negative cases prove the keywords are enforced.
    let run_set_schema = Schema::from_file("run-set.schema.json");
    let record_schema = Schema::from_file("run-record.schema.json");

    // Start from a known-good fixture manifest and record.
    let base = fixtures_dir().join("accept");
    let mut manifest = load(&base.join("run-set.json"));
    let good_record: Value = serde_json::from_str(
        std::fs::read_to_string(base.join("records.jsonl"))
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert!(
        record_schema.errors(&good_record).is_empty(),
        "the base record should be valid"
    );

    // An empty MD5 pin violates `^[0-9a-f]{32}$`.
    manifest["images"][0]["md5"] = Value::String(String::new());
    assert!(
        !run_set_schema.errors(&manifest).is_empty(),
        "an empty md5 must fail the pattern"
    );

    // A malformed sha256 (wrong length) violates its pattern.
    let mut m2 = load(&base.join("run-set.json"));
    m2["images"][0]["sha256"] = Value::String("deadbeef".into());
    assert!(
        !run_set_schema.errors(&m2).is_empty(),
        "a short sha256 must fail the pattern"
    );

    // A zero sampling period violates `minimum: 1`.
    let mut m3 = load(&base.join("run-set.json"));
    m3["perf"]["sample_period"] = Value::from(0);
    assert!(
        !run_set_schema.errors(&m3).is_empty(),
        "a zero sample_period must fail the minimum"
    );

    // And a valid 32-hex md5 passes, so the matcher is not simply rejecting everything.
    let mut m4 = load(&base.join("run-set.json"));
    m4["images"][0]["md5"] = Value::String("0".repeat(32));
    assert!(
        run_set_schema.errors(&m4).is_empty(),
        "a valid md5 must pass: {:?}",
        run_set_schema.errors(&m4)
    );
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

/// Whether `s` matches a JSON-Schema `pattern`.
///
/// No regex crate is on the dependency whitelist, so this recognises exactly the two
/// patterns the committed schemas use — a sha256 (with an optional `sha256:` prefix)
/// and an md5 — and **panics on any other pattern string**. That is deliberate: a new
/// pattern the schemas start using must be handled here explicitly rather than
/// silently passing, which is the failure mode this whole test guards against.
fn pattern_matches(pattern: &str, s: &str) -> bool {
    match pattern {
        "^(sha256:)?[0-9a-f]{64}$" => {
            let hex = s.strip_prefix("sha256:").unwrap_or(s);
            is_lower_hex(hex, 64)
        }
        "^[0-9a-f]{32}$" => is_lower_hex(s, 32),
        other => panic!(
            "schema_conformance: unrecognised pattern {other:?} — add an explicit matcher \
             rather than letting it validate unchecked"
        ),
    }
}

/// Exactly `len` lowercase hex digits.
fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
