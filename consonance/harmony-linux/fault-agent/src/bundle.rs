// SPDX-License-Identifier: AGPL-3.0-or-later
//! The workload description the agent reads at start, one item per line.
//!
//! ```text
//! node <name> <argv...>    a supervised long-lived process
//! hook <id> <argv...>      a one-shot command a RunHook fault launches
//! ready <argv...>          a command that exits 0 once setup is done
//! ```
//!
//! A node's id is its line order among `node` lines, from 0 — the same id the
//! host names in a `DecisionClass::Process` standing-fault target, so the two
//! sides agree without a handshake. Blank lines and `#` comments are ignored.
//! Arguments split on whitespace; a double-quoted argument keeps its spaces and
//! honours `\"` and `\\`, which is what a `sh -c "..."` node needs.

/// The largest number of nodes a bundle may describe: the alive bitmap the
/// agent publishes in a state register is one `u64`, one bit per node.
pub const MAX_NODES: usize = 64;

/// A supervised long-lived process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeSpec {
    /// The operator-facing name; the agent's own identity for a node is its
    /// index in [`Bundle::nodes`].
    pub name: String,
    /// The command and its arguments.
    pub argv: Vec<String>,
}

/// A one-shot command launched by a `Fault::RunHook(id)` window opening.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookSpec {
    /// The hook id the host names in the fault.
    pub id: u32,
    /// The command and its arguments.
    pub argv: Vec<String>,
}

/// A parsed bundle.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Bundle {
    /// The supervised nodes, in bundle order; the index is the node id.
    pub nodes: Vec<NodeSpec>,
    /// The hooks, sorted by id.
    pub hooks: Vec<HookSpec>,
    /// The readiness probe, if the bundle declares one.
    pub ready: Option<Vec<String>>,
}

impl Bundle {
    /// The hook with `id`, if the bundle declares it.
    #[must_use]
    pub fn hook(&self, id: u32) -> Option<&HookSpec> {
        self.hooks
            .binary_search_by_key(&id, |hook| hook.id)
            .ok()
            .map(|index| &self.hooks[index])
    }
}

/// Why a bundle was rejected. Every variant names the 1-based line so an image
/// build reports a usable location.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BundleError {
    /// A line started with something other than `node`, `hook`, or `ready`.
    #[error("line {line}: unknown item {word:?}")]
    UnknownItem {
        /// The 1-based line number.
        line: usize,
        /// The keyword that was not recognised.
        word: String,
    },
    /// A line has too few words to be the item it claims to be.
    #[error("line {line}: {item} needs {need} more word(s)")]
    Incomplete {
        /// The 1-based line number.
        line: usize,
        /// The item keyword.
        item: &'static str,
        /// How many words are missing.
        need: usize,
    },
    /// A hook id is not a `u32`.
    #[error("line {line}: hook id {word:?} is not a u32")]
    BadHookId {
        /// The 1-based line number.
        line: usize,
        /// The offending word.
        word: String,
    },
    /// Two hooks share an id, so a `RunHook` fault would be ambiguous.
    #[error("line {line}: hook id {id} is already declared")]
    DuplicateHookId {
        /// The 1-based line number.
        line: usize,
        /// The repeated id.
        id: u32,
    },
    /// Two nodes share a name.
    #[error("line {line}: node name {name:?} is already declared")]
    DuplicateNodeName {
        /// The 1-based line number.
        line: usize,
        /// The repeated name.
        name: String,
    },
    /// More than one readiness probe.
    #[error("line {line}: a second ready probe")]
    DuplicateReady {
        /// The 1-based line number.
        line: usize,
    },
    /// A quoted argument has no closing quote.
    #[error("line {line}: unterminated quote")]
    UnterminatedQuote {
        /// The 1-based line number.
        line: usize,
    },
    /// The bundle describes more nodes than the alive bitmap can address.
    #[error("line {line}: more than {MAX_NODES} nodes")]
    TooManyNodes {
        /// The 1-based line number.
        line: usize,
    },
    /// The bundle describes no node at all, so there is nothing to supervise.
    #[error("the bundle declares no node")]
    NoNodes,
}

/// Parse a bundle file's contents.
///
/// # Errors
///
/// Returns the first [`BundleError`] the text triggers; a bundle is a build
/// artifact, so a malformed one fails the agent at start rather than being
/// partially honoured.
pub fn parse_bundle(text: &str) -> Result<Bundle, BundleError> {
    let mut bundle = Bundle::default();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut words = tokenize(trimmed)
            .ok_or(BundleError::UnterminatedQuote { line })?
            .into_iter();
        // `trimmed` is non-empty, so the tokenizer yields at least one word.
        let Some(item) = words.next() else {
            continue;
        };
        match item.as_str() {
            "node" => parse_node(&mut bundle, line, words)?,
            "hook" => parse_hook(&mut bundle, line, words)?,
            "ready" => {
                let argv: Vec<String> = words.collect();
                if argv.is_empty() {
                    return Err(BundleError::Incomplete {
                        line,
                        item: "ready",
                        need: 1,
                    });
                }
                if bundle.ready.is_some() {
                    return Err(BundleError::DuplicateReady { line });
                }
                bundle.ready = Some(argv);
            }
            _ => return Err(BundleError::UnknownItem { line, word: item }),
        }
    }
    if bundle.nodes.is_empty() {
        return Err(BundleError::NoNodes);
    }
    bundle.hooks.sort_by_key(|hook| hook.id);
    Ok(bundle)
}

fn parse_node(
    bundle: &mut Bundle,
    line: usize,
    mut words: impl Iterator<Item = String>,
) -> Result<(), BundleError> {
    let Some(name) = words.next() else {
        return Err(BundleError::Incomplete {
            line,
            item: "node",
            need: 2,
        });
    };
    let argv: Vec<String> = words.collect();
    if argv.is_empty() {
        return Err(BundleError::Incomplete {
            line,
            item: "node",
            need: 1,
        });
    }
    if bundle.nodes.iter().any(|node| node.name == name) {
        return Err(BundleError::DuplicateNodeName { line, name });
    }
    if bundle.nodes.len() == MAX_NODES {
        return Err(BundleError::TooManyNodes { line });
    }
    bundle.nodes.push(NodeSpec { name, argv });
    Ok(())
}

fn parse_hook(
    bundle: &mut Bundle,
    line: usize,
    mut words: impl Iterator<Item = String>,
) -> Result<(), BundleError> {
    let Some(word) = words.next() else {
        return Err(BundleError::Incomplete {
            line,
            item: "hook",
            need: 2,
        });
    };
    let id = word
        .parse::<u32>()
        .map_err(|_| BundleError::BadHookId { line, word })?;
    let argv: Vec<String> = words.collect();
    if argv.is_empty() {
        return Err(BundleError::Incomplete {
            line,
            item: "hook",
            need: 1,
        });
    }
    if bundle.hooks.iter().any(|hook| hook.id == id) {
        return Err(BundleError::DuplicateHookId { line, id });
    }
    bundle.hooks.push(HookSpec { id, argv });
    Ok(())
}

/// Split a line into words. `None` when a double-quoted word is unterminated.
fn tokenize(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut open = false;
    let mut quoted = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                open = true;
            }
            '\\' if quoted => match chars.next() {
                Some(escaped @ ('"' | '\\')) => {
                    current.push(escaped);
                    open = true;
                }
                Some(other) => {
                    current.push('\\');
                    current.push(other);
                    open = true;
                }
                None => return None,
            },
            c if c.is_whitespace() && !quoted => {
                if open {
                    words.push(core::mem::take(&mut current));
                    open = false;
                }
            }
            c => {
                current.push(c);
                open = true;
            }
        }
    }
    if quoted {
        return None;
    }
    if open {
        words.push(current);
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_bundle_parses_into_ordered_ids() {
        let text = "\
# the etcd workload
node etcd /usr/bin/etcd --data-dir /run/etcd

hook 2 /bin/verify.sh
hook 1 /bin/put.sh 200
ready /usr/bin/etcdctl endpoint health
";
        let bundle = parse_bundle(text).unwrap();
        assert_eq!(bundle.nodes.len(), 1);
        assert_eq!(bundle.nodes[0].name, "etcd");
        assert_eq!(
            bundle.nodes[0].argv,
            ["/usr/bin/etcd", "--data-dir", "/run/etcd"]
        );
        // Hooks are keyed by declared id, and sorted so lookup is a binary search.
        assert_eq!(
            bundle.hooks.iter().map(|hook| hook.id).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(bundle.hook(1).unwrap().argv, ["/bin/put.sh", "200"]);
        assert!(bundle.hook(3).is_none());
        assert_eq!(
            bundle.ready.unwrap(),
            ["/usr/bin/etcdctl", "endpoint", "health"]
        );
    }

    /// The bundles the image ships, byte-for-byte. The searcher parses the same
    /// files independently (`dissonance/searcher/src/faultlab/bundle.rs`); the
    /// ids asserted here and there must agree, or the searcher explores an
    /// alphabet the agent never applies.
    const SHIPPED_ETCD: &str = include_str!("../../linux/faultlab-etcd.bundle");
    const SHIPPED_PGCIC: &str = include_str!("../../linux/faultlab-pgcic.bundle");
    const SHIPPED_SQLITE: &str = include_str!("../../linux/faultlab-sqlite.bundle");

    #[test]
    fn the_shipped_sqlite_bundle_has_nodes_0_1_and_hooks_1_2() {
        let bundle = parse_bundle(SHIPPED_SQLITE).unwrap();
        assert_eq!(bundle.nodes.len(), 2);
        assert_eq!(bundle.nodes[0].name, "checkpointer");
        assert_eq!(bundle.nodes[1].name, "writer");
        assert_eq!(
            bundle.hooks.iter().map(|hook| hook.id).collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(bundle.ready.is_some());
    }

    #[test]
    fn the_shipped_etcd_bundle_has_node_0_and_hooks_1_2() {
        let bundle = parse_bundle(SHIPPED_ETCD).unwrap();
        assert_eq!(bundle.nodes.len(), 1);
        assert_eq!(bundle.nodes[0].name, "etcd");
        assert_eq!(
            bundle.hooks.iter().map(|hook| hook.id).collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(bundle.ready.is_some());
    }

    #[test]
    fn the_shipped_postgres_bundle_has_node_0_and_hooks_1_to_4() {
        let bundle = parse_bundle(SHIPPED_PGCIC).unwrap();
        assert_eq!(bundle.nodes.len(), 1);
        assert_eq!(bundle.nodes[0].name, "postgres");
        assert_eq!(
            bundle.hooks.iter().map(|hook| hook.id).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert!(bundle.ready.is_some());
    }

    #[test]
    fn node_ids_follow_line_order() {
        let bundle = parse_bundle("node a /a\nhook 1 /h\nnode b /b\nnode c /c\n").unwrap();
        let names: Vec<&str> = bundle.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn quoted_arguments_keep_their_spaces_and_escapes() {
        let bundle = parse_bundle(r#"node pg /bin/sh -c "postgres -D \"/db\" -c x=1""#).unwrap();
        assert_eq!(
            bundle.nodes[0].argv,
            ["/bin/sh", "-c", r#"postgres -D "/db" -c x=1"#]
        );
    }

    #[test]
    fn an_empty_quoted_argument_survives() {
        let bundle = parse_bundle("node a /bin/sh -c \"\"\n").unwrap();
        assert_eq!(bundle.nodes[0].argv, ["/bin/sh", "-c", ""]);
    }

    #[test]
    fn a_backslash_outside_quotes_is_literal() {
        let bundle = parse_bundle(r"node a /bin/x C:\tmp").unwrap();
        assert_eq!(bundle.nodes[0].argv, ["/bin/x", r"C:\tmp"]);
    }

    #[test]
    fn malformed_bundles_are_rejected_with_the_line() {
        let cases: [(&str, BundleError); 9] = [
            (
                "node a /a\nrun b\n",
                BundleError::UnknownItem {
                    line: 2,
                    word: "run".to_string(),
                },
            ),
            (
                "node a\n",
                BundleError::Incomplete {
                    line: 1,
                    item: "node",
                    need: 1,
                },
            ),
            (
                "node\n",
                BundleError::Incomplete {
                    line: 1,
                    item: "node",
                    need: 2,
                },
            ),
            (
                "node a /a\nhook x /h\n",
                BundleError::BadHookId {
                    line: 2,
                    word: "x".to_string(),
                },
            ),
            (
                "node a /a\nhook 1 /h\nhook 1 /g\n",
                BundleError::DuplicateHookId { line: 3, id: 1 },
            ),
            (
                "node a /a\nnode a /b\n",
                BundleError::DuplicateNodeName {
                    line: 2,
                    name: "a".to_string(),
                },
            ),
            (
                "node a /a\nready /r\nready /s\n",
                BundleError::DuplicateReady { line: 3 },
            ),
            ("node a \"/a\n", BundleError::UnterminatedQuote { line: 1 }),
            ("# nothing\n", BundleError::NoNodes),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_bundle(text), Err(expected), "text: {text:?}");
        }
    }

    #[test]
    fn the_node_count_is_capped_by_the_alive_bitmap_width() {
        let mut text = String::new();
        for i in 0..MAX_NODES {
            text.push_str(&format!("node n{i} /bin/x\n"));
        }
        assert_eq!(parse_bundle(&text).unwrap().nodes.len(), MAX_NODES);
        text.push_str("node overflow /bin/x\n");
        assert_eq!(
            parse_bundle(&text),
            Err(BundleError::TooManyNodes {
                line: MAX_NODES + 1
            })
        );
    }

    #[test]
    fn parsing_never_panics_on_arbitrary_bytes() {
        for text in [
            "",
            "\n\n\n",
            "\"",
            "node",
            "node \"a\\",
            "hook 4294967296 /x",
            "\u{feff}node a /a",
            "node \u{1f600} /a",
        ] {
            let _ = parse_bundle(text);
        }
    }
}
