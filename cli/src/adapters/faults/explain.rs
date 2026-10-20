// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{config::Explain, investigate, runs::Manifest, shrink, terminal};
use crate::{adapters::Request, config::Result};
use faults_workload::{
    FaultAction, FaultOperation, consonance::FaultTarget, package::ReplaySummary,
    target::SUPERVISOR_TICK_MICROS,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    num::NonZeroU16,
    path::Path,
};

const MARK: &str = "@@harmony-explain";
const LOG_LIMIT: u64 = 1 << 20;
const NOVEL_LINES: usize = 30;
const FAMILIAR_LINES: usize = 5;
const DATA_ENTRIES: usize = 40;
const HISTORY_LINES: usize = 200;

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn patterns(paths: &[String]) -> String {
    paths
        .iter()
        .map(|path| quote(path))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn script(settings: &Explain, offsets: &BTreeMap<String, u64>, history: bool) -> String {
    let mut text =
        format!("IFS='\n'\nm={MARK}\nsizes=\necho \"$m clock $(cat /proc/uptime 2>/dev/null)\"\n");
    if !settings.logs.is_empty() {
        let _ = write!(
            text,
            "for p in {}; do\n  for f in $p; do\n    [ -f \"$f\" ] && sizes=\"$sizes$(($(wc -c < \"$f\"))) $f\n\"\n  done\ndone\n",
            patterns(&settings.logs)
        );
    }
    if !settings.data.is_empty() {
        let _ = write!(
            text,
            "for p in {}; do\n  for d in $p; do\n    [ -d \"$d\" ] || continue\n    echo \"$m data $d\"\n    find \"$d\" -type f -exec cksum {{}} + 2>/dev/null\n    echo \"$m end\"\n  done\ndone\n",
            patterns(&settings.data)
        );
    }
    text.push_str("echo \"$m settled $(cat /proc/uptime 2>/dev/null)\"\n");
    if !settings.logs.is_empty() {
        let cases = offsets
            .iter()
            .map(|(path, offset)| format!("    {}) start={offset} ;;\n", quote(path)))
            .collect::<String>();
        let _ = write!(
            text,
            "for e in $sizes; do\n  size=${{e%% *}}\n  f=${{e#* }}\n  case \"$f\" in\n{cases}    *) start=0 ;;\n  esac\n  [ \"$size\" -lt \"$start\" ] && start=0\n  [ $((size - start)) -gt {LOG_LIMIT} ] && start=$((size - {LOG_LIMIT}))\n  echo \"$m log $size $start $f\"\n  tail -c +$((start + 1)) \"$f\" | head -c $((size - start))\n  echo\n  echo \"$m end\"\ndone\n"
        );
    }
    if let Some(command) = settings.history.as_ref().filter(|_| history) {
        let _ = write!(
            text,
            "echo \"$m history\"\n(\nunset IFS\n{command}\n) 2>&1\necho\necho \"$m end\"\n"
        );
    }
    text
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LogPart {
    pub size: u64,
    pub start: u64,
    pub text: String,
}

pub type Files = BTreeMap<String, (u64, u64)>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Capture {
    pub uptime: Option<f64>,
    pub settled: Option<f64>,
    pub logs: BTreeMap<String, LogPart>,
    pub data: BTreeMap<String, Files>,
    pub history: Option<String>,
}

pub fn parse(output: &str) -> Capture {
    let text = output.replace("\r\n", "\n");
    let end = format!("{MARK} end");
    let mut capture = Capture::default();
    let mut lines = text.split('\n');
    while let Some(line) = lines.next() {
        let Some(rest) = line
            .strip_prefix(MARK)
            .and_then(|rest| rest.strip_prefix(' '))
        else {
            continue;
        };
        let (kind, arguments) = rest.split_once(' ').unwrap_or((rest, ""));
        if matches!(kind, "clock" | "settled") {
            let value = arguments
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok());
            if kind == "clock" {
                capture.uptime = value;
            } else {
                capture.settled = value;
            }
            continue;
        }
        if !matches!(kind, "log" | "data" | "history") {
            continue;
        }
        let mut body = Vec::new();
        for line in lines.by_ref() {
            if line == end {
                break;
            }
            body.push(line);
        }
        match kind {
            "log" => {
                let mut fields = arguments.splitn(3, ' ');
                if let (Some(size), Some(start), Some(path)) =
                    (fields.next(), fields.next(), fields.next())
                    && let (Ok(size), Ok(start)) = (size.parse(), start.parse())
                {
                    capture.logs.insert(
                        path.to_owned(),
                        LogPart {
                            size,
                            start,
                            text: body.join("\n"),
                        },
                    );
                }
            }
            "data" => {
                let files = body
                    .iter()
                    .filter_map(|line| {
                        let mut fields = line.splitn(3, ' ');
                        let crc = fields.next()?.parse().ok()?;
                        let size = fields.next()?.parse().ok()?;
                        Some((fields.next()?.to_owned(), (crc, size)))
                    })
                    .collect();
                capture.data.insert(arguments.to_owned(), files);
            }
            _ => {
                let mut history = body.join("\n");
                history.truncate(history.trim_end().len());
                capture.history = Some(history);
            }
        }
    }
    capture
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Log {
    pub size: u64,
    pub text: String,
    pub partial: bool,
}

pub fn absorb(
    previous: &BTreeMap<String, Log>,
    parts: BTreeMap<String, LogPart>,
) -> BTreeMap<String, Log> {
    parts
        .into_iter()
        .map(|(path, part)| {
            let log = match previous.get(&path) {
                Some(prior) if part.start != 0 && prior.size == part.start => Log {
                    size: part.size,
                    text: prior.text.clone() + &part.text,
                    partial: prior.partial,
                },
                _ => Log {
                    size: part.size,
                    partial: part.start != 0,
                    text: part.text,
                },
            };
            (path, log)
        })
        .collect()
}

pub fn template(line: &str) -> String {
    let mut out = String::new();
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if token.is_empty() {
            return;
        }
        let digits = token.chars().any(|c| c.is_ascii_digit());
        let mixed = token.len() >= 6
            && token.chars().skip(1).any(char::is_uppercase)
            && token.chars().any(char::is_lowercase);
        out.push_str(if digits || mixed { "#" } else { token });
        token.clear();
    };
    for c in line.chars() {
        if c.is_alphanumeric() {
            token.push(c);
        } else {
            flush(&mut token, &mut out);
            out.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}

pub fn appended<'a>(before: &str, after: &'a str) -> &'a str {
    if let Some(rest) = after.strip_prefix(before) {
        return rest;
    }
    let common = before
        .split_inclusive('\n')
        .zip(after.split_inclusive('\n'))
        .take_while(|(old, new)| old == new && old.ends_with('\n'))
        .map(|(old, _)| old.len())
        .sum::<usize>();
    if common > 0 {
        return &after[common..];
    }
    let last = before.trim_end_matches('\n').rsplit('\n').next();
    if let Some(last) = last.filter(|line| !line.is_empty())
        && let Some(at) = after.rfind(last)
    {
        return after[at + last.len()..].trim_start_matches('\n');
    }
    after
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Line {
    pub text: String,
    pub count: usize,
    pub novel: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Excerpt {
    pub file: String,
    pub new_lines: usize,
    pub novel_lines: usize,
    pub partial: bool,
    pub lines: Vec<Line>,
}

pub fn excerpts(before: &BTreeMap<String, Log>, after: &BTreeMap<String, Log>) -> Vec<Excerpt> {
    let baseline = before
        .values()
        .flat_map(|log| log.text.lines())
        .map(template)
        .collect::<BTreeSet<_>>();
    after
        .iter()
        .filter_map(|(file, log)| {
            let prior = before.get(file);
            let new = appended(prior.map_or("", |log| log.text.as_str()), &log.text);
            let mut groups: Vec<(String, Line)> = Vec::new();
            let mut new_lines = 0;
            for line in new.lines().filter(|line| !line.trim().is_empty()) {
                new_lines += 1;
                let key = template(line);
                if let Some((_, group)) = groups.iter_mut().find(|(seen, _)| *seen == key) {
                    group.count += 1;
                } else {
                    let novel = !baseline.contains(&key);
                    groups.push((
                        key,
                        Line {
                            text: line.to_owned(),
                            count: 1,
                            novel,
                        },
                    ));
                }
            }
            if new_lines == 0 {
                return None;
            }
            let novel = groups
                .iter()
                .filter(|(_, line)| line.novel)
                .map(|(_, line)| line.clone())
                .collect::<Vec<_>>();
            let novel_lines = novel.iter().map(|line| line.count).sum();
            let mut lines = novel.into_iter().take(NOVEL_LINES).collect::<Vec<_>>();
            if lines.len() < FAMILIAR_LINES {
                let mut familiar = groups
                    .into_iter()
                    .map(|(_, line)| line)
                    .filter(|line| !line.novel)
                    .collect::<Vec<_>>();
                familiar.sort_by_key(|line| line.count);
                lines.extend(familiar.into_iter().take(FAMILIAR_LINES - lines.len()));
            }
            Some(Excerpt {
                file: file.clone(),
                new_lines,
                novel_lines,
                partial: log.partial || prior.is_some_and(|log| log.partial),
                lines,
            })
        })
        .collect()
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Entry {
    pub path: String,
    pub files: usize,
    pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Change {
    pub path: String,
    pub before: u64,
    pub after: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct DataChanges {
    pub root: String,
    pub present_before: bool,
    pub present_after: bool,
    pub added: Vec<Entry>,
    pub removed: Vec<Entry>,
    pub changed: Vec<Change>,
}

impl DataChanges {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.present_before == self.present_after
            && self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
    }
}

fn under<'a>(files: &'a Files, directory: &'a str) -> impl Iterator<Item = &'a String> + 'a {
    files
        .keys()
        .filter(move |path| path.starts_with(directory) && path[directory.len()..].starts_with('/'))
}

fn collapse(root: &str, moved: &BTreeSet<&String>, side: &Files, other: &Files) -> Vec<Entry> {
    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    for path in moved {
        let relative = path.strip_prefix(root).unwrap_or(path);
        let mut key = (*path).clone();
        let mut directory = root.trim_end_matches('/').to_owned();
        let parts = relative
            .trim_start_matches('/')
            .split('/')
            .collect::<Vec<_>>();
        for part in &parts[..parts.len().saturating_sub(1)] {
            directory.push('/');
            directory.push_str(part);
            if under(side, &directory).all(|file| moved.contains(file))
                && under(other, &directory).next().is_none()
            {
                key = format!("{directory}/");
                break;
            }
        }
        let entry = entries.entry(key.clone()).or_insert_with(|| Entry {
            path: key,
            ..Entry::default()
        });
        entry.files += 1;
        entry.bytes += side.get(*path).map_or(0, |(_, size)| *size);
    }
    entries.into_values().collect()
}

pub fn data_changes(root: &str, before: Option<&Files>, after: Option<&Files>) -> DataChanges {
    let empty = Files::new();
    let (old, new) = (before.unwrap_or(&empty), after.unwrap_or(&empty));
    let removed = old
        .keys()
        .filter(|path| !new.contains_key(*path))
        .collect::<BTreeSet<_>>();
    let added = new
        .keys()
        .filter(|path| !old.contains_key(*path))
        .collect::<BTreeSet<_>>();
    DataChanges {
        root: root.to_owned(),
        present_before: before.is_some(),
        present_after: after.is_some(),
        added: collapse(root, &added, new, old),
        removed: collapse(root, &removed, old, new),
        changed: old
            .iter()
            .filter_map(|(path, (crc, size))| {
                let (new_crc, new_size) = new.get(path)?;
                (new_crc != crc || new_size != size).then(|| Change {
                    path: path.clone(),
                    before: *size,
                    after: *new_size,
                })
            })
            .collect(),
    }
}

#[derive(Clone, Debug, Default)]
pub struct Names {
    pub nodes: Vec<String>,
    pub hooks: BTreeMap<u32, String>,
}

pub fn names(bundle: &str) -> Names {
    let mut names = Names::default();
    for line in bundle.lines() {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("node") => {
                if let Some(name) = fields.next() {
                    names.nodes.push(name.to_owned());
                }
            }
            Some("hook") => {
                if let Some(Ok(id)) = fields.next().map(str::parse) {
                    names.hooks.insert(id, fields.collect::<Vec<_>>().join(" "));
                }
            }
            _ => {}
        }
    }
    names
}

fn seconds(ticks: u64) -> String {
    format!("{:.2} s", (ticks * SUPERVISOR_TICK_MICROS) as f64 / 1e6)
}

pub fn describe(operation: &FaultOperation, names: &Names) -> String {
    let node = |index: u16| {
        names
            .nodes
            .get(usize::from(index))
            .cloned()
            .unwrap_or_else(|| format!("node {index}"))
    };
    let span = |ticks: NonZeroU16| seconds(u64::from(ticks.get()));
    match *operation {
        FaultOperation::Wait(ticks) => format!("wait {}", span(ticks)),
        FaultOperation::Kill(index, _) => {
            format!("kill {} (down until a later restart)", node(index))
        }
        FaultOperation::KillAll(_) => "kill every node (each down until a later restart)".into(),
        FaultOperation::Pause(index, ticks) => format!("pause {} for {}", node(index), span(ticks)),
        FaultOperation::Restart(index, ticks) => format!(
            "restart {} (back after {})",
            node(index),
            seconds((u64::from(ticks.get()) / 4).max(1))
        ),
        FaultOperation::Hook(id, _) => match names.hooks.get(&id) {
            Some(command) => format!("run hook {id} `{command}`"),
            None => format!("run hook {id}"),
        },
        FaultOperation::EventKill {
            node: index,
            rarity,
            ..
        } => format!(
            "kill {} at an instrumented event (rarity {rarity})",
            node(index)
        ),
        FaultOperation::EventPark {
            node: index,
            edges,
            hold_us,
            ..
        } => format!("park {} after {edges} edges for {hold_us} us", node(index)),
    }
}

pub fn alive(mask: u64, names: &Names) -> Vec<String> {
    (0..64_u16)
        .filter(|index| mask >> index & 1 == 1)
        .map(|index| {
            names
                .nodes
                .get(usize::from(index))
                .cloned()
                .unwrap_or_else(|| format!("node {index}"))
        })
        .collect()
}

pub fn fault_indices(actions: &[FaultAction]) -> Vec<usize> {
    actions
        .iter()
        .enumerate()
        .filter(|(_, action)| !matches!(action.operation, FaultOperation::Wait(_)))
        .map(|(index, _)| index)
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Point {
    pub step: usize,
    pub labels: Vec<String>,
    pub history: bool,
}

pub fn plan(actions: &[FaultAction], violation: usize) -> Vec<Point> {
    let mut points: BTreeMap<usize, Point> = BTreeMap::new();
    let mut add = |step: usize, label: String, history: bool| {
        let point = points.entry(step).or_insert(Point {
            step,
            labels: Vec::new(),
            history: false,
        });
        point.labels.push(label);
        point.history |= history;
    };
    let faults = fault_indices(actions);
    if faults.is_empty() {
        add(0, "start".into(), false);
    }
    for (number, index) in faults.into_iter().enumerate() {
        add(index, format!("before fault {}", number + 1), false);
        add(index + 1, format!("after fault {}", number + 1), false);
    }
    add(violation, "violation".into(), true);
    points.into_values().collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Span {
    pub title: String,
    pub from: usize,
    pub to: usize,
}

pub fn spans(actions: &[FaultAction], violation: usize, names: &Names) -> Vec<Span> {
    let faults = fault_indices(actions);
    let mut spans = Vec::new();
    for (number, &index) in faults.iter().enumerate() {
        spans.push(Span {
            title: format!(
                "Fault {}: {}",
                number + 1,
                describe(&actions[index].operation, names)
            ),
            from: index,
            to: index + 1,
        });
        if let Some(&next) = faults.get(number + 1)
            && next > index + 1
        {
            spans.push(Span {
                title: format!("Between fault {} and fault {}", number + 1, number + 2),
                from: index + 1,
                to: next,
            });
        }
    }
    let last = faults.last().map_or(0, |index| index + 1);
    if violation > last {
        spans.push(Span {
            title: if faults.is_empty() {
                "From the start to the violation".into()
            } else {
                "From the last fault to the violation".into()
            },
            from: last,
            to: violation,
        });
    }
    spans
}

#[derive(Clone, Debug, Default)]
pub struct Observed {
    pub step: usize,
    pub uptime: Option<f64>,
    pub settled: Option<f64>,
    pub logs: BTreeMap<String, Log>,
    pub data: BTreeMap<String, Files>,
    pub history: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Fault {
    pub number: usize,
    pub step: usize,
    pub operation: FaultOperation,
    pub description: String,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub guest_clock_seconds: Option<f64>,
    pub running_after: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Window {
    pub title: String,
    pub from_step: usize,
    pub to_step: usize,
    pub from_seconds: f64,
    pub to_seconds: f64,
    pub missing: Vec<String>,
    pub logs: Vec<Excerpt>,
    pub data: Vec<DataChanges>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Commands {
    pub replay: String,
    pub fix_check: String,
    pub branch: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Explanation {
    pub promise: Vec<String>,
    pub violation_step: usize,
    pub violation_seconds: f64,
    pub faults_before_shrink: Option<usize>,
    pub shrink_replays: Option<usize>,
    pub faults: Vec<Fault>,
    pub guest_clock_offset_seconds: Option<f64>,
    pub capture_seconds: Option<f64>,
    pub history: Option<String>,
    pub windows: Vec<Window>,
    pub commands: Commands,
    pub probes: usize,
    pub probe_replays: usize,
    pub reference_matches: bool,
    pub unverified_probes: Vec<usize>,
    pub shrink_seconds: u64,
    pub probe_seconds: u64,
    pub settings: Explain,
}

pub struct Inputs<'a> {
    pub out: &'a Path,
    pub actions: &'a [FaultAction],
    pub bundle: &'a str,
    pub image: &'a str,
    pub seed: u64,
    pub expected: &'a ReplaySummary,
    pub violation: usize,
    pub observed: &'a BTreeMap<usize, Observed>,
    pub settings: &'a Explain,
}

fn moments(expected: &ReplaySummary) -> BTreeMap<usize, (u64, u64)> {
    let mut moments = BTreeMap::new();
    for step in &expected.timeline {
        moments
            .entry(step.step as usize)
            .or_insert((step.observation.moment, step.observation.alive));
    }
    moments
}

pub fn explanation(inputs: &Inputs<'_>) -> Explanation {
    let actions = inputs.actions;
    let names = names(inputs.bundle);
    let moments = moments(inputs.expected);
    let at = |step: usize| {
        moments
            .get(&step)
            .map_or(0.0, |(moment, _)| *moment as f64 / 1e9)
    };
    let offset = inputs
        .observed
        .values()
        .find_map(|observed| Some(at(observed.step) - observed.uptime?));
    let faults = fault_indices(actions)
        .into_iter()
        .enumerate()
        .map(|(number, index)| Fault {
            number: number + 1,
            step: index + 1,
            operation: actions[index].operation,
            description: describe(&actions[index].operation, &names),
            start_seconds: at(index),
            end_seconds: at(index + 1),
            guest_clock_seconds: offset.map(|offset| at(index) - offset),
            running_after: moments
                .get(&(index + 1))
                .map(|(_, mask)| alive(*mask, &names))
                .unwrap_or_default(),
        })
        .collect();
    let windows = spans(actions, inputs.violation, &names)
        .into_iter()
        .map(|span| {
            let before = inputs.observed.get(&span.from);
            let after = inputs.observed.get(&span.to);
            let mut missing = Vec::new();
            for (step, observed) in [(span.from, before), (span.to, after)] {
                match observed {
                    None => missing.push(format!("step {step} was not probed")),
                    Some(Observed {
                        error: Some(error), ..
                    }) => missing.push(format!("probe at step {step} failed: {error}")),
                    _ => {}
                }
            }
            let empty = Observed::default();
            let (before, after) = (before.unwrap_or(&empty), after.unwrap_or(&empty));
            let roots = before
                .data
                .keys()
                .chain(after.data.keys())
                .collect::<BTreeSet<_>>();
            Window {
                title: span.title,
                from_step: span.from,
                to_step: span.to,
                from_seconds: at(span.from),
                to_seconds: at(span.to),
                missing,
                logs: excerpts(&before.logs, &after.logs),
                data: roots
                    .into_iter()
                    .map(|root| data_changes(root, before.data.get(root), after.data.get(root)))
                    .filter(|changes| !changes.is_empty())
                    .collect(),
            }
        })
        .collect();
    let out = inputs.out.display();
    let seed = inputs.seed;
    let run = |image: &str, suffix: &str| {
        format!(
            "harmony debug run {image} --config {out}/resolved.toml --seed {seed} --actions {out}/actions.json --out {out}-{suffix}"
        )
    };
    Explanation {
        promise: inputs.expected.violations.clone(),
        violation_step: inputs.violation,
        violation_seconds: at(inputs.violation),
        faults_before_shrink: None,
        shrink_replays: None,
        faults,
        guest_clock_offset_seconds: offset,
        capture_seconds: inputs
            .observed
            .values()
            .filter_map(|observed| Some(observed.settled? - observed.uptime?))
            .reduce(f64::max),
        history: inputs
            .observed
            .get(&inputs.violation)
            .and_then(|observed| observed.history.clone()),
        windows,
        commands: Commands {
            replay: run(inputs.image, "replay"),
            fix_check: run("FIXED.oci", "fixed"),
            branch: format!(
                "harmony branch {out} --step {} --shell --name {}-inspect",
                inputs.violation,
                inputs
                    .out
                    .file_name()
                    .map_or("explain".into(), |name| name.to_string_lossy())
            ),
        },
        probes: inputs.observed.len(),
        probe_replays: 0,
        reference_matches: false,
        unverified_probes: Vec::new(),
        shrink_seconds: 0,
        probe_seconds: 0,
        settings: inputs.settings.clone(),
    }
}

fn pretty_history(history: &str) -> String {
    let text = history
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .filter(serde_json::Value::is_object)
                .and_then(|value| serde_json::to_string_pretty(&value).ok())
                .unwrap_or_else(|| line.to_owned())
        })
        .collect::<Vec<_>>()
        .join("\n");
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > HISTORY_LINES {
        format!(
            "{}\n... {} more lines in explain.json",
            lines[..HISTORY_LINES].join("\n"),
            lines.len() - HISTORY_LINES
        )
    } else {
        text
    }
}

fn fence(text: &str) -> String {
    let mut ticks = 3;
    while text.contains(&"`".repeat(ticks)) {
        ticks += 1;
    }
    let fence = "`".repeat(ticks);
    format!("{fence}\n{text}\n{fence}\n")
}

fn render_data(text: &mut String, changes: &DataChanges) {
    let _ = write!(text, "- `{}`:", changes.root);
    if changes.present_before != changes.present_after {
        let _ = write!(
            text,
            " {}",
            if changes.present_after {
                "appeared"
            } else {
                "disappeared"
            }
        );
    }
    text.push('\n');
    let mut shown = 0;
    let mut line = |text: &mut String, entry: String| {
        if shown < DATA_ENTRIES {
            let _ = writeln!(text, "  - {entry}");
        }
        shown += 1;
    };
    for entry in &changes.removed {
        line(
            text,
            format!(
                "removed `{}` ({} files, {} bytes)",
                entry.path, entry.files, entry.bytes
            ),
        );
    }
    for entry in &changes.added {
        line(
            text,
            format!(
                "added `{}` ({} files, {} bytes)",
                entry.path, entry.files, entry.bytes
            ),
        );
    }
    for change in &changes.changed {
        line(
            text,
            format!(
                "changed `{}` ({} -> {} bytes)",
                change.path, change.before, change.after
            ),
        );
    }
    if shown > DATA_ENTRIES {
        let _ = writeln!(
            text,
            "  - {} more changes in explain.json",
            shown - DATA_ENTRIES
        );
    }
}

pub fn render(explanation: &Explanation) -> String {
    let mut text = String::new();
    let promise = explanation.promise.join("; ");
    let _ = writeln!(text, "# Explanation: {promise}\n");
    let _ = writeln!(
        text,
        "- Broken promise: `{promise}`, first observed at step {} ({:.2} s virtual time).",
        explanation.violation_step, explanation.violation_seconds
    );
    match (explanation.faults_before_shrink, explanation.shrink_replays) {
        (Some(before), Some(replays)) => {
            let _ = writeln!(
                text,
                "- Minimal faults: {} of {before}, found in {replays} replays. Every other fault is a wait of the same length.",
                explanation.faults.len()
            );
        }
        _ => {
            let _ = writeln!(
                text,
                "- Minimal faults: {} (the input was already shrunk).",
                explanation.faults.len()
            );
        }
    }
    if let Some(capture) = explanation.capture_seconds {
        let _ = writeln!(
            text,
            "- Capture: each probe recorded log sizes and data checksums within {capture:.2} s of guest time after its boundary. The guest keeps running during a probe, without the next fault."
        );
    }
    if let Some(offset) = explanation.guest_clock_offset_seconds {
        let _ = writeln!(
            text,
            "- Guest clock: CLOCK_MONOTONIC reads virtual time minus {offset:.2} s."
        );
    }
    let _ = write!(
        text,
        "- Probes: {} points in {} replays. ",
        explanation.probes, explanation.probe_replays
    );
    let _ = if !explanation.reference_matches {
        writeln!(
            text,
            "The probe-free reference replay DOES NOT match the reduced replay, so the probes may describe a different execution."
        )
    } else if explanation.unverified_probes.is_empty() {
        writeln!(
            text,
            "Each probe saw the same whole-state certificate as a probe-free reference replay that matches the reduced replay."
        )
    } else {
        writeln!(
            text,
            "The probes at steps {} saw a different state from the probe-free reference replay; their excerpts may describe a different execution.",
            explanation
                .unverified_probes
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let _ = writeln!(
        text,
        "- Time: shrink {} s, probes {} s.\n",
        explanation.shrink_seconds, explanation.probe_seconds
    );
    text.push_str("## Fault sequence\n\n");
    text.push_str(
        "| # | step | fault | starts (s) | ends (s) | guest clock (s) | running after |\n",
    );
    text.push_str("|---|---|---|---|---|---|---|\n");
    for fault in &explanation.faults {
        let _ = writeln!(
            text,
            "| {} | {} | {} | {:.2} | {:.2} | {} | {} |",
            fault.number,
            fault.step,
            fault.description,
            fault.start_seconds,
            fault.end_seconds,
            fault
                .guest_clock_seconds
                .map_or("-".into(), |value| format!("{value:.2}")),
            if fault.running_after.is_empty() {
                "none".into()
            } else {
                fault.running_after.join(", ")
            }
        );
    }
    text.push_str("\nTimes are virtual seconds since boot. The guest clock column is the same instant on the guest's CLOCK_MONOTONIC, which client histories use.\n\n");
    text.push_str("## Client-visible symptom\n\n");
    for violation in &explanation.promise {
        let _ = writeln!(text, "- Failed check: `{violation}`");
    }
    match &explanation.history {
        Some(history) => {
            let _ = write!(
                text,
                "\nOutput of the history command at step {}:\n\n{}",
                explanation.violation_step,
                fence(&pretty_history(history))
            );
        }
        None => text.push_str(
            "\nNo history command is configured. Set `history` in `[workload.options.explain]` or pass `--history`.\n",
        ),
    }
    text.push('\n');
    for window in &explanation.windows {
        let _ = writeln!(
            text,
            "## {} (steps {} to {}, {:.2} s to {:.2} s)\n",
            window.title, window.from_step, window.to_step, window.from_seconds, window.to_seconds
        );
        for missing in &window.missing {
            let _ = writeln!(text, "- {missing}");
        }
        if window.logs.is_empty() {
            text.push_str("No new log lines.\n\n");
        }
        for excerpt in &window.logs {
            let _ = writeln!(
                text,
                "`{}`: {} new lines, {} with a pattern not seen before step {}{}.\n",
                excerpt.file,
                excerpt.new_lines,
                excerpt.novel_lines,
                window.from_step,
                if excerpt.partial {
                    " (log capture truncated)"
                } else {
                    ""
                }
            );
            let lines = excerpt
                .lines
                .iter()
                .map(|line| {
                    let mark = if line.novel { "+" } else { " " };
                    if line.count > 1 {
                        format!("{mark} {} (x{})", line.text, line.count)
                    } else {
                        format!("{mark} {}", line.text)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            text.push_str(&fence(&lines));
            text.push('\n');
        }
        if window.data.is_empty() {
            if !explanation.settings.data.is_empty() {
                text.push_str("No data file changes.\n\n");
            }
        } else {
            text.push_str("Data file changes:\n\n");
            for changes in &window.data {
                render_data(&mut text, changes);
            }
            text.push('\n');
        }
    }
    text.push_str("## Reproduce and check a fix\n\n");
    let _ = write!(
        text,
        "Replay the reduced actions on the recorded image, then on a fixed image. A changed binary changes guest timing, so a clean replay is evidence for a fix, not proof.\n\n{}\nInspect the state at the violation:\n\n{}",
        fence(&format!(
            "{}\n{}",
            explanation.commands.replay, explanation.commands.fix_check
        )),
        fence(&explanation.commands.branch)
    );
    text.push_str("\nLines marked `+` have a pattern (digits and identifiers removed) that no captured log showed before the window. Data changes compare file lists, sizes and checksums.\n");
    text
}

struct Worker<'a> {
    out: &'a Path,
    settings: &'a Explain,
    logs: BTreeMap<String, Log>,
    observed: Vec<Observed>,
}

impl Worker<'_> {
    fn probe(&mut self, point: &Point, target: &mut FaultTarget) {
        let offsets = self
            .logs
            .iter()
            .map(|(path, log)| (path.clone(), log.size))
            .collect();
        let script = script(self.settings, &offsets, point.history);
        let directory = self.out.join("probes").join(point.step.to_string());
        let output = (|| -> Result<String> {
            fs::create_dir_all(&directory)?;
            fs::write(directory.join("probe.sh"), &script)?;
            let code =
                target.probe(|target| terminal::capture(target, &directory, script.as_bytes()))?;
            if code != 0 {
                return Err(format!("the probe script exited with {code}").into());
            }
            Ok(String::from_utf8_lossy(&fs::read(directory.join("terminal.log"))?).into_owned())
        })();
        let mut observed = Observed {
            step: point.step,
            ..Observed::default()
        };
        match output {
            Ok(output) => {
                let capture = parse(&output);
                self.logs = absorb(&self.logs, capture.logs);
                observed.logs = self.logs.clone();
                observed.data = capture.data;
                observed.uptime = capture.uptime;
                observed.settled = capture.settled;
                observed.history = capture.history;
            }
            Err(error) => observed.error = Some(error.to_string()),
        }
        self.observed.push(observed);
    }
}

struct Replayed {
    summary: ReplaySummary,
    observed: Vec<Observed>,
    certificates: BTreeMap<usize, [u8; 32]>,
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn replay_one(
    out: &Path,
    manifest: &Manifest,
    artifacts: &faults_workload::Artifacts,
    options: &faults_workload::package::Options,
    settings: &Explain,
    name: &str,
    share: Option<&BTreeMap<usize, Point>>,
) -> std::result::Result<Replayed, String> {
    let mut options = options.clone();
    options.output = out.join("probes").join(name);
    fs::create_dir_all(&options.output).map_err(|error| error.to_string())?;
    let mut worker = Worker {
        out,
        settings,
        logs: BTreeMap::new(),
        observed: Vec::new(),
    };
    let mut certificates = BTreeMap::new();
    let summary = faults_workload::package::execute_visiting(
        artifacts,
        &manifest.actions,
        &options,
        manifest.settle,
        &mut |step, target| {
            let step = step as usize;
            let point = share.and_then(|share| share.get(&step));
            if (share.is_none() || point.is_some())
                && let Some(certificate) = target.endpoint_certificate()
            {
                certificates.insert(step, certificate);
            }
            if let Some(point) = point {
                worker.probe(point, target);
            }
            Ok(())
        },
    )
    .map_err(|error| error.to_string())?;
    let record = serde_json::json!({
        "probes": share.map(|share| share.keys().copied().collect::<Vec<_>>()).unwrap_or_default(),
        "certificates": certificates
            .iter()
            .map(|(step, certificate)| (step.to_string(), hex(certificate)))
            .collect::<BTreeMap<_, _>>(),
        "summary": summary,
    });
    fs::write(
        options.output.join("replay.json"),
        serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(Replayed {
        summary,
        observed: worker.observed,
        certificates,
    })
}

fn probe_all(
    out: &Path,
    manifest: &Manifest,
    settings: &Explain,
    points: &[Point],
    jobs: usize,
) -> Result<(Replayed, Vec<Replayed>)> {
    let artifacts = manifest.fault_artifacts(out)?;
    let options = manifest.options(out)?;
    let probers = jobs.saturating_sub(1).clamp(1, points.len().max(1));
    let shares = (0..probers)
        .map(|worker| {
            points
                .iter()
                .skip(worker)
                .step_by(probers)
                .map(|point| (point.step, point.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .collect::<Vec<_>>();
    let tasks = std::iter::once(None)
        .chain(shares.iter().map(Some))
        .collect::<Vec<_>>();
    let threads = jobs.clamp(1, tasks.len());
    let mut outcomes = std::thread::scope(|scope| {
        let handles = (0..threads)
            .map(|thread| {
                let (artifacts, options, tasks) = (&artifacts, &options, &tasks);
                scope.spawn(move || {
                    (thread..tasks.len())
                        .step_by(threads)
                        .map(|index| {
                            let name = if index == 0 {
                                "reference".to_owned()
                            } else {
                                format!("replay-{index}")
                            };
                            (
                                index,
                                replay_one(
                                    out,
                                    manifest,
                                    artifacts,
                                    options,
                                    settings,
                                    &name,
                                    tasks[index],
                                ),
                            )
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|handle| {
                handle.join().unwrap_or_else(|_| {
                    vec![(usize::MAX, Err("an explain probe replay panicked".into()))]
                })
            })
            .collect::<Vec<_>>()
    });
    outcomes.sort_by_key(|(index, _)| *index);
    let mut replayed = outcomes
        .into_iter()
        .map(|(_, outcome)| outcome.map_err(Into::into))
        .collect::<Result<Vec<_>>>()?;
    let reference = replayed.remove(0);
    Ok((reference, replayed))
}

fn unverified(reference: &Replayed, probes: &[Replayed]) -> Vec<usize> {
    let mut steps = probes
        .iter()
        .flat_map(|replayed| {
            replayed.observed.iter().filter_map(|observed| {
                let seen = replayed.certificates.get(&observed.step);
                (seen.is_none() || seen != reference.certificates.get(&observed.step))
                    .then_some(observed.step)
            })
        })
        .collect::<Vec<_>>();
    steps.sort_unstable();
    steps
}

#[allow(clippy::disallowed_methods)]
fn clock() -> std::time::Instant {
    std::time::Instant::now()
}

#[cfg(feature = "frontend")]
pub fn explain(request: Request) -> Result<u8> {
    let saved = crate::runs::Saved::read(
        &request
            .selection
            .as_ref()
            .ok_or("select a search or branch")?
            .run,
    )?;
    explain_saved(request, saved)
}

pub fn explain_saved(request: Request, saved: crate::runs::Saved) -> Result<u8> {
    let selection = request
        .selection
        .clone()
        .ok_or("select a search or branch")?;
    if request.skip_shrink && selection.finding.is_some() {
        return Err("--no-shrink probes a recorded run; omit --finding".into());
    }
    let shrunk = request.skip_shrink
        || (selection.finding.is_none() && saved.path.join("shrink.json").is_file());
    let source = shrink::source(saved, selection.finding, "explain")?;
    let settings = request.explain.or(&source.manifest.config.explain);
    settings.validate()?;
    let out = request.destination.create()?;
    let mut manifest = source.manifest.inherit(&source.path, &out, "replay")?;
    manifest.actions = if shrunk {
        source.manifest.actions.clone()
    } else {
        source.actions.clone()
    };
    manifest.save(&out)?;
    let result = (|| -> Result<u8> {
        let started = clock();
        let (report, reduction) = if shrunk {
            (investigate::recorded_report(&source.manifest)?, None)
        } else {
            let reduced =
                shrink::shrink_into(&out, &manifest, &source, selection.finding, request.jobs)?;
            manifest.actions = reduced.actions.clone();
            (
                reduced.report,
                Some((reduced.faults_before, reduced.replays)),
            )
        };
        report.write(&out)?;
        manifest.pin_outcome(&report)?;
        fs::write(
            out.join("actions.json"),
            serde_json::to_vec_pretty(&serde_json::json!({ "actions": manifest.actions }))?,
        )?;
        let shrink_seconds = started.elapsed().as_secs();
        let expected = report
            .replays
            .first()
            .cloned()
            .ok_or("the reduced replay has no result")?;
        let (_, _, violation) = investigate::trajectory(&expected, true);
        let points = plan(&manifest.actions, violation);
        println!(
            "probing {} points of the reduced execution with {} replays",
            points.len(),
            request.jobs.clamp(1, points.len())
        );
        let started = clock();
        let (reference, probed) = probe_all(&out, &manifest, &settings, &points, request.jobs)?;
        let probe_seconds = started.elapsed().as_secs();
        let reference_matches = investigate::equivalent(&expected, &reference.summary);
        let unverified_probes = unverified(&reference, &probed);
        let observed = probed
            .iter()
            .flat_map(|replayed| replayed.observed.iter().cloned())
            .map(|observed| (observed.step, observed))
            .collect::<BTreeMap<_, _>>();
        let mut explanation = explanation(&Inputs {
            out: &out,
            actions: &manifest.actions,
            bundle: manifest.bundle.as_deref().unwrap_or_default(),
            image: manifest.config.image.as_deref().unwrap_or("IMAGE"),
            seed: manifest.config.seed,
            expected: &expected,
            violation,
            observed: &observed,
            settings: &settings,
        });
        if let Some((before, replays)) = reduction {
            explanation.faults_before_shrink = Some(before);
            explanation.shrink_replays = Some(replays);
        }
        explanation.probe_replays = probed.len() + 1;
        explanation.reference_matches = reference_matches;
        explanation.unverified_probes = unverified_probes.clone();
        explanation.shrink_seconds = shrink_seconds;
        explanation.probe_seconds = probe_seconds;
        fs::write(
            out.join("explain.json"),
            serde_json::to_vec_pretty(&explanation)?,
        )?;
        fs::write(out.join("explain.md"), render(&explanation))?;
        println!(
            "explained {} with {} faults: {}",
            explanation.promise.join("; "),
            explanation.faults.len(),
            out.join("explain.md").display()
        );
        if !reference_matches {
            return Err(format!(
                "the probe-free reference replay differs from the reduced replay; compare {} with report.json",
                out.join("probes/reference/replay.json").display()
            )
            .into());
        }
        if !unverified_probes.is_empty() {
            return Err(format!(
                "the probes at steps {unverified_probes:?} saw a different state from the reference replay"
            )
            .into());
        }
        Ok(0)
    })();
    super::workflow::finish(&out, &mut manifest, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use faults_workload::{
        package::ReplayStep,
        target::{FaultObservations, FaultStop},
    };

    fn ticks(value: u16) -> NonZeroU16 {
        NonZeroU16::new(value).unwrap()
    }

    fn action(operation: FaultOperation) -> FaultAction {
        FaultAction::new(operation, NonZeroU16::MIN)
    }

    fn schedule() -> Vec<FaultAction> {
        vec![
            action(FaultOperation::Kill(1, ticks(256))),
            action(FaultOperation::Wait(ticks(256))),
            action(FaultOperation::KillAll(ticks(256))),
            action(FaultOperation::Restart(0, ticks(256))),
            action(FaultOperation::Wait(ticks(256))),
        ]
    }

    const BUNDLE: &str = "node n1 /app/node 1\nnode n2 /app/node 2\nnode n3 /app/node 3\nhook 1 /app/net partition\n";

    fn log(text: &str) -> Log {
        Log {
            size: text.len() as u64,
            text: text.into(),
            partial: false,
        }
    }

    fn shell(script: &str) -> String {
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout)
            .unwrap()
            .replace('\n', "\r\n")
    }

    #[test]
    fn the_probe_script_reports_new_log_bytes_data_files_and_history() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().display().to_string();
        fs::create_dir_all(root.path().join("n1/streams/S")).unwrap();
        fs::write(root.path().join("n1/streams/S/1.blk"), b"block").unwrap();
        fs::write(root.path().join("n1/meta.inf"), b"meta").unwrap();
        fs::write(root.path().join("n1.log"), "start\nleader elected\n").unwrap();
        fs::write(root.path().join("n2.log"), "start").unwrap();
        let settings = Explain {
            logs: vec![format!("{base}/n*.log"), format!("{base}/missing.log")],
            data: vec![format!("{base}/n1")],
            history: Some("echo '{\"lost\":[7]}'; echo $((1 + 1))".into()),
        };
        let first = parse(&shell(&script(&settings, &BTreeMap::new(), true)));
        assert_eq!(first.logs.len(), 2);
        assert_eq!(
            first.logs[&format!("{base}/n1.log")].text,
            "start\nleader elected\n"
        );
        assert_eq!(first.logs[&format!("{base}/n2.log")].text, "start");
        assert_eq!(first.history.as_deref(), Some("{\"lost\":[7]}\n2"));
        assert_eq!(first.settled.is_some(), first.uptime.is_some());
        assert!(first.settled >= first.uptime);
        let files = &first.data[&format!("{base}/n1")];
        assert_eq!(
            files.keys().cloned().collect::<Vec<_>>(),
            vec![
                format!("{base}/n1/meta.inf"),
                format!("{base}/n1/streams/S/1.blk")
            ]
        );
        assert_eq!(files[&format!("{base}/n1/meta.inf")].1, 4);

        let logs = absorb(&BTreeMap::new(), first.logs);
        fs::write(
            root.path().join("n1.log"),
            "start\nleader elected\nstream S removed\n",
        )
        .unwrap();
        fs::write(root.path().join("n2.log"), "new").unwrap();
        fs::remove_dir_all(root.path().join("n1/streams")).unwrap();
        let offsets = logs
            .iter()
            .map(|(path, log)| (path.clone(), log.size))
            .collect();
        let second = parse(&shell(&script(&settings, &offsets, false)));
        assert_eq!(
            second.logs[&format!("{base}/n1.log")].text,
            "stream S removed\n"
        );
        assert_eq!(second.logs[&format!("{base}/n1.log")].start, 21);
        assert_eq!(second.history, None);
        let after = absorb(&logs, second.logs);
        assert_eq!(
            after[&format!("{base}/n1.log")].text,
            "start\nleader elected\nstream S removed\n"
        );
        assert_eq!(after[&format!("{base}/n2.log")].text, "new");
        let changes = data_changes(
            &format!("{base}/n1"),
            Some(files),
            second.data.get(&format!("{base}/n1")),
        );
        assert_eq!(
            changes.removed,
            vec![Entry {
                path: format!("{base}/n1/streams/"),
                files: 1,
                bytes: 5
            }]
        );
    }

    #[test]
    fn a_capped_capture_is_marked_partial() {
        let previous = BTreeMap::from([("a".to_owned(), log("old\n"))]);
        let parts = BTreeMap::from([(
            "a".to_owned(),
            LogPart {
                size: 100,
                start: 90,
                text: "tail\n".into(),
            },
        )]);
        let logs = absorb(&previous, parts);
        assert!(logs["a"].partial);
        assert_eq!(logs["a"].text, "tail\n");
    }

    #[test]
    fn templates_ignore_numbers_and_generated_identifiers() {
        assert_eq!(
            template(
                "[41] 2026/10/19 23:01:02.123 [INF] JetStream cluster new stream leader for 'S > xYqTzwLbQ'"
            ),
            template(
                "[87] 2026/10/19 23:05:09.981 [INF] JetStream cluster new stream leader for 'S > PmKdhQrstV'"
            )
        );
        assert_ne!(
            template("[INF] Self is new JetStream cluster metadata leader"),
            template("[WRN] Detected orphaned stream 'S'")
        );
    }

    #[test]
    fn appended_text_follows_the_earlier_capture() {
        assert_eq!(appended("a\nb\n", "a\nb\nc\n"), "c\n");
        assert_eq!(appended("x\nb\n", "b\nc\n"), "c\n");
        assert_eq!(appended("", "c\n"), "c\n");
        assert_eq!(appended("gone\n", "fresh\n"), "fresh\n");
        assert_eq!(
            appended("a\nb\nlost\n", "a\nb\nlater\nmore\n"),
            "later\nmore\n"
        );
    }

    #[test]
    fn excerpts_rank_lines_with_new_patterns_first() {
        let before = BTreeMap::from([
            (
                "n1.log".to_owned(),
                log("[1] heartbeat 1\n[1] heartbeat 2\n"),
            ),
            ("n2.log".to_owned(), log("[2] heartbeat 3\n")),
        ]);
        let after = BTreeMap::from([
            (
                "n1.log".to_owned(),
                log(
                    "[1] heartbeat 1\n[1] heartbeat 2\n[1] heartbeat 4\n[1] stream S deleted\n[1] stream S deleted\n",
                ),
            ),
            ("n2.log".to_owned(), log("[2] heartbeat 3\n")),
            ("n3.log".to_owned(), log("[3] heartbeat 5\n")),
        ]);
        let excerpts = excerpts(&before, &after);
        assert_eq!(
            excerpts.iter().map(|e| e.file.as_str()).collect::<Vec<_>>(),
            vec!["n1.log", "n3.log"]
        );
        assert_eq!(excerpts[0].new_lines, 3);
        assert_eq!(excerpts[0].novel_lines, 2);
        assert_eq!(
            excerpts[0].lines[0],
            Line {
                text: "[1] stream S deleted".into(),
                count: 2,
                novel: true
            }
        );
        assert!(!excerpts[0].lines[1].novel);
        assert!(!excerpts[1].lines[0].novel);
    }

    #[test]
    fn data_changes_report_whole_removed_directories_once() {
        let file = |crc, size| (crc, size);
        let before = Files::from([
            ("/d/meta/snap".to_owned(), file(1, 10)),
            ("/d/streams/S/1.blk".to_owned(), file(2, 20)),
            ("/d/streams/S/obs/c".to_owned(), file(3, 30)),
            ("/d/streams/T/1.blk".to_owned(), file(4, 40)),
        ]);
        let after = Files::from([
            ("/d/meta/snap".to_owned(), file(9, 0)),
            ("/d/streams/T/1.blk".to_owned(), file(4, 40)),
            ("/d/streams/U/1.blk".to_owned(), file(5, 50)),
        ]);
        let changes = data_changes("/d", Some(&before), Some(&after));
        assert_eq!(
            changes.removed,
            vec![Entry {
                path: "/d/streams/S/".into(),
                files: 2,
                bytes: 50
            }]
        );
        assert_eq!(
            changes.added,
            vec![Entry {
                path: "/d/streams/U/".into(),
                files: 1,
                bytes: 50
            }]
        );
        assert_eq!(
            changes.changed,
            vec![Change {
                path: "/d/meta/snap".into(),
                before: 10,
                after: 0
            }]
        );
        assert!(data_changes("/d", Some(&after), Some(&after)).is_empty());
        assert!(!data_changes("/d", Some(&after), None).is_empty());
    }

    #[test]
    fn the_plan_probes_both_sides_of_each_fault_and_the_violation() {
        let points = plan(&schedule(), 5);
        assert_eq!(
            points.iter().map(|point| point.step).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 5]
        );
        assert_eq!(points[3].labels, vec!["after fault 2", "before fault 3"]);
        assert!(
            points
                .iter()
                .all(|point| point.history == (point.step == 5))
        );
        let titles = spans(&schedule(), 5, &names(BUNDLE))
            .into_iter()
            .map(|span| (span.title, span.from, span.to))
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            vec![
                ("Fault 1: kill n2 (down until a later restart)".into(), 0, 1),
                ("Between fault 1 and fault 2".into(), 1, 2),
                (
                    "Fault 2: kill every node (each down until a later restart)".into(),
                    2,
                    3
                ),
                ("Fault 3: restart n1 (back after 0.64 s)".into(), 3, 4),
                ("From the last fault to the violation".into(), 4, 5),
            ]
        );
        let waits = vec![action(FaultOperation::Wait(ticks(1)))];
        assert_eq!(plan(&waits, 1).len(), 2);
        assert_eq!(spans(&waits, 1, &Names::default())[0].from, 0);
    }

    #[test]
    fn descriptions_name_nodes_and_hooks_from_the_bundle() {
        let names = names(BUNDLE);
        assert_eq!(
            describe(&FaultOperation::Pause(2, ticks(128)), &names),
            "pause n3 for 1.28 s"
        );
        assert_eq!(
            describe(&FaultOperation::Hook(1, ticks(256)), &names),
            "run hook 1 `/app/net partition`"
        );
        assert_eq!(
            describe(&FaultOperation::Kill(7, ticks(1)), &names),
            "kill node 7 (down until a later restart)"
        );
        assert_eq!(alive(0b101, &names), vec!["n1", "n3"]);
    }

    fn summary(actions: &[FaultAction]) -> ReplaySummary {
        let mut summary =
            ReplaySummary::from_observation(&FaultObservations::default(), Some([0; 32]), 0, 0);
        summary.violations = vec!["log: every acknowledged append is in every later read".into()];
        summary.timeline = (0..=actions.len())
            .map(|step| ReplayStep {
                step: step as u64,
                action: step.checked_sub(1).map(|index| actions[index]),
                observation: FaultObservations {
                    moment: 14_000_000_000 + step as u64 * 2_560_000_000,
                    alive: if step >= 3 { 0b001 } else { 0b101 },
                    stop: FaultStop::Deadline,
                    ..FaultObservations::default()
                },
                console: String::new(),
                settlement: false,
            })
            .collect();
        summary
    }

    #[test]
    fn a_probe_is_verified_only_when_its_state_matches_the_reference() {
        let replayed = |certificates: &[(usize, u8)], steps: &[usize]| Replayed {
            summary: summary(&[]),
            observed: steps
                .iter()
                .map(|&step| Observed {
                    step,
                    ..Observed::default()
                })
                .collect(),
            certificates: certificates
                .iter()
                .map(|&(step, byte)| (step, [byte; 32]))
                .collect(),
        };
        let reference = replayed(&[(0, 1), (1, 2), (2, 3)], &[]);
        let probes = [replayed(&[(0, 1), (2, 9)], &[0, 2]), replayed(&[], &[1])];
        assert_eq!(unverified(&reference, &probes), vec![1, 2]);
        assert!(unverified(&reference, &[replayed(&[(1, 2)], &[1])]).is_empty());
    }

    #[test]
    fn the_report_names_the_promise_faults_symptom_evidence_and_commands() {
        let actions = schedule();
        let expected = summary(&actions);
        let mut observed = BTreeMap::new();
        for step in 0..=5 {
            let mut point = Observed {
                step,
                uptime: Some(14.0 + step as f64 * 2.56 - 0.5),
                ..Observed::default()
            };
            let text = if step < 4 {
                "[1] ready\n".to_owned()
            } else {
                "[1] ready\n[1] removed stream S\n".to_owned()
            };
            point.logs.insert("/tmp/n1.log".into(), log(&text));
            point.data.insert(
                "/tmp/n1".into(),
                if step < 4 {
                    Files::from([("/tmp/n1/streams/S/1.blk".to_owned(), (1, 9))])
                } else {
                    Files::new()
                },
            );
            observed.insert(step, point);
        }
        observed.get_mut(&5).unwrap().history = Some("{\"missing\":3}".into());
        observed.get_mut(&2).unwrap().error = Some("guest refused".into());
        let settings = Explain {
            logs: vec!["/tmp/*.log".into()],
            data: vec!["/tmp/n1".into()],
            history: Some("check".into()),
        };
        let mut explanation = explanation(&Inputs {
            out: Path::new("/runs/x"),
            actions: &actions,
            bundle: BUNDLE,
            image: "app.oci",
            seed: 51,
            expected: &expected,
            violation: 5,
            observed: &observed,
            settings: &settings,
        });
        explanation.probe_replays = 2;
        explanation.reference_matches = true;
        assert_eq!(explanation.faults.len(), 3);
        assert_eq!(explanation.faults[1].step, 3);
        assert_eq!(explanation.faults[1].start_seconds, 19.12);
        assert_eq!(explanation.faults[1].running_after, vec!["n1"]);
        assert!((explanation.guest_clock_offset_seconds.unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(
            explanation.commands.fix_check,
            "harmony debug run FIXED.oci --config /runs/x/resolved.toml --seed 51 --actions /runs/x/actions.json --out /runs/x-fixed"
        );
        let restart = &explanation.windows[3];
        assert_eq!(restart.logs[0].lines[0].text, "[1] removed stream S");
        assert_eq!(restart.data[0].removed[0].path, "/tmp/n1/streams/");
        assert!(explanation.windows[1].missing[0].contains("guest refused"));
        let text = render(&explanation);
        for expected in [
            "# Explanation: log: every acknowledged append is in every later read",
            "| 2 | 3 | kill every node (each down until a later restart) | 19.12 | 21.68 | 18.62 | n1 |",
            "\"missing\": 3",
            "+ [1] removed stream S",
            "removed `/tmp/n1/streams/` (1 files, 9 bytes)",
            "harmony debug run app.oci --config /runs/x/resolved.toml --seed 51",
            "probe at step 2 failed: guest refused",
            "Each probe saw the same whole-state certificate as a probe-free reference replay",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
    }
}
