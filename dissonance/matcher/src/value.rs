// SPDX-License-Identifier: AGPL-3.0-or-later
//! Canonical byte views of a spine [`Value`], the DSL's three uses of an
//! attribute value: glob comparison, stable hashing, and integer extraction.
//!
//! All three are **total and deterministic** — no floats (the spine [`Value`]
//! has no float variant by design), no panics, no locale, no iteration-order
//! surface — so a cell id or a bug fingerprint derived from them is stable
//! across runs and re-derivations (task-66 semantics 3).

use explorer::Value;

/// The **glob comparison** view: render a value to the byte string a DSL
/// pattern is matched against. Numbers render as their decimal ASCII, booleans
/// as `true`/`false`, strings and bytes verbatim — so a config author writes
/// `"error": "true"` or `"lsn": "4*"` and it matches the decoded value.
pub fn glob_bytes(v: &Value) -> Vec<u8> {
    match v {
        Value::Bool(true) => b"true".to_vec(),
        Value::Bool(false) => b"false".to_vec(),
        Value::Int(i) => i.to_string().into_bytes(),
        Value::UInt(u) => u.to_string().into_bytes(),
        Value::Str(s) => s.as_bytes().to_vec(),
        Value::Bytes(b) => b.clone(),
    }
}

/// The **stable-hash** view: a tagged, length-delimited canonical encoding that
/// distinguishes variants (so `Int(1)` and `Str("1")` never collide) and is
/// prefix-free (so a concatenation of encodings is unambiguous). Feeds the
/// `cell` role's `FeatureId` and the `never` role's `Bug` fingerprint.
pub fn canonical(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    match v {
        Value::Bool(b) => {
            out.push(0x00);
            out.push(u8::from(*b));
        }
        Value::Int(i) => {
            out.push(0x01);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::UInt(u) => {
            out.push(0x02);
            out.extend_from_slice(&u.to_le_bytes());
        }
        Value::Str(s) => {
            out.push(0x03);
            out.extend_from_slice(&(s.len() as u64).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        Value::Bytes(b) => {
            out.push(0x04);
            out.extend_from_slice(&(b.len() as u64).to_le_bytes());
            out.extend_from_slice(b);
        }
    }
    out
}

/// The **integer** view for the `state_max` register: `UInt` verbatim, a
/// non-negative `Int` widened, everything else (a string, a bool, a negative
/// int) a decode miss — `None`, never a panic (task-66 config semantics). A
/// negative int is a miss because the `state_max` register buckets a magnitude.
pub fn as_u64(v: &Value) -> Option<u64> {
    match v {
        Value::UInt(u) => Some(*u),
        Value::Int(i) if *i >= 0 => Some(*i as u64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_bytes_render_each_variant() {
        assert_eq!(glob_bytes(&Value::Bool(true)), b"true");
        assert_eq!(glob_bytes(&Value::Bool(false)), b"false");
        assert_eq!(glob_bytes(&Value::Int(-7)), b"-7");
        assert_eq!(glob_bytes(&Value::UInt(42)), b"42");
        assert_eq!(glob_bytes(&Value::Str("won".into())), b"won");
        assert_eq!(glob_bytes(&Value::Bytes(vec![1, 2, 3])), &[1, 2, 3]);
    }

    #[test]
    fn canonical_distinguishes_variants() {
        // Same "1" surface, different types → different canonical bytes.
        assert_ne!(
            canonical(&Value::Int(1)),
            canonical(&Value::Str("1".into()))
        );
        assert_ne!(canonical(&Value::Int(1)), canonical(&Value::UInt(1)));
        // Length delimiting keeps concatenation unambiguous: ("a","b") vs
        // ("ab","") must differ.
        let a_b = [
            canonical(&Value::Str("a".into())),
            canonical(&Value::Str("b".into())),
        ]
        .concat();
        let ab_empty = [
            canonical(&Value::Str("ab".into())),
            canonical(&Value::Str("".into())),
        ]
        .concat();
        assert_ne!(a_b, ab_empty);
    }

    #[test]
    fn as_u64_accepts_only_non_negative_integers() {
        assert_eq!(as_u64(&Value::UInt(9)), Some(9));
        assert_eq!(as_u64(&Value::Int(9)), Some(9));
        assert_eq!(as_u64(&Value::Int(0)), Some(0));
        assert_eq!(as_u64(&Value::Int(-1)), None);
        assert_eq!(as_u64(&Value::Str("9".into())), None);
        assert_eq!(as_u64(&Value::Bool(true)), None);
        assert_eq!(as_u64(&Value::Bytes(vec![9])), None);
    }
}
