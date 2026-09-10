// SPDX-License-Identifier: AGPL-3.0-or-later
//! The presentation layer's checks: a workspace built by hand, then each
//! read-only verb rendered both ways. Advancing verbs need a KVM host and are
//! covered by the package's own tests and the Linux acceptance job.

use super::{
    Command, Common, ExecArgs, ExportArgs, InspectArgs, RunArgs, branches, describe_workspace,
    exec_request, findings, humanize, inspect, is_scalar, render, resolve_moment, run, run_bound,
    write_text,
};
use faults_workload::declarations::Declarations;
use faults_workload::target::{FaultAction, FaultObservations};
use faults_workload::workspace::{
    Branch, EvidenceRecord, Finding, History, MomentRecord, Record, StopReason, Workspace,
    WorkspaceFacts,
};

const BUNDLE: &str = "\
node 1 postgres /usr/lib/postgresql/bin/postgres -D /var/lib/postgresql/data
describe node 1 the database server under test
hook 3 /usr/bin/pg_amcheck --heapallindexed -d faultlab
describe hook 3 pg_amcheck --heapallindexed
assert always 2 from 3 every required heap tuple has a matching index entry
assert sometimes 5 a concurrent index build finished while updates ran
diagnostic amcheck-output sh -c cat /run/amcheck.out
";

/// A workspace holding one confirmed finding, one branch with a retained
/// console capture, and the declarations inspection reads meanings from.
fn fixture() -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("pg");
    let declarations = Declarations::parse(BUNDLE).expect("bundle parses");
    let facts = WorkspaceFacts {
        format: faults_workload::workspace::FORMAT.to_owned(),
        package: "faults".to_owned(),
        identity: "faults-consonance-whole-vm-v1".to_owned(),
        image_sha256: "aa".to_owned(),
        kernel_sha256: "bb".to_owned(),
        fault_agent_sha256: "cc".to_owned(),
        image: "pgcic-14.3.oci".to_owned(),
        root_seal: 1_000,
        horizon_nanos: 1_000_000_000,
        ram_mib: 2048,
        knobs: Vec::new(),
        seed: 7,
        executions: 100,
        declarations,
    };
    let mut workspace = Workspace::create(&root, facts).expect("create");
    let console = workspace
        .store_blob(b"LOG: starting\nNOTICE: chatter\nERROR: index tuple missing\n")
        .expect("blob");
    let observations = FaultObservations {
        moment: 12_300_000_000,
        violations: [2].into_iter().collect(),
        ..FaultObservations::default()
    };
    workspace
        .commit(vec![
            Record::Moment(Box::new(MomentRecord {
                id: "m-0001".to_owned(),
                branch: None,
                virtual_time: 12_300_000_000,
                history: History::Recorded,
                checkpoint: None,
                state_hash: Some("f00d".to_owned()),
                stop: Some(StopReason::Assertion { point: 2 }),
                observations: Some(observations.clone()),
            })),
            Record::Finding(Box::new(Finding {
                id: "bug-1".to_owned(),
                execution: 42,
                actions: vec![FaultAction::Wait, FaultAction::Hook(3)],
                standing: "00".to_owned(),
                observations,
                violations: vec![2],
                evaluated: vec![2],
                moment: "m-0001".to_owned(),
                confirmed: true,
                state_hash: "f00d".to_owned(),
            })),
            Record::Moment(Box::new(MomentRecord {
                id: "m-0002".to_owned(),
                branch: Some("trace".to_owned()),
                virtual_time: 9_300_000_000,
                history: History::Recorded,
                checkpoint: Some("dead".to_owned()),
                state_hash: Some("beef".to_owned()),
                stop: None,
                observations: None,
            })),
            Record::Branch(Box::new(Branch {
                name: "trace".to_owned(),
                source: "bug-1".to_owned(),
                start: "m-0002".to_owned(),
                head: "m-0002".to_owned(),
                history: History::Recorded,
                continuation_end: 12_300_000_000,
                inherited_actions: vec![FaultAction::Hook(3)],
                probe: false,
                pending_command: None,
            })),
            Record::Evidence(Box::new(EvidenceRecord {
                id: "ev-0001".to_owned(),
                kind: "console".to_owned(),
                moment: "m-0002".to_owned(),
                blob: console,
                bytes: 58,
                truncated: false,
                precision: "interval; console lines carry only the span they were drained over"
                    .to_owned(),
            })),
        ])
        .expect("commit");
    (dir, workspace)
}

fn text(value: &serde_json::Value) -> String {
    let mut shown = value.clone();
    humanize(&mut shown);
    let mut out = String::new();
    write_text(&shown, 0, &mut out);
    out
}

#[test]
fn findings_name_the_property_and_what_it_means() {
    let (_dir, workspace) = fixture();
    let value = findings(&workspace);
    let listed = &value["findings"][0];
    assert_eq!(listed["finding"], "bug-1");
    assert_eq!(listed["violations"][0], 2);
    assert_eq!(
        listed["meanings"][0], "2: every required heap tuple has a matching index entry",
        "an agent constructs its next command from the meaning, not the bare id"
    );
    assert_eq!(listed["verified"], true);
}

#[test]
fn an_empty_campaign_says_a_clean_run_is_not_proof() {
    let dir = tempfile::tempdir().expect("temp dir");
    let workspace = Workspace::create(
        &dir.path().join("empty"),
        WorkspaceFacts {
            format: faults_workload::workspace::FORMAT.to_owned(),
            ..WorkspaceFacts::default()
        },
    )
    .expect("create");
    let value = findings(&workspace);
    let next = value["next"][0].as_str().expect("a next line");
    assert!(
        next.contains("not proof of correctness"),
        "a budgeted campaign that found nothing must not read as a pass: {next}"
    );
}

#[test]
fn every_read_only_result_renders_the_same_facts_as_text() {
    let (_dir, workspace) = fixture();
    for value in [
        findings(&workspace),
        branches(&workspace),
        describe_workspace(&workspace),
        inspect(&workspace, &args("bug-1", None)).expect("inspect"),
    ] {
        let rendered = text(&value);
        let format = value["format"].as_str().expect("a format");
        assert!(
            rendered.contains(format),
            "the text form dropped the format {format}:\n{rendered}"
        );
    }
    let value = inspect(&workspace, &args("bug-1", None)).expect("inspect");
    let rendered = text(&value);
    for fact in ["m-0001", "12.3s", "recorded", "f00d"] {
        assert!(
            rendered.contains(fact),
            "the text form dropped {fact}:\n{rendered}"
        );
    }
}

#[test]
fn a_finding_summary_carries_the_hook_that_reported_it() {
    let (_dir, workspace) = fixture();
    let value = inspect(&workspace, &args("bug-1", None)).expect("inspect");
    let property = &value["properties"][0];
    assert_eq!(property["assertion"], 2);
    assert_eq!(property["reported_by_hook"], 3);
    assert_eq!(
        property["hook_command"],
        "/usr/bin/pg_amcheck --heapallindexed -d faultlab"
    );
    assert_eq!(
        value["preceding_inputs"].as_array().expect("inputs").len(),
        2
    );
    assert_eq!(value["verified"], true);
    assert!(
        value["verification_scope"]
            .as_str()
            .expect("a scope")
            .contains("boot"),
        "a confirmed finding must say what its replay started from"
    );
}

#[test]
fn a_declared_property_with_no_verdict_here_is_listed_unevaluated() {
    let (_dir, workspace) = fixture();
    let value = inspect(&workspace, &args("bug-1", None)).expect("inspect");
    let unevaluated = value["unevaluated"].as_array().expect("a list");
    assert_eq!(
        unevaluated,
        &vec![serde_json::json!(5)],
        "property 5 reached no verdict and must not read as satisfied"
    );
    assert!(
        value["unevaluated_means"]
            .as_str()
            .expect("an explanation")
            .contains("not a pass")
    );
}

#[test]
fn the_next_commands_are_runnable_for_the_point_they_describe() {
    let (_dir, workspace) = fixture();
    let value = inspect(&workspace, &args("bug-1", None)).expect("inspect");
    let next: Vec<&str> = value["next"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|line| line.as_str().expect("text"))
        .collect();
    assert!(
        next.iter().any(|line| line.contains("fork bug-1")),
        "{next:?}"
    );
    let value = inspect(&workspace, &args("trace@head", None)).expect("inspect");
    let next: Vec<&str> = value["next"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|line| line.as_str().expect("text"))
        .collect();
    assert!(
        next.iter().any(|line| line.contains("run trace")),
        "{next:?}"
    );
}

#[test]
fn console_reads_without_advancing_and_reports_its_precision() {
    let (_dir, workspace) = fixture();
    let value = inspect(&workspace, &args("trace@head", Some("console"))).expect("console");
    let lines = value["lines"].as_array().expect("lines");
    assert_eq!(lines.len(), 3);
    assert!(
        value["precision"]
            .as_str()
            .expect("precision")
            .contains("interval"),
        "console capture must not claim a precision it does not have"
    );
    assert_eq!(value["truncated"], false);
}

#[test]
fn a_filtered_page_reports_what_it_left_out() {
    let (_dir, workspace) = fixture();
    let mut filtered = args("trace@head", Some("console"));
    filtered.matches = Some("ERROR:".to_owned());
    let value = inspect(&workspace, &filtered).expect("console");
    assert_eq!(value["lines"].as_array().expect("lines").len(), 1);
    assert_eq!(
        value["truncated"], false,
        "a filter that dropped lines is not a capture that lost them"
    );
    assert_eq!(value["matched"], 1);

    let mut page = args("trace@head", Some("console"));
    page.limit = 2;
    let value = inspect(&workspace, &page).expect("console");
    assert_eq!(value["shown"], 2);
    assert_eq!(value["truncated"], true, "one line was left unread");
    assert!(
        value["next"]
            .as_str()
            .expect("a next page")
            .contains("--offset 2"),
        "a bounded view must name the command that reads the rest"
    );
}

#[test]
fn an_unavailable_view_names_the_views_this_build_has() {
    let (_dir, workspace) = fixture();
    let error = inspect(&workspace, &args("trace@head", Some("regs")))
        .expect_err("regs is not implemented");
    let message = error.to_string();
    assert!(message.contains("console"), "{message}");
    assert!(message.contains("hash"), "{message}");
}

#[test]
fn a_missing_capture_is_refused_rather_than_answered_from_elsewhere() {
    let (_dir, workspace) = fixture();
    let error =
        inspect(&workspace, &args("bug-1", Some("console"))).expect_err("no console at m-0001");
    assert!(
        error.to_string().contains("inspect m-0001"),
        "the error must name the command that lists what the point has: {error}"
    );
}

#[test]
fn an_unknown_selector_is_named_and_never_substituted() {
    let (_dir, workspace) = fixture();
    let error = inspect(&workspace, &args("bug-9", None)).expect_err("no such finding");
    assert!(error.to_string().contains("bug-9"), "{error}");
    let error = inspect(&workspace, &args("trace@1s", None)).expect_err("no such point");
    let message = error.to_string();
    assert!(message.contains("1s"), "{message}");
    assert!(
        message.contains("branches"),
        "the error must name the command that lists saved endpoints: {message}"
    );
}

#[test]
fn the_hash_view_returns_the_moments_own_hash() {
    let (_dir, workspace) = fixture();
    let value = inspect(&workspace, &args("bug-1", Some("hash"))).expect("hash");
    assert_eq!(value["moment"], "m-0001");
    assert_eq!(value["state_hash"], "f00d");
}

#[test]
fn a_run_without_a_bound_is_refused_with_both_forms() {
    let error = run_bound(&RunArgs {
        branch: "trace".to_owned(),
        run_for: None,
        until: None,
        within: None,
        extend: false,
        wall_seconds: 30,
        request_id: None,
    })
    .expect_err("no bound");
    let message = error.to_string();
    assert!(message.contains("--for 4s"), "{message}");
    assert!(message.contains("--within 4s"), "{message}");

    let error = run_bound(&RunArgs {
        branch: "trace".to_owned(),
        run_for: Some("4s".to_owned()),
        until: None,
        within: Some("4s".to_owned()),
        extend: false,
        wall_seconds: 30,
        request_id: None,
    })
    .expect_err("two bounds");
    assert!(error.to_string().contains("not both"), "{error}");
}

#[test]
fn a_watched_condition_parses_into_the_bound() {
    let bound = run_bound(&RunArgs {
        branch: "trace".to_owned(),
        run_for: None,
        until: Some("assertion:2:fail".to_owned()),
        within: Some("4s".to_owned()),
        extend: true,
        wall_seconds: 12,
        request_id: None,
    })
    .expect("a bound");
    assert_eq!(bound.within_nanos, 4_000_000_000);
    assert!(bound.until.is_some());
    assert!(bound.extend);
    assert_eq!(bound.wall_seconds, 12);
}

#[test]
fn exec_defaults_to_one_virtual_second_and_needs_a_target() {
    let request = exec_request(&ExecArgs {
        branch: Some("trace".to_owned()),
        at: None,
        within: None,
        extend: false,
        wall_seconds: 30,
        request_id: None,
        argv: vec!["sh".to_owned(), "-c".to_owned(), "echo hi".to_owned()],
    })
    .expect("a request");
    assert_eq!(
        request.bound.within_nanos,
        faults_workload::investigate::DEFAULT_EXEC_NANOS
    );
    assert_eq!(
        request.argv.len(),
        3,
        "argv survives without a shell round trip"
    );

    let error = exec_request(&ExecArgs {
        branch: None,
        at: None,
        within: None,
        extend: false,
        wall_seconds: 30,
        request_id: None,
        argv: vec!["true".to_owned()],
    })
    .expect_err("no target");
    assert!(error.to_string().contains("--at"), "{error}");

    let error = exec_request(&ExecArgs {
        branch: Some("trace".to_owned()),
        at: Some("bug-1".to_owned()),
        within: None,
        extend: false,
        wall_seconds: 30,
        request_id: None,
        argv: vec!["true".to_owned()],
    })
    .expect_err("two targets");
    assert!(error.to_string().contains("not both"), "{error}");
}

#[test]
fn a_verb_without_a_workspace_names_the_flag_that_supplies_one() {
    let common = Common {
        workspace: None,
        json: false,
    };
    let error = run(&common, Command::Findings).expect_err("no workspace");
    assert!(error.to_string().contains("-w DIR"), "{error}");
}

#[test]
fn export_writes_the_recorded_reproducer_separately_from_evidence() {
    let (dir, _workspace) = fixture();
    let common = Common {
        workspace: Some(dir.path().join("pg")),
        json: true,
    };
    run(
        &common,
        Command::Export(ExportArgs {
            finding: "bug-1".to_owned(),
            out: dir.path().join("shared"),
            evidence: true,
        }),
    )
    .expect("export");
    let shared = dir.path().join("shared");
    assert!(
        shared.join("reproducer.json").exists(),
        "the recorded reproducer is written on its own"
    );
    assert!(
        shared.join("evidence").is_dir(),
        "investigation evidence carries its own directory and provenance"
    );
}

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
#[test]
fn advancing_off_a_kvm_host_says_so_and_names_what_still_works() {
    let (dir, _workspace) = fixture();
    let common = Common {
        workspace: Some(dir.path().join("pg")),
        json: false,
    };
    let error = run(
        &common,
        Command::Fork(super::ForkArgs {
            source: "bug-1".to_owned(),
            rewind: "3s".to_owned(),
            name: "trace2".to_owned(),
            request_id: None,
        }),
    )
    .expect_err("no KVM here");
    let message = error.to_string();
    assert!(message.contains("Linux KVM host"), "{message}");
    assert!(message.contains("inspect"), "{message}");
}

/// A value that prints on its key's own line, as against one that opens an
/// indented block. Text and JSON come from one document, so this decides the
/// shape of every text rendering.
#[test]
fn only_values_without_structure_print_beside_their_key() {
    for scalar in [
        serde_json::json!("text"),
        serde_json::json!(7),
        serde_json::json!(true),
        serde_json::Value::Null,
    ] {
        assert!(is_scalar(&scalar), "{scalar} prints beside its key");
    }
    for structured in [serde_json::json!({"a": 1}), serde_json::json!([1, 2])] {
        assert!(
            !is_scalar(&structured),
            "{structured} opens an indented block"
        );
    }
}

/// Each level of nesting indents by exactly two spaces, in objects and in
/// arrays alike, so a reader can tell what a line belongs to.
#[test]
fn nesting_indents_by_one_level_each_time() {
    let mut out = String::new();
    write_text(
        &serde_json::json!({"outer": {"middle": {"inner": "leaf"}}}),
        0,
        &mut out,
    );
    assert_eq!(out, "outer:\n  middle:\n    inner: leaf\n", "{out}");

    let mut out = String::new();
    write_text(
        &serde_json::json!({"items": [{"name": "first"}, {"name": "second"}]}),
        0,
        &mut out,
    );
    assert_eq!(
        out, "items:\n  -\n    name: first\n  -\n    name: second\n",
        "{out}"
    );
}

/// Both renderings carry the same facts, and the JSON one is a whole document
/// a pipe can read.
#[test]
fn the_two_renderings_come_from_one_document() {
    let value = serde_json::json!({"branch": "trace", "moment": "m-0002"});
    let text = render(&value, false);
    assert_eq!(text, "branch: trace\nmoment: m-0002\n", "{text}");
    let json = render(&value, true);
    assert!(json.ends_with('\n'), "{json}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).expect("valid json"),
        value
    );
}

/// A virtual time inside an array is a duration a reader can act on, the same
/// as one at the top level.
#[test]
fn every_virtual_time_is_humanized_however_deeply_it_is_nested() {
    let mut value = serde_json::json!({
        "moments": [{"id": "m-0002", "virtual_time": 9_300_000_000_u64}]
    });
    humanize(&mut value);
    let item = &value["moments"][0];
    assert_eq!(item["virtual_time"], "9.3s");
    assert_eq!(item["virtual_time_nanos"], 9_300_000_000_u64);
}

/// `branch@time` names a point on that branch. Two branches can hold a point
/// at the same virtual time, and the selector must not return the other one.
#[test]
fn a_branch_selector_resolves_on_its_own_branch() {
    let (_dir, mut workspace) = fixture();
    workspace
        .commit(vec![
            Record::Moment(Box::new(MomentRecord {
                id: "m-0003".to_owned(),
                branch: Some("other".to_owned()),
                virtual_time: 9_300_000_000,
                history: History::Recorded,
                checkpoint: Some("cafe".to_owned()),
                state_hash: Some("cafe".to_owned()),
                stop: None,
                observations: None,
            })),
            Record::Branch(Box::new(Branch {
                name: "other".to_owned(),
                source: "bug-1".to_owned(),
                start: "m-0003".to_owned(),
                head: "m-0003".to_owned(),
                history: History::Recorded,
                continuation_end: 12_300_000_000,
                inherited_actions: vec![FaultAction::Hook(3)],
                probe: false,
                pending_command: None,
            })),
        ])
        .expect("commit");

    let selector = super::Selector::parse("trace@9.3s").expect("selector");
    assert_eq!(
        resolve_moment(&workspace, &selector).expect("a point on trace"),
        "m-0002"
    );
    let selector = super::Selector::parse("other@9.3s").expect("selector");
    assert_eq!(
        resolve_moment(&workspace, &selector).expect("a point on other"),
        "m-0003"
    );
}

fn args(target: &str, view: Option<&str>) -> InspectArgs {
    InspectArgs {
        target: Some(target.to_owned()),
        view: view.map(ToOwned::to_owned),
        since: None,
        limit: 200,
        offset: 0,
        matches: None,
    }
}
