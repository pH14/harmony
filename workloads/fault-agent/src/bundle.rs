// SPDX-License-Identifier: AGPL-3.0-or-later

pub const MAX_NODES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeSpec {
    pub name: String,
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookSpec {
    pub id: u32,
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Bundle {
    pub nodes: Vec<NodeSpec>,
    pub hooks: Vec<HookSpec>,
    pub ready: Option<Vec<String>>,
    pub setup: Option<Vec<String>>,
}

impl Bundle {
    #[must_use]
    pub fn hook(&self, id: u32) -> Option<&HookSpec> {
        self.hooks
            .binary_search_by_key(&id, |hook| hook.id)
            .ok()
            .map(|index| &self.hooks[index])
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BundleError {
    #[error("line {line}: unknown item {word:?}")]
    UnknownItem { line: usize, word: String },
    #[error("line {line}: {item} needs {need} more word(s)")]
    Incomplete {
        line: usize,
        item: &'static str,
        need: usize,
    },
    #[error("line {line}: hook id {word:?} is not a u32")]
    BadHookId { line: usize, word: String },
    #[error("line {line}: hook id {id} is already declared")]
    DuplicateHookId { line: usize, id: u32 },
    #[error("line {line}: node name {name:?} is already declared")]
    DuplicateNodeName { line: usize, name: String },
    #[error("line {line}: a second ready probe")]
    DuplicateReady { line: usize },
    #[error("line {line}: a second setup command")]
    DuplicateSetup { line: usize },
    #[error("line {line}: unterminated quote")]
    UnterminatedQuote { line: usize },
    #[error("line {line}: more than {MAX_NODES} nodes")]
    TooManyNodes { line: usize },
    #[error("the bundle declares no node")]
    NoNodes,
}

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
            "setup" => {
                let argv: Vec<String> = words.collect();
                if argv.is_empty() {
                    return Err(BundleError::Incomplete {
                        line,
                        item: "setup",
                        need: 1,
                    });
                }
                if bundle.setup.is_some() {
                    return Err(BundleError::DuplicateSetup { line });
                }
                bundle.setup = Some(argv);
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

    #[test]
    fn a_setup_line_is_kept_whole_and_declared_once() {
        let bundle = parse_bundle(
            "setup /bin/sh -c \"mount -t tmpfs shm /dev/shm; ip link set lo up\"\nnode a /a\n",
        )
        .unwrap();
        assert_eq!(
            bundle.setup.unwrap(),
            [
                "/bin/sh",
                "-c",
                "mount -t tmpfs shm /dev/shm; ip link set lo up"
            ]
        );
        assert_eq!(
            parse_bundle("setup /a\nsetup /b\nnode a /a\n"),
            Err(BundleError::DuplicateSetup { line: 2 })
        );
        assert_eq!(
            parse_bundle("setup\nnode a /a\n"),
            Err(BundleError::Incomplete {
                line: 1,
                item: "setup",
                need: 1,
            })
        );
    }

    #[test]
    fn a_bundle_without_a_setup_line_declares_none() {
        assert_eq!(parse_bundle("node a /a\n").unwrap().setup, None);
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
