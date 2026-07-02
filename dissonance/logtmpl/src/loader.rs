// SPDX-License-Identifier: AGPL-3.0-or-later
//! The thin fixture loader — **test scaffolding, not a decoder** (raw console →
//! `Record` decoding is task 65's job; this crate consumes decoded records).
//!
//! It turns captured console text into the scrape-tier record stream the sensor
//! consumes: each line becomes a `Record { kind: "log", attrs: { "msg": … } }`
//! stamped at a synthetic [`Moment`] equal to its zero-based line index. That
//! one-for-one line-index→moment mapping is what the fixtures' gates key on.

use std::collections::BTreeMap;

use explorer::{Moment, Record, Value};

/// The record kind the loader stamps (matching [`LogSensor`](crate::LogSensor)).
const LOG_KIND: &str = "log";
/// The attribute key the raw line lands under.
const MSG_ATTR: &str = "msg";

/// Decode console text into a scrape-tier record stream: one `"log"` record per
/// line, at `Moment(line_index)`. Total over any `&str`.
pub fn load_console_log(text: &str) -> Vec<(Moment, Record)> {
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            let mut attrs = BTreeMap::new();
            attrs.insert(MSG_ATTR.to_string(), Value::Str(line.to_string()));
            (
                Moment(i as u64),
                Record {
                    kind: LOG_KIND.to_string(),
                    attrs,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_log_record_per_line_at_its_index() {
        let recs = load_console_log("first line\nsecond 2\n");
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].0, Moment(0));
        assert_eq!(recs[1].0, Moment(1));
        assert_eq!(recs[0].1.kind, "log");
        assert_eq!(
            recs[0].1.attrs.get("msg"),
            Some(&Value::Str("first line".into()))
        );
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(load_console_log("").is_empty());
    }
}
