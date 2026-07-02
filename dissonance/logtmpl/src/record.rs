// SPDX-License-Identifier: AGPL-3.0-or-later
//! The [`Matchable`] adapter — a log record plus its assigned template, adapted
//! to the matcher DSL (task 66) with **no crate dependency** between the two.
//!
//! The contract is exactly the spec's: `kind() == "log"`, `attr("msg")` is the
//! raw line, `attr("template")` is the template id, `attr("param.N")` is the
//! Nth extracted parameter, and `moment()` is the record's moment. Task 66's DSL
//! then matches `{ "kind": "log", "attr": { "msg": "database system is ready*" } }`
//! or on `template` / `param.N`, learning nothing about the codebook behind it.

use explorer::{Matchable, Moment, Value};

/// A decoded log line with its Drain-assigned template — the unit the matcher
/// DSL matches over. Owns its data (not borrowed from the trace) so a channel
/// plugin can serve it verbatim, per task 66's `ChannelSource` contract.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TemplateRecord {
    moment: Moment,
    msg: String,
    template: u64,
    params: Vec<String>,
}

impl TemplateRecord {
    /// Assemble a record from its decoded parts.
    pub fn new(moment: Moment, msg: String, template: u64, params: Vec<String>) -> Self {
        Self {
            moment,
            msg,
            template,
            params,
        }
    }

    /// The raw log line.
    pub fn msg(&self) -> &str {
        &self.msg
    }

    /// The assigned template id (the sensor's stable `FeatureId`).
    pub fn template(&self) -> u64 {
        self.template
    }

    /// The extracted parameters, in position order.
    pub fn params(&self) -> &[String] {
        &self.params
    }
}

/// The `param.` attribute prefix; `attr("param.N")` selects the Nth parameter.
const PARAM_PREFIX: &str = "param.";

impl Matchable for TemplateRecord {
    /// Log records discriminate as `"log"`.
    fn kind(&self) -> &str {
        "log"
    }

    /// The documented attribute surface: `msg` (raw line, string), `template`
    /// (the id, unsigned int), and `param.N` (the Nth parameter, string; absent
    /// when `N` is out of range or not a number). Total — any key is safe.
    fn attr(&self, k: &str) -> Option<Value> {
        match k {
            "msg" => Some(Value::Str(self.msg.clone())),
            "template" => Some(Value::UInt(self.template)),
            _ => {
                let n: usize = k.strip_prefix(PARAM_PREFIX)?.parse().ok()?;
                self.params.get(n).map(|p| Value::Str(p.clone()))
            }
        }
    }

    /// The moment the line was observed.
    fn moment(&self) -> Moment {
        self.moment
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec() -> TemplateRecord {
        TemplateRecord::new(
            Moment(7),
            "connection received port 5432 pid 991".to_string(),
            3,
            vec!["5432".to_string(), "991".to_string()],
        )
    }

    #[test]
    fn exposes_the_documented_attributes() {
        let r = rec();
        assert_eq!(r.kind(), "log");
        assert_eq!(r.moment(), Moment(7));
        assert_eq!(
            r.attr("msg"),
            Some(Value::Str("connection received port 5432 pid 991".into()))
        );
        assert_eq!(r.attr("template"), Some(Value::UInt(3)));
        assert_eq!(r.attr("param.0"), Some(Value::Str("5432".into())));
        assert_eq!(r.attr("param.1"), Some(Value::Str("991".into())));
    }

    #[test]
    fn absent_and_malformed_keys_are_none_not_panics() {
        let r = rec();
        assert_eq!(r.attr("param.2"), None, "out-of-range index");
        assert_eq!(r.attr("param.x"), None, "non-numeric index");
        assert_eq!(r.attr("param."), None, "empty index");
        assert_eq!(r.attr("nope"), None, "unknown key");
        assert_eq!(r.attr("paramX0"), None, "not the param prefix");
    }
}
