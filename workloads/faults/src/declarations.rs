// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a workload image declares about itself, beyond the action alphabet.
//!
//! The action alphabet ([`crate::bundle`]) is what search needs: how many nodes
//! and which hook ids exist. Investigation needs more. An agent reading a
//! finding has to learn what assertion 2 claims, which hook evaluates it, and
//! which guest command reads the evidence it left behind. Harmony never invents
//! those meanings; the workload author writes them in the same
//! `/etc/harmony/bundle` file the image already carries.
//!
//! | line | meaning |
//! |---|---|
//! | `describe node <name> <text...>` | what a node is |
//! | `describe hook <id> <text...>` | what a hook does |
//! | `assert always\|sometimes\|reachable <id> [from <hook>] <text...>` | what a property claims |
//! | `diagnostic <name> <argv...>` | a guest command that reads retained evidence |
//!
//! A bundle that declares none of these still parses; inspection then reports
//! the meaning as undeclared rather than guessing one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Format tag recorded with a declaration set.
pub const DECLARATIONS_FORMAT: &str = "harmony-workload-declarations-v1";

/// What kind of claim a property makes. The three kinds are different claims
/// and are never interchangeable: a violated `always` is a failure, a never-hit
/// `sometimes` is an unexercised precondition, and a `reachable` id states only
/// that control arrived somewhere.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AssertionKind {
    /// Must hold every time it is evaluated. A violation is a finding.
    Always,
    /// Must hold at least once across a campaign. Silence is an unexercised
    /// precondition, not a failure.
    Sometimes,
    /// Records that execution reached a point. It makes no correctness claim.
    Reachable,
}

impl AssertionKind {
    /// The keyword this kind is written with.
    #[must_use]
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Sometimes => "sometimes",
            Self::Reachable => "reachable",
        }
    }

    /// What silence from this property is allowed to mean.
    #[must_use]
    pub fn silence_means(self) -> &'static str {
        match self {
            Self::Always => "no evaluation was reported; silence is not a pass",
            Self::Sometimes => "the precondition was never exercised",
            Self::Reachable => "the point was never reached",
        }
    }
}

/// One node the image supervises.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NodeDeclaration {
    /// Node id, its position among the bundle's `node` lines from zero.
    pub id: u16,
    /// The name the bundle gives it.
    pub name: String,
    /// The command the agent supervises.
    pub argv: Vec<String>,
    /// The author's description, when declared.
    pub description: Option<String>,
}

/// One hook the search may launch.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct HookDeclaration {
    /// The hook id an action names.
    pub id: u32,
    /// The guest command the agent runs.
    pub argv: Vec<String>,
    /// The author's description, when declared.
    pub description: Option<String>,
}

/// One property the workload reports through the guest SDK.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AssertionDeclaration {
    /// The SDK assertion id. Hook ids name commands; assertion ids name
    /// properties, and the two numbering spaces are unrelated.
    pub id: u32,
    /// What kind of claim it makes.
    pub kind: AssertionKind,
    /// The hook whose run evaluates it, when one does.
    pub reported_by_hook: Option<u32>,
    /// What the property claims, in the author's words.
    pub meaning: String,
}

/// One guest command that reads retained evidence.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiagnosticDeclaration {
    /// The name an operator asks for.
    pub name: String,
    /// The guest command to run.
    pub argv: Vec<String>,
}

/// Everything one bundle declares about itself.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Declarations {
    /// Always [`DECLARATIONS_FORMAT`].
    pub format: String,
    /// The supervised nodes, by id.
    pub nodes: Vec<NodeDeclaration>,
    /// The declared hooks, ascending by id.
    pub hooks: Vec<HookDeclaration>,
    /// The declared properties, ascending by id.
    pub assertions: Vec<AssertionDeclaration>,
    /// The declared evidence commands.
    pub diagnostics: Vec<DiagnosticDeclaration>,
    /// The setup command, when the bundle names one.
    pub setup: Vec<String>,
    /// The readiness command, when the bundle names one.
    pub ready: Vec<String>,
}

impl Declarations {
    /// Read every declaration from a bundle file's text.
    ///
    /// # Errors
    ///
    /// Returns an error naming the line that is malformed or repeats an id.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut declarations = Self {
            format: DECLARATIONS_FORMAT.to_owned(),
            ..Self::default()
        };
        let mut node_descriptions: BTreeMap<String, String> = BTreeMap::new();
        let mut hook_descriptions: BTreeMap<u32, String> = BTreeMap::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let at = number.saturating_add(1);
            let mut fields = line.split_whitespace();
            let keyword = fields.next().unwrap_or_default();
            match keyword {
                "node" => {
                    let name = word(&mut fields, at, "node has no name")?;
                    let id = u16::try_from(declarations.nodes.len())
                        .map_err(|_| format!("bundle line {at}: too many nodes"))?;
                    declarations.nodes.push(NodeDeclaration {
                        id,
                        name,
                        argv: rest(&mut fields),
                        description: None,
                    });
                }
                "hook" => {
                    let id = number_field(&mut fields, at, "hook")?;
                    declarations.hooks.push(HookDeclaration {
                        id,
                        argv: rest(&mut fields),
                        description: None,
                    });
                }
                "setup" => declarations.setup = rest(&mut fields),
                "ready" => declarations.ready = rest(&mut fields),
                "describe" => {
                    let what = word(&mut fields, at, "describe has no subject")?;
                    let subject = word(&mut fields, at, "describe has no id")?;
                    let text = rest(&mut fields).join(" ");
                    if text.is_empty() {
                        return Err(format!("bundle line {at}: describe has no text"));
                    }
                    match what.as_str() {
                        "node" => {
                            node_descriptions.insert(subject, text);
                        }
                        "hook" => {
                            let id = subject.parse::<u32>().map_err(|error| {
                                format!("bundle line {at}: hook id {subject:?}: {error}")
                            })?;
                            hook_descriptions.insert(id, text);
                        }
                        other => {
                            return Err(format!(
                                "bundle line {at}: describe subject {other:?} is not node or hook"
                            ));
                        }
                    }
                }
                "assert" => declarations
                    .assertions
                    .push(parse_assertion(&mut fields, at)?),
                "diagnostic" => {
                    let name = word(&mut fields, at, "diagnostic has no name")?;
                    let argv = rest(&mut fields);
                    if argv.is_empty() {
                        return Err(format!("bundle line {at}: diagnostic has no command"));
                    }
                    declarations
                        .diagnostics
                        .push(DiagnosticDeclaration { name, argv });
                }
                other => return Err(format!("bundle line {at}: unknown keyword {other:?}")),
            }
        }
        for node in &mut declarations.nodes {
            node.description = node_descriptions.get(&node.name).cloned();
        }
        for hook in &mut declarations.hooks {
            hook.description = hook_descriptions.get(&hook.id).cloned();
        }
        declarations.check_unique()?;
        Ok(declarations)
    }

    /// Reject repeated ids, which would make a finding's citation ambiguous.
    fn check_unique(&self) -> Result<(), String> {
        let mut hooks: Vec<u32> = self.hooks.iter().map(|hook| hook.id).collect();
        hooks.sort_unstable();
        if hooks.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("a fault bundle declares one hook id twice".to_owned());
        }
        let mut assertions: Vec<u32> = self.assertions.iter().map(|check| check.id).collect();
        assertions.sort_unstable();
        if assertions.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("a fault bundle declares one assertion id twice".to_owned());
        }
        Ok(())
    }

    /// The declaration of one assertion id, when the bundle names it.
    #[must_use]
    pub fn assertion(&self, id: u32) -> Option<&AssertionDeclaration> {
        self.assertions.iter().find(|check| check.id == id)
    }

    /// The declaration of one hook id, when the bundle names it.
    #[must_use]
    pub fn hook(&self, id: u32) -> Option<&HookDeclaration> {
        self.hooks.iter().find(|hook| hook.id == id)
    }

    /// The declaration of one node id, when the bundle names it.
    #[must_use]
    pub fn node(&self, id: u16) -> Option<&NodeDeclaration> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// The diagnostic command of that name, when the bundle names it.
    #[must_use]
    pub fn diagnostic(&self, name: &str) -> Option<&DiagnosticDeclaration> {
        self.diagnostics
            .iter()
            .find(|diagnostic| diagnostic.name == name)
    }

    /// Assertion ids the workload declares but this run never evaluated.
    ///
    /// A failure-only assertion is silent both when it holds and when it was
    /// never run, so a caller cannot read silence as a pass. This names the
    /// declared properties for which the run holds no evaluation evidence.
    #[must_use]
    pub fn unevaluated(&self, evaluated: &[u32]) -> Vec<u32> {
        self.assertions
            .iter()
            .map(|check| check.id)
            .filter(|id| !evaluated.contains(id))
            .collect()
    }
}

fn parse_assertion<'a>(
    fields: &mut impl Iterator<Item = &'a str>,
    at: usize,
) -> Result<AssertionDeclaration, String> {
    let keyword = word(fields, at, "assert has no kind")?;
    let kind = match keyword.as_str() {
        "always" => AssertionKind::Always,
        "sometimes" => AssertionKind::Sometimes,
        "reachable" => AssertionKind::Reachable,
        other => {
            return Err(format!(
                "bundle line {at}: assertion kind {other:?} is not always, sometimes, or reachable"
            ));
        }
    };
    let id = number_field(fields, at, "assert")?;
    let mut words = rest(fields);
    let mut reported_by_hook = None;
    if words.first().is_some_and(|word| word == "from") {
        let hook = words
            .get(1)
            .ok_or_else(|| format!("bundle line {at}: assert from has no hook id"))?;
        reported_by_hook = Some(
            hook.parse::<u32>()
                .map_err(|error| format!("bundle line {at}: hook id {hook:?}: {error}"))?,
        );
        words.drain(..2);
    }
    let meaning = words.join(" ");
    if meaning.is_empty() {
        return Err(format!("bundle line {at}: assert {id} has no meaning"));
    }
    Ok(AssertionDeclaration {
        id,
        kind,
        reported_by_hook,
        meaning,
    })
}

fn word<'a>(
    fields: &mut impl Iterator<Item = &'a str>,
    at: usize,
    complaint: &str,
) -> Result<String, String> {
    fields
        .next()
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("bundle line {at}: {complaint}"))
}

fn number_field<'a>(
    fields: &mut impl Iterator<Item = &'a str>,
    at: usize,
    what: &str,
) -> Result<u32, String> {
    let id = word(fields, at, &format!("{what} has no id"))?;
    id.parse::<u32>()
        .map_err(|error| format!("bundle line {at}: {what} id {id:?} is not a u32: {error}"))
}

fn rest<'a>(fields: &mut impl Iterator<Item = &'a str>) -> Vec<String> {
    fields.map(ToOwned::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUNDLE: &str = "\
setup /opt/harmony/setup.sh
node postgres /opt/harmony/node.sh
describe node postgres the seeded cluster, running as uid 70
ready /opt/harmony/ready.sh
hook 3 /opt/harmony/hooks.sh 3
describe hook 3 pg_amcheck --heapallindexed against cic_k_idx
assert always 2 from 3 every required heap tuple has a matching index entry
assert sometimes 24 the checker reached a verdict
diagnostic amcheck sh -c cat /run/amcheck.out
";

    #[test]
    fn a_bundle_declares_nodes_hooks_properties_and_diagnostics() {
        let declarations = Declarations::parse(BUNDLE).expect("parse");
        assert_eq!(declarations.format, DECLARATIONS_FORMAT);
        assert_eq!(declarations.setup, ["/opt/harmony/setup.sh"]);
        assert_eq!(declarations.ready, ["/opt/harmony/ready.sh"]);
        let node = declarations.node(0).expect("node 0");
        assert_eq!(node.name, "postgres");
        assert_eq!(
            node.description.as_deref(),
            Some("the seeded cluster, running as uid 70")
        );
        let hook = declarations.hook(3).expect("hook 3");
        assert_eq!(hook.argv, ["/opt/harmony/hooks.sh", "3"]);
        assert!(hook.description.is_some());
        let always = declarations.assertion(2).expect("assertion 2");
        assert_eq!(always.kind, AssertionKind::Always);
        assert_eq!(always.reported_by_hook, Some(3));
        assert_eq!(
            always.meaning,
            "every required heap tuple has a matching index entry"
        );
        assert_eq!(
            declarations.assertion(24).expect("assertion 24").kind,
            AssertionKind::Sometimes
        );
        assert_eq!(
            declarations.diagnostic("amcheck").expect("diagnostic").argv,
            ["sh", "-c", "cat", "/run/amcheck.out"]
        );
    }

    #[test]
    fn a_bundle_with_no_declarations_still_parses() {
        let declarations =
            Declarations::parse("node a /bin/a\nhook 1 /bin/h\n").expect("a bare bundle parses");
        assert!(declarations.assertions.is_empty());
        assert_eq!(declarations.node(0).expect("node 0").description, None);
        assert_eq!(declarations.hook(1).expect("hook 1").description, None);
    }

    #[test]
    fn every_declared_property_with_no_evaluation_is_named() {
        let declarations = Declarations::parse(BUNDLE).expect("parse");
        assert_eq!(declarations.unevaluated(&[2]), vec![24]);
        assert!(declarations.unevaluated(&[2, 24]).is_empty());
    }

    #[test]
    fn silence_means_something_different_for_each_kind() {
        assert_ne!(
            AssertionKind::Always.silence_means(),
            AssertionKind::Sometimes.silence_means()
        );
        assert_eq!(AssertionKind::Reachable.keyword(), "reachable");
    }

    #[test]
    fn malformed_declarations_name_their_line() {
        for (text, fragment) in [
            ("assert maybe 1 text\n", "is not always"),
            ("assert always x text\n", "is not a u32"),
            ("assert always 1\n", "has no meaning"),
            ("assert always 1 from\n", "has no hook id"),
            ("describe hook 1\n", "has no text"),
            ("describe cluster a b\n", "is not node or hook"),
            ("diagnostic amcheck\n", "has no command"),
            ("cheese 1\n", "unknown keyword"),
            ("hook 1 /bin/h\nhook 1 /bin/g\n", "hook id twice"),
            (
                "assert always 1 a\nassert sometimes 1 b\n",
                "assertion id twice",
            ),
        ] {
            let error = Declarations::parse(text).expect_err("malformed bundle");
            assert!(error.contains(fragment), "{error} lacks {fragment}");
        }
    }
}
