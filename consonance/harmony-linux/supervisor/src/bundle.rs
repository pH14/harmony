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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiskSpec {
    pub node: u16,
    pub path: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Bundle {
    pub nodes: Vec<NodeSpec>,
    pub hooks: Vec<HookSpec>,
    pub disks: Vec<DiskSpec>,
    pub ready: Option<Vec<String>>,
    pub setup: Option<Vec<String>>,
    pub workload: Option<Vec<String>>,
    pub check: Option<Vec<String>>,
}

impl Bundle {
    #[must_use]
    pub fn hook(&self, id: u32) -> Option<&HookSpec> {
        self.hooks
            .binary_search_by_key(&id, |hook| hook.id)
            .ok()
            .map(|index| &self.hooks[index])
    }

    #[must_use]
    pub fn disk(&self, node: u16) -> Option<&DiskSpec> {
        self.disks.iter().find(|disk| disk.node == node)
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
    #[error("line {line}: a second workload command")]
    DuplicateWorkload { line: usize },
    #[error("line {line}: a second check command")]
    DuplicateCheck { line: usize },
    #[error("line {line}: unterminated quote")]
    UnterminatedQuote { line: usize },
    #[error("line {line}: a command word contains NUL")]
    Nul { line: usize },
    #[error("line {line}: more than {MAX_NODES} nodes")]
    TooManyNodes { line: usize },
    #[error("the bundle declares no node")]
    NoNodes,
    #[error("line {line}: disk names undeclared node {name:?}")]
    DiskNode { line: usize, name: String },
    #[error("line {line}: node {name:?} already has a disk")]
    DuplicateDisk { line: usize, name: String },
    #[error("line {line}: disk path {path:?} is not absolute")]
    DiskPath { line: usize, path: String },
    #[error(
        "line {line}: disk size {size:?} is not a multiple of 4K from 16M through {}M",
        crate::disk::MAX_DISK_BYTES >> 20
    )]
    DiskSize { line: usize, size: String },
    #[error("line {line}: more than {} disks", crate::disk::MAX_DISKS)]
    TooManyDisks { line: usize },
    #[error("line {line}: disk flag {word:?} is not \"checksummed\"")]
    DiskFlag { line: usize, word: String },
}

pub fn parse_bundle(text: &str) -> Result<Bundle, BundleError> {
    let mut bundle = Bundle::default();
    let mut disks = Vec::new();
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
            "disk" => {
                let mut words: Vec<String> = words.collect();
                if words.len() > 3 {
                    let flags = words.split_off(3);
                    if flags != ["checksummed"] {
                        return Err(BundleError::DiskFlag {
                            line,
                            word: flags.join(" "),
                        });
                    }
                }
                let [node, path, size] =
                    <[String; 3]>::try_from(words).map_err(|words| BundleError::Incomplete {
                        line,
                        item: "disk",
                        need: 3_usize.saturating_sub(words.len()),
                    })?;
                disks.push((line, node, path, size));
            }
            "ready" => {
                let argv: Vec<String> = words.collect();
                validate_words(line, &argv)?;
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
                validate_words(line, &argv)?;
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
            "workload" => {
                let argv: Vec<String> = words.collect();
                if argv.is_empty() {
                    return Err(BundleError::Incomplete {
                        line,
                        item: "workload",
                        need: 1,
                    });
                }
                if bundle.workload.is_some() {
                    return Err(BundleError::DuplicateWorkload { line });
                }
                bundle.workload = Some(argv);
            }
            "check" => {
                let argv: Vec<String> = words.collect();
                if argv.is_empty() {
                    return Err(BundleError::Incomplete {
                        line,
                        item: "check",
                        need: 1,
                    });
                }
                if bundle.check.is_some() {
                    return Err(BundleError::DuplicateCheck { line });
                }
                bundle.check = Some(argv);
            }
            _ => return Err(BundleError::UnknownItem { line, word: item }),
        }
    }
    if bundle.nodes.is_empty() {
        return Err(BundleError::NoNodes);
    }
    for (line, name, path, size) in disks {
        parse_disk(&mut bundle, line, name, path, size)?;
    }
    bundle.hooks.sort_by_key(|hook| hook.id);
    Ok(bundle)
}

fn parse_disk(
    bundle: &mut Bundle,
    line: usize,
    name: String,
    path: String,
    size: String,
) -> Result<(), BundleError> {
    let Some(node) = bundle
        .nodes
        .iter()
        .position(|node| node.name == name)
        .and_then(|index| u16::try_from(index).ok())
    else {
        return Err(BundleError::DiskNode { line, name });
    };
    if bundle.disk(node).is_some() {
        return Err(BundleError::DuplicateDisk { line, name });
    }
    if !path.starts_with('/') || path.contains('\0') {
        return Err(BundleError::DiskPath { line, path });
    }
    let Some(bytes) = crate::disk::parse_size(&size) else {
        return Err(BundleError::DiskSize { line, size });
    };
    if bundle.disks.len() == crate::disk::MAX_DISKS {
        return Err(BundleError::TooManyDisks { line });
    }
    bundle.disks.push(DiskSpec { node, path, bytes });
    Ok(())
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
    if name.contains('\0') || argv.iter().any(|word| word.contains('\0')) {
        return Err(BundleError::Nul { line });
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
    validate_words(line, &argv)?;
    if bundle.hooks.iter().any(|hook| hook.id == id) {
        return Err(BundleError::DuplicateHookId { line, id });
    }
    bundle.hooks.push(HookSpec { id, argv });
    Ok(())
}

fn validate_words(line: usize, words: &[String]) -> Result<(), BundleError> {
    if words.iter().any(|word| word.contains('\0')) {
        Err(BundleError::Nul { line })
    } else {
        Ok(())
    }
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
# the sample service
node service /usr/bin/service --data-dir /run/service

hook 2 /bin/verify.sh
hook 1 /bin/write.sh 200
ready /usr/bin/servicectl endpoint health
";
        let bundle = parse_bundle(text).unwrap();
        assert_eq!(bundle.nodes.len(), 1);
        assert_eq!(bundle.nodes[0].name, "service");
        assert_eq!(
            bundle.nodes[0].argv,
            ["/usr/bin/service", "--data-dir", "/run/service"]
        );
        assert_eq!(
            bundle.hooks.iter().map(|hook| hook.id).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(bundle.hook(1).unwrap().argv, ["/bin/write.sh", "200"]);
        assert!(bundle.hook(3).is_none());
        assert_eq!(
            bundle.ready.unwrap(),
            ["/usr/bin/servicectl", "endpoint", "health"]
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
    fn workload_and_check_commands_are_kept_whole_and_declared_once() {
        let bundle = parse_bundle(
            r#"node a /a
workload /bin/sh -c "run forever"
check /bin/check --json
"#,
        )
        .unwrap();
        assert_eq!(bundle.workload.unwrap(), ["/bin/sh", "-c", "run forever"]);
        assert_eq!(bundle.check.unwrap(), ["/bin/check", "--json"]);
        assert_eq!(
            parse_bundle("node a /a\nworkload /a\nworkload /b\n"),
            Err(BundleError::DuplicateWorkload { line: 3 })
        );
        assert_eq!(
            parse_bundle("node a /a\ncheck /a\ncheck /b\n"),
            Err(BundleError::DuplicateCheck { line: 3 })
        );
        assert_eq!(
            parse_bundle("node a /a\nworkload\n"),
            Err(BundleError::Incomplete {
                line: 2,
                item: "workload",
                need: 1,
            })
        );
        assert_eq!(
            parse_bundle("node a /a\ncheck\n"),
            Err(BundleError::Incomplete {
                line: 2,
                item: "check",
                need: 1,
            })
        );
    }

    #[test]
    fn a_disk_line_gives_a_declared_node_its_own_filesystem() {
        let bundle =
            parse_bundle("node a /a\ndisk b /data/b 64M\nnode b /b\ndisk a /data/a 1G\n").unwrap();
        assert_eq!(
            bundle.disks,
            [
                DiskSpec {
                    node: 1,
                    path: "/data/b".to_owned(),
                    bytes: 64 << 20,
                },
                DiskSpec {
                    node: 0,
                    path: "/data/a".to_owned(),
                    bytes: 1 << 30,
                },
            ]
        );
        assert_eq!(bundle.disk(0).map(|disk| disk.bytes), Some(1 << 30));
        assert_eq!(bundle.disk(2), None);
        let error = |text: &str| parse_bundle(text).unwrap_err();
        assert_eq!(
            error("node a /a\ndisk z /d 64M\n"),
            BundleError::DiskNode {
                line: 2,
                name: "z".to_owned(),
            }
        );
        assert_eq!(
            error("node a /a\ndisk a /d 64M\ndisk a /e 64M\n"),
            BundleError::DuplicateDisk {
                line: 3,
                name: "a".to_owned(),
            }
        );
        assert_eq!(
            error("node a /a\ndisk a d 64M\n"),
            BundleError::DiskPath {
                line: 2,
                path: "d".to_owned(),
            }
        );
        for size in ["8M", "2G", "64", "64X", "", "65537K"] {
            assert!(
                matches!(
                    parse_bundle(&format!("node a /a\ndisk a /d \"{size}\"\n")),
                    Err(BundleError::DiskSize { line: 2, .. })
                ),
                "{size}"
            );
        }
        assert_eq!(
            error("node a /a\ndisk a /d\n"),
            BundleError::Incomplete {
                line: 2,
                item: "disk",
                need: 1,
            }
        );
        assert_eq!(
            parse_bundle("node a /a\ndisk a /d 16M checksummed\n")
                .unwrap()
                .disk(0)
                .map(|disk| disk.bytes),
            Some(16 << 20)
        );
        assert_eq!(
            error("node a /a\ndisk a /d 16M fast\n"),
            BundleError::DiskFlag {
                line: 2,
                word: "fast".to_owned(),
            }
        );
        assert_eq!(
            error("node a /a\ndisk a /d 16M checksummed extra\n"),
            BundleError::DiskFlag {
                line: 2,
                word: "checksummed extra".to_owned(),
            }
        );
        let mut text = String::new();
        for index in 0..=crate::disk::MAX_DISKS {
            text.push_str(&format!("node n{index} /x\ndisk n{index} /d{index} 16M\n"));
        }
        assert_eq!(
            error(&text),
            BundleError::TooManyDisks {
                line: 2 * crate::disk::MAX_DISKS + 2
            }
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
        let bundle =
            parse_bundle(r#"node service /bin/sh -c "service -D \"/db\" -c x=1""#).unwrap();
        assert_eq!(
            bundle.nodes[0].argv,
            ["/bin/sh", "-c", r#"service -D "/db" -c x=1"#]
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
        let cases: [(&str, BundleError); 10] = [
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
            ("node a /a\0\n", BundleError::Nul { line: 1 }),
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
    fn parsing_never_panics_on_arbitrary_text() {
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
