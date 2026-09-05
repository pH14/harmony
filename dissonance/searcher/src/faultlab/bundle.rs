// SPDX-License-Identifier: AGPL-3.0-or-later

//! The action alphabet a workload bundle admits.
//!
//! The guest fault agent numbers nodes by their order among the bundle's `node`
//! lines, starting at zero, and runs a hook only for a declared id. An action
//! naming an absent node or hook is skipped by the agent rather than refused,
//! so a hardcoded alphabet would spend search budget on actions that cannot
//! perturb anything. The vocabulary is therefore read from the same bundle the
//! image is built from, and recorded into the stream so a replay draws the
//! identical alphabet.

use serde::{Deserialize, Serialize};

/// Prefix of the recorded vocabulary identifier.
pub const VOCABULARY_FORMAT: &str = "faultlab_bundle_v1";

/// The nodes and hooks one workload bundle declares.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultVocabulary {
    nodes: u16,
    hooks: Vec<u32>,
}

impl FaultVocabulary {
    /// Build a vocabulary directly.
    ///
    /// # Errors
    ///
    /// Returns an error when the bundle declares no node, or a duplicate hook.
    pub fn new(nodes: u16, hooks: Vec<u32>) -> Result<Self, String> {
        if nodes == 0 {
            return Err("a fault-library bundle must declare at least one node".to_owned());
        }
        let mut sorted = hooks.clone();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.len() != hooks.len() {
            return Err("a fault-library bundle declares a duplicate hook id".to_owned());
        }
        Ok(Self {
            nodes,
            hooks: sorted,
        })
    }

    /// Read the vocabulary from a bundle file's text.
    ///
    /// # Errors
    ///
    /// Returns an error on an unknown keyword, a malformed hook id, more nodes
    /// than a node id can carry, or a bundle with no node.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut nodes = 0_u16;
        let mut hooks = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            let keyword = fields.next().unwrap_or_default();
            let line_number = number.saturating_add(1);
            match keyword {
                "node" => {
                    if fields.next().is_none() {
                        return Err(format!("bundle line {line_number}: node has no name"));
                    }
                    nodes = nodes
                        .checked_add(1)
                        .ok_or("a fault-library bundle declares more nodes than a node id holds")?;
                }
                "hook" => {
                    let id = fields
                        .next()
                        .ok_or_else(|| format!("bundle line {line_number}: hook has no id"))?;
                    hooks.push(id.parse::<u32>().map_err(|error| {
                        format!("bundle line {line_number}: hook id {id:?} is not a u32: {error}")
                    })?);
                }
                "ready" => {}
                other => {
                    return Err(format!(
                        "bundle line {line_number}: unknown keyword {other:?}"
                    ));
                }
            }
        }
        Self::new(nodes, hooks)
    }

    /// Node ids the alphabet may fault, `0..nodes`.
    #[must_use]
    pub fn nodes(&self) -> u16 {
        self.nodes
    }

    /// Hook ids the alphabet may run, ascending.
    #[must_use]
    pub fn hooks(&self) -> &[u32] {
        &self.hooks
    }

    /// The identifier recorded in the stream header and report.
    #[must_use]
    pub fn identifier(&self) -> String {
        let hooks = self
            .hooks
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        format!("{VOCABULARY_FORMAT};nodes={};hooks={hooks}", self.nodes)
    }

    /// Resolve a recorded identifier back into a vocabulary.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown format or a malformed field.
    pub fn from_identifier(identifier: &str) -> Result<Self, String> {
        let mut fields = identifier.split(';');
        if fields.next() != Some(VOCABULARY_FORMAT) {
            return Err(format!(
                "fault-library vocabulary {identifier:?} is not {VOCABULARY_FORMAT}"
            ));
        }
        let nodes = fields
            .next()
            .and_then(|field| field.strip_prefix("nodes="))
            .ok_or("fault-library vocabulary has no node count")?
            .parse::<u16>()
            .map_err(|error| format!("fault-library vocabulary node count: {error}"))?;
        let hooks = fields
            .next()
            .and_then(|field| field.strip_prefix("hooks="))
            .ok_or("fault-library vocabulary has no hook list")?;
        if fields.next().is_some() {
            return Err("fault-library vocabulary has trailing fields".to_owned());
        }
        let hooks = if hooks.is_empty() {
            Vec::new()
        } else {
            hooks
                .split(',')
                .map(|id| {
                    id.parse::<u32>()
                        .map_err(|error| format!("fault-library vocabulary hook id: {error}"))
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        Self::new(nodes, hooks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The etcd bundle of the fault-library plan: one node, two hooks.
    const ETCD: &str = "\
node etcd /usr/bin/etcd --data-dir /tmp/etcd
hook 1 /hooks/put-batch
hook 2 /hooks/read-back
ready /usr/bin/etcdctl endpoint health
";

    /// The Postgres bundle of the fault-library plan: one node, four hooks.
    const POSTGRES: &str = "\
# CREATE INDEX CONCURRENTLY workload
node postgres /usr/local/pgsql/bin/postgres -D /pgdata

hook 1 /hooks/churn
hook 2 /hooks/cic-round
hook 3 /hooks/amcheck
hook 4 /hooks/vacuum
ready /usr/local/pgsql/bin/pg_isready
";

    /// The bundles the image ships, byte-for-byte. The guest fault agent parses
    /// the same files independently (`consonance/harmony-linux/fault-agent/src/
    /// bundle.rs`); the ids asserted here and there must agree, or the searcher
    /// explores an alphabet the agent never applies.
    const SHIPPED_ETCD: &str =
        include_str!("../../../../consonance/harmony-linux/linux/faultlab-etcd.bundle");
    const SHIPPED_PGCIC: &str =
        include_str!("../../../../consonance/harmony-linux/linux/faultlab-pgcic.bundle");

    #[test]
    fn the_shipped_etcd_bundle_has_node_0_and_hooks_1_2() {
        let vocabulary = FaultVocabulary::parse(SHIPPED_ETCD).expect("parse");
        assert_eq!(vocabulary.nodes(), 1);
        assert_eq!(vocabulary.hooks(), [1, 2]);
        assert_eq!(
            vocabulary.identifier(),
            "faultlab_bundle_v1;nodes=1;hooks=1,2"
        );
    }

    #[test]
    fn the_shipped_postgres_bundle_has_node_0_and_hooks_1_to_4() {
        let vocabulary = FaultVocabulary::parse(SHIPPED_PGCIC).expect("parse");
        assert_eq!(vocabulary.nodes(), 1);
        assert_eq!(vocabulary.hooks(), [1, 2, 3, 4]);
        assert_eq!(
            vocabulary.identifier(),
            "faultlab_bundle_v1;nodes=1;hooks=1,2,3,4"
        );
    }

    #[test]
    fn the_etcd_bundle_admits_one_node_and_two_hooks() {
        let vocabulary = FaultVocabulary::parse(ETCD).expect("parse");
        assert_eq!(vocabulary.nodes(), 1);
        assert_eq!(vocabulary.hooks(), [1, 2]);
    }

    #[test]
    fn the_postgres_bundle_admits_one_node_and_four_hooks() {
        let vocabulary = FaultVocabulary::parse(POSTGRES).expect("parse");
        assert_eq!(vocabulary.nodes(), 1);
        assert_eq!(vocabulary.hooks(), [1, 2, 3, 4]);
    }

    #[test]
    fn node_ids_follow_the_order_of_the_node_lines() {
        let vocabulary =
            FaultVocabulary::parse("node a /a\nhook 7 /h\nnode b /b\nnode c /c\nready /r\n")
                .expect("parse");
        assert_eq!(vocabulary.nodes(), 3, "ids 0, 1 and 2 are addressable");
        assert_eq!(vocabulary.hooks(), [7]);
    }

    #[test]
    fn a_bundle_with_no_node_is_refused() {
        assert!(FaultVocabulary::parse("hook 1 /h\nready /r\n").is_err());
        assert!(FaultVocabulary::parse("").is_err());
    }

    #[test]
    fn a_malformed_bundle_is_loud_rather_than_silently_narrowing_the_alphabet() {
        assert!(FaultVocabulary::parse("node a /a\nhook x /h\n").is_err());
        assert!(FaultVocabulary::parse("node a /a\nhook\n").is_err());
        assert!(FaultVocabulary::parse("node\n").is_err());
        assert!(FaultVocabulary::parse("node a /a\nservice s /s\n").is_err());
        assert!(FaultVocabulary::new(1, vec![1, 1]).is_err());
    }

    #[test]
    fn the_identifier_pins_the_alphabet_and_round_trips() {
        for text in [ETCD, POSTGRES] {
            let vocabulary = FaultVocabulary::parse(text).expect("parse");
            let identifier = vocabulary.identifier();
            assert_eq!(
                FaultVocabulary::from_identifier(&identifier).expect("resolve"),
                vocabulary
            );
        }
        let etcd = FaultVocabulary::parse(ETCD).expect("parse");
        let postgres = FaultVocabulary::parse(POSTGRES).expect("parse");
        assert_ne!(
            etcd.identifier(),
            postgres.identifier(),
            "two workloads never record the same alphabet"
        );
        assert_eq!(etcd.identifier(), "faultlab_bundle_v1;nodes=1;hooks=1,2");
    }

    #[test]
    fn an_unrecognized_identifier_is_refused() {
        for identifier in [
            "faultlab_bundle_v0;nodes=1;hooks=1",
            "faultlab_bundle_v1;nodes=1",
            "faultlab_bundle_v1;nodes=0;hooks=1",
            "faultlab_bundle_v1;nodes=x;hooks=1",
            "faultlab_bundle_v1;nodes=1;hooks=1;extra=2",
            "faultlab_bundle_v1;nodes=1;hooks=one",
        ] {
            assert!(
                FaultVocabulary::from_identifier(identifier).is_err(),
                "{identifier} must be refused"
            );
        }
        let no_hooks =
            FaultVocabulary::from_identifier("faultlab_bundle_v1;nodes=2;hooks=").expect("resolve");
        assert_eq!(no_hooks.nodes(), 2);
        assert!(no_hooks.hooks().is_empty());
    }
}
