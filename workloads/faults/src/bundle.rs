// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

pub const VOCABULARY_FORMAT: &str = "faultlab_bundle_v9";
pub const MAX_NODES: u16 = 64;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultVocabulary {
    nodes: u16,
    hooks: Vec<u32>,
    #[serde(default)]
    disks: Vec<u16>,
    instrumented_events: bool,
    clock_faults: bool,
}

impl FaultVocabulary {
    pub fn new(nodes: u16, hooks: Vec<u32>) -> Result<Self, String> {
        if nodes == 0 {
            return Err("a fault bundle must declare at least one node".to_owned());
        }
        if nodes > MAX_NODES {
            return Err(format!(
                "a fault bundle declares {nodes} nodes; the platform supervisor supervises at most {MAX_NODES}"
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
            disks: Vec::new(),
            instrumented_events: false,
            clock_faults: false,
        })
    }

    pub fn with_disks(mut self, disks: Vec<u16>) -> Result<Self, String> {
        let mut sorted = disks.clone();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.len() != disks.len() {
            return Err("a fault bundle declares two disks for one node".to_owned());
        }
        if sorted.last().is_some_and(|&node| node >= self.nodes) {
            return Err("a fault bundle declares a disk for an undeclared node".to_owned());
        }
        self.disks = sorted;
        Ok(self)
    }

    #[must_use]
    pub(crate) fn with_instrumented_events(mut self, enabled: bool) -> Self {
        self.instrumented_events = enabled;
        self
    }

    #[must_use]
    pub fn with_clock_faults(mut self, enabled: bool) -> Self {
        self.clock_faults = enabled;
        self
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut nodes = 0_u16;
        let mut names = Vec::new();
        let mut disk_names = Vec::new();
        let mut hooks = Vec::new();
        let mut lifecycle = std::collections::BTreeSet::new();
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
                    let Some(name) = fields.next() else {
                        return Err(format!("bundle line {line_number}: node has no name"));
                    };
                    names.push(name.to_owned());
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
                "disk" => {
                    let (Some(node), Some(_path), Some(_size), None) =
                        (fields.next(), fields.next(), fields.next(), fields.next())
                    else {
                        return Err(format!(
                            "bundle line {line_number}: disk needs a node, a path and a size"
                        ));
                    };
                    disk_names.push((line_number, node.to_owned()));
                }
                "ready" | "setup" | "workload" | "check" => {
                    if fields.next().is_none() {
                        return Err(format!(
                            "bundle line {line_number}: {keyword} has no command"
                        ));
                    }
                    if !lifecycle.insert(keyword) {
                        return Err(format!("bundle line {line_number}: duplicate {keyword}"));
                    }
                }
                other => {
                    return Err(format!(
                        "bundle line {line_number}: unknown keyword {other:?}"
                    ));
                }
            }
        }
        let disks = disk_names
            .into_iter()
            .map(|(line_number, name)| {
                names
                    .iter()
                    .position(|node| *node == name)
                    .and_then(|index| u16::try_from(index).ok())
                    .ok_or_else(|| {
                        format!("bundle line {line_number}: disk names undeclared node {name:?}")
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(nodes, hooks)?.with_disks(disks)
    }

    #[must_use]
    pub fn nodes(&self) -> u16 {
        self.nodes
    }

    #[must_use]
    pub fn hooks(&self) -> &[u32] {
        &self.hooks
    }

    #[must_use]
    pub fn disks(&self) -> &[u16] {
        &self.disks
    }

    #[must_use]
    pub fn instrumented_events(&self) -> bool {
        self.instrumented_events
    }

    #[must_use]
    pub fn clock_faults(&self) -> bool {
        self.clock_faults
    }

    #[must_use]
    pub fn identifier(&self) -> String {
        let hooks = self
            .hooks
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let disks = self
            .disks
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let events = if self.instrumented_events {
            "antithesis"
        } else {
            "none"
        };
        let clock = if self.clock_faults {
            "time_namespace"
        } else {
            "none"
        };
        format!(
            "{VOCABULARY_FORMAT};nodes={};hooks={hooks};disks={disks};events={events};clock={clock}",
            self.nodes,
        )
    }

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
        let disks = fields
            .next()
            .and_then(|field| field.strip_prefix("disks="))
            .ok_or("fault vocabulary has no disk list")?;
        let instrumented_events = match fields
            .next()
            .and_then(|field| field.strip_prefix("events="))
            .ok_or("fault vocabulary has no instrumented-event capability")?
        {
            "none" => false,
            "antithesis" => true,
            _ => return Err("fault vocabulary has an unknown event capability".to_owned()),
        };
        let clock_faults = match fields
            .next()
            .and_then(|field| field.strip_prefix("clock="))
            .ok_or("fault vocabulary has no clock capability")?
        {
            "none" => false,
            "time_namespace" => true,
            _ => return Err("fault vocabulary has an unknown clock capability".to_owned()),
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
        let disks = if disks.is_empty() {
            Vec::new()
        } else {
            disks
                .split(',')
                .map(|id| {
                    id.parse::<u16>()
                        .map_err(|error| format!("fault vocabulary disk node: {error}"))
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(Self::new(nodes, hooks)?
            .with_disks(disks)?
            .with_instrumented_events(instrumented_events)
            .with_clock_faults(clock_faults))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ETCD: &str = "\
setup /bin/sh -c \"mkdir -p /tmp/etcd\"
node etcd /usr/bin/etcd --data-dir /tmp/etcd
hook 1 /hooks/put-batch
hook 2 /hooks/read-back
ready /usr/bin/etcdctl endpoint health
";

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
    fn workload_and_check_commands_are_lifecycle_not_search_actions() {
        let vocabulary =
            FaultVocabulary::parse("node a /a\nworkload /load\ncheck /check\n").unwrap();
        assert_eq!(vocabulary.nodes(), 1);
        assert!(vocabulary.hooks().is_empty());
        for extra in [
            "check",
            "workload",
            "check /one\ncheck /two",
            "workload /one\nworkload /two",
        ] {
            assert!(FaultVocabulary::parse(&format!("node a /a\n{extra}\n")).is_err());
        }
    }

    #[test]
    fn a_disk_line_gives_its_node_disk_faults() {
        let vocabulary = FaultVocabulary::parse(
            "node a /a\nnode b /b\ndisk b /data/b 64M\nnode c /c\ndisk a /data/a 16M\n",
        )
        .expect("parse");
        assert_eq!(vocabulary.nodes(), 3);
        assert_eq!(vocabulary.disks(), [0, 1]);
        let identifier = vocabulary.identifier();
        assert_eq!(
            identifier,
            "faultlab_bundle_v9;nodes=3;hooks=;disks=0,1;events=none;clock=none"
        );
        assert_eq!(
            FaultVocabulary::from_identifier(&identifier).expect("resolve"),
            vocabulary
        );
        for bad in [
            "node a /a\ndisk z /data 16M\n",
            "node a /a\ndisk a /data\n",
            "node a /a\ndisk a /data 16M extra\n",
            "node a /a\ndisk a /one 16M\ndisk a /two 16M\n",
        ] {
            assert!(FaultVocabulary::parse(bad).is_err(), "{bad}");
        }
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
    fn a_bundle_past_the_supervisor_node_limit_is_refused() {
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
        assert_eq!(
            etcd.identifier(),
            "faultlab_bundle_v9;nodes=1;hooks=1,2;disks=;events=none;clock=none"
        );
        assert_ne!(
            etcd.identifier(),
            etcd.clone().with_instrumented_events(true).identifier()
        );
        let clocked = etcd.clone().with_clock_faults(true);
        assert_eq!(
            clocked.identifier(),
            "faultlab_bundle_v9;nodes=1;hooks=1,2;disks=;events=none;clock=time_namespace"
        );
        assert_eq!(
            FaultVocabulary::from_identifier(&clocked.identifier()).expect("resolve"),
            clocked
        );
        let both = clocked.with_disks(vec![0]).expect("disks");
        assert_eq!(
            both.identifier(),
            "faultlab_bundle_v9;nodes=1;hooks=1,2;disks=0;events=none;clock=time_namespace"
        );
        assert_eq!(
            FaultVocabulary::from_identifier(&both.identifier()).expect("resolve"),
            both
        );
    }

    #[test]
    fn an_unrecognized_identifier_is_refused() {
        for identifier in [
            "faultlab_bundle_v0;nodes=1;hooks=1",
            "faultlab_bundle_v1;nodes=1;hooks=1",
            "faultlab_bundle_v2;nodes=1",
            "faultlab_bundle_v5;nodes=1;hooks=1;events=none;interrupts=enabled",
            "faultlab_bundle_v7;nodes=1;hooks=1;events=none",
            "faultlab_bundle_v8;nodes=1;hooks=1;disks=;events=none",
            "faultlab_bundle_v9;nodes=1;hooks=1;disks=;events=none",
            "faultlab_bundle_v9;nodes=0;hooks=1;disks=;events=none;clock=none",
            "faultlab_bundle_v9;nodes=x;hooks=1;disks=;events=none;clock=none",
            "faultlab_bundle_v9;nodes=1;hooks=1;disks=;events=other;clock=none",
            "faultlab_bundle_v9;nodes=1;hooks=1;disks=;events=none;clock=other",
            "faultlab_bundle_v9;nodes=1;hooks=1;disks=;events=none;clock=none;extra=2",
            "faultlab_bundle_v9;nodes=1;hooks=one;disks=;events=none;clock=none",
            "faultlab_bundle_v9;nodes=1;hooks=1;disks=1;events=none;clock=none",
            "faultlab_bundle_v9;nodes=2;hooks=1;disks=0,0;events=none;clock=none",
        ] {
            assert!(
                FaultVocabulary::from_identifier(identifier).is_err(),
                "{identifier} must be refused"
            );
        }
        let no_hooks = FaultVocabulary::from_identifier(
            "faultlab_bundle_v9;nodes=2;hooks=;disks=;events=antithesis;clock=none",
        )
        .expect("resolve");
        assert_eq!(no_hooks.nodes(), 2);
        assert!(no_hooks.hooks().is_empty());
        assert!(no_hooks.instrumented_events());
    }
}
