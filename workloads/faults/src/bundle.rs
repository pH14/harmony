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
//!
//! A park needs a place: a user instruction address in the node's binary. The
//! list is generated from the binary's line table and given to the search with
//! `--places`; the stream records its count and a digest, and a resumed
//! campaign must be given a list with the same digest.

use serde::{Deserialize, Serialize};

/// Prefix of the recorded vocabulary identifier.
pub const VOCABULARY_FORMAT: &str = "faultlab_bundle_v1";
/// Largest node count the guest fault agent accepts: its alive bitmap is one
/// `u64`, one bit per node, so a wider vocabulary would name nodes the agent
/// never runs.
pub const MAX_NODES: u16 = 64;

/// The nodes and hooks one workload bundle declares, and the places a park
/// may name.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultVocabulary {
    nodes: u16,
    hooks: Vec<u32>,
    places: Vec<u64>,
    /// The place list's `(count, digest)` when the vocabulary was resolved
    /// from a recorded identifier and the addresses themselves are not at hand.
    places_digest: Option<(u64, u64)>,
}

/// FNV-1a over the sorted place addresses, the digest the identifier carries.
fn digest_places(places: &[u64]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for addr in places {
        for byte in addr.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

/// Parse a place list: one hex address per line, with or without `0x`, blank
/// lines and `#` comments ignored.
///
/// # Errors
///
/// Returns an error naming the line that is not a hex address.
pub fn parse_places(text: &str) -> Result<Vec<u64>, String> {
    let mut places = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let word = line.split('#').next().unwrap_or_default().trim();
        if word.is_empty() {
            continue;
        }
        let hex = word.strip_prefix("0x").unwrap_or(word);
        let addr = u64::from_str_radix(hex, 16).map_err(|error| {
            format!(
                "places line {}: {word:?} is not a hex address: {error}",
                number + 1
            )
        })?;
        places.push(addr);
    }
    Ok(places)
}

impl FaultVocabulary {
    /// Build a vocabulary directly.
    ///
    /// # Errors
    ///
    /// Returns an error when the bundle declares no node, more nodes than the
    /// guest agent can supervise, or a duplicate hook.
    pub fn new(nodes: u16, hooks: Vec<u32>) -> Result<Self, String> {
        if nodes == 0 {
            return Err("a fault bundle must declare at least one node".to_owned());
        }
        if nodes > MAX_NODES {
            return Err(format!(
                "a fault bundle declares {nodes} nodes; the guest agent supervises at most {MAX_NODES}"
            ));
        }
        let mut sorted = hooks.clone();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.len() != hooks.len() {
            return Err("a fault bundle declares a duplicate hook id".to_owned());
        }
        Ok(Self {
            nodes,
            hooks: sorted,
            places: Vec::new(),
            places_digest: None,
        })
    }

    /// The vocabulary with `places` a park may name, sorted and deduplicated.
    ///
    /// # Errors
    ///
    /// Returns an error when the vocabulary was resolved from a recorded
    /// identifier whose place digest does not match `places`.
    pub fn with_places(mut self, mut places: Vec<u64>) -> Result<Self, String> {
        places.sort_unstable();
        places.dedup();
        let digest = (places.len() as u64, digest_places(&places));
        if let Some(recorded) = self.places_digest
            && recorded != digest
        {
            return Err(format!(
                "the place list ({} places, digest {:#018x}) is not the recorded one ({} places, digest {:#018x})",
                digest.0, digest.1, recorded.0, recorded.1
            ));
        }
        self.places = places;
        self.places_digest = None;
        Ok(self)
    }

    /// Read the vocabulary from a bundle file's text.
    ///
    /// # Errors
    ///
    /// Returns an error on an unknown keyword, a malformed hook id, more nodes
    /// than a node id can carry, or a bundle with no node.
    pub fn parse(text: &str) -> Result<Self, String> {
        // Validate the complete bundle first. The alphabet parser only needs
        // node and hook counts, but investigation also relies on declaration
        // syntax and references being checked rather than silently ignored.
        crate::declarations::Declarations::parse(text)?;

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
                        .ok_or("a fault bundle declares more nodes than a node id holds")?;
                }
                "hook" => {
                    let id = fields
                        .next()
                        .ok_or_else(|| format!("bundle line {line_number}: hook has no id"))?;
                    hooks.push(id.parse::<u32>().map_err(|error| {
                        format!("bundle line {line_number}: hook id {id:?} is not a u32: {error}")
                    })?);
                }
                // Declaration lines carry meanings for inspection, not
                // alphabet entries. `Declarations` validated them above.
                "ready" | "setup" | "describe" | "assert" | "diagnostic" => {}
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

    /// Places a park may name, ascending. Empty when the campaign was given
    /// no place list, and when the vocabulary was resolved from a recorded
    /// identifier and the list has not been supplied again.
    #[must_use]
    pub fn places(&self) -> &[u64] {
        &self.places
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
        let (count, digest) = self
            .places_digest
            .unwrap_or_else(|| (self.places.len() as u64, digest_places(&self.places)));
        if count == 0 {
            return format!("{VOCABULARY_FORMAT};nodes={};hooks={hooks}", self.nodes);
        }
        format!(
            "{VOCABULARY_FORMAT};nodes={};hooks={hooks};places={count}/{digest:016x}",
            self.nodes
        )
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
                "fault vocabulary {identifier:?} is not {VOCABULARY_FORMAT}"
            ));
        }
        let nodes = fields
            .next()
            .and_then(|field| field.strip_prefix("nodes="))
            .ok_or("fault vocabulary has no node count")?
            .parse::<u16>()
            .map_err(|error| format!("fault vocabulary node count: {error}"))?;
        let hooks = fields
            .next()
            .and_then(|field| field.strip_prefix("hooks="))
            .ok_or("fault vocabulary has no hook list")?;
        let places_digest = match fields.next() {
            None => None,
            Some(field) => {
                let (count, digest) = field
                    .strip_prefix("places=")
                    .and_then(|places| places.split_once('/'))
                    .ok_or("fault vocabulary has an unknown field")?;
                let count = count
                    .parse::<u64>()
                    .map_err(|error| format!("fault vocabulary place count: {error}"))?;
                let digest = u64::from_str_radix(digest, 16)
                    .map_err(|error| format!("fault vocabulary place digest: {error}"))?;
                Some((count, digest))
            }
        };
        if fields.next().is_some() {
            return Err("fault vocabulary has trailing fields".to_owned());
        }
        let hooks = if hooks.is_empty() {
            Vec::new()
        } else {
            hooks
                .split(',')
                .map(|id| {
                    id.parse::<u32>()
                        .map_err(|error| format!("fault vocabulary hook id: {error}"))
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut vocabulary = Self::new(nodes, hooks)?;
        vocabulary.places_digest = places_digest;
        Ok(vocabulary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-node, two-hook bundle.
    const ETCD: &str = "\
setup /bin/sh -c \"mkdir -p /tmp/etcd\"
node etcd /usr/bin/etcd --data-dir /tmp/etcd
hook 1 /hooks/put-batch
hook 2 /hooks/read-back
ready /usr/bin/etcdctl endpoint health
";

    /// A one-node, four-hook bundle.
    const POSTGRES: &str = "\
# CREATE INDEX CONCURRENTLY workload
node postgres /usr/local/pgsql/bin/postgres -D /pgdata

hook 1 /hooks/churn
hook 2 /hooks/cic-round
hook 3 /hooks/amcheck
hook 4 /hooks/vacuum
ready /usr/local/pgsql/bin/pg_isready
";

    #[test]
    fn places_round_trip_through_the_identifier_as_a_digest() {
        let vocabulary = FaultVocabulary::parse(ETCD)
            .expect("parse")
            .with_places(vec![0x4b0e86, 0x47eca0, 0x4b0e86])
            .expect("places");
        assert_eq!(vocabulary.places(), [0x47eca0, 0x4b0e86]);
        let identifier = vocabulary.identifier();
        assert!(identifier.starts_with("faultlab_bundle_v1;nodes=1;hooks=1,2;places=2/"));
        let resolved = FaultVocabulary::from_identifier(&identifier).expect("resolve");
        assert_eq!(resolved.identifier(), identifier);
        assert!(resolved.places().is_empty());
        // The same list is accepted again, a different one is refused.
        let again = resolved
            .clone()
            .with_places(vec![0x47eca0, 0x4b0e86])
            .expect("same places");
        assert_eq!(again, vocabulary);
        assert!(resolved.with_places(vec![0x47eca0]).is_err());
    }

    #[test]
    fn a_place_list_parses_hex_with_comments() {
        assert_eq!(
            parse_places("# places\n0x4b0e86\n47eca0 # entry\n\n").expect("parse"),
            [0x4b0e86, 0x47eca0]
        );
        assert!(parse_places("0x4b0e86\nnope\n").is_err());
    }

    #[test]
    fn a_setup_line_declares_no_node_or_hook() {
        let vocabulary = FaultVocabulary::parse("setup /bin/prep\nnode a /a\n").expect("parse");
        assert_eq!(vocabulary.nodes(), 1);
        assert!(vocabulary.hooks().is_empty());
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
    fn a_bundle_past_the_agent_node_limit_is_refused() {
        let nodes = |count: u16| {
            (0..count)
                .map(|index| format!("node n{index} /bin/true\n"))
                .chain(std::iter::once("ready /bin/true\n".to_owned()))
                .collect::<String>()
        };
        assert_eq!(
            FaultVocabulary::parse(&nodes(MAX_NODES))
                .expect("the limit itself is accepted")
                .nodes(),
            MAX_NODES
        );
        assert!(FaultVocabulary::parse(&nodes(MAX_NODES + 1)).is_err());
        assert!(FaultVocabulary::new(MAX_NODES + 1, Vec::new()).is_err());
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
    fn declaration_metadata_does_not_change_the_action_alphabet() {
        let bare = FaultVocabulary::parse(
            "setup /bin/prep\nnode service /bin/service\nhook 1 /bin/check\nready /bin/ready\n",
        )
        .expect("bare bundle");
        let with_metadata = FaultVocabulary::parse(
            "setup /bin/prep\nnode service /bin/service\n\
             describe node service the supervised service\n\
             hook 1 /bin/check\n\
             describe hook 1 checks the service\n\
             assert always 2 from 1 the service is healthy\n\
             diagnostic output /bin/cat /run/service.out\n\
             ready /bin/ready\n",
        )
        .expect("bundle metadata is validated and ignored by the alphabet");
        assert_eq!(with_metadata.nodes(), bare.nodes());
        assert_eq!(with_metadata.hooks(), bare.hooks());
    }

    #[test]
    fn malformed_declaration_references_are_rejected_before_alphabet_parse() {
        let error = FaultVocabulary::parse(
            "node service /bin/service\nhook 1 /bin/check\nassert always 2 from 99 missing hook\n",
        )
        .expect_err("an assertion cannot report through an undeclared hook");
        assert!(error.contains("undeclared reporting hook 99"), "{error}");

        let error = FaultVocabulary::parse(
            "node service /bin/service\ndescribe node absent the wrong node\n",
        )
        .expect_err("a description cannot name an undeclared node");
        assert!(error.contains("does not declare"), "{error}");
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
