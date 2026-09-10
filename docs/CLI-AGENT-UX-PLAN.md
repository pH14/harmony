# Harmony CLI and agent UX: one delivery effort

## What we're solving

An agent should be able to take an application from documented guarantees to a
working Harmony integration, run a bounded experiment, and explain the results
with reproducible evidence. Today the CLI exposes execution and search, while
property design, instrumentation, builds, and investigation require substantial
outside knowledge. Shipping commands without guidance, or skills without a usable
execution path, leaves that workflow incomplete.

Build and evaluate the whole workflow together:

**properties → instrument → build → validate → search → investigate → report**

PostgreSQL is the only initial application. Scope preparation to concurrent index
construction under updates and maintenance. Use benign checker controls to qualify
the integration and existing evaluator-owned regression evidence for assessment.
Preserve application semantics. Do not expand into other databases, a daemon,
a graphical debugger, or an automatic root-cause engine.

This is the consolidated delivery plan, not a statement that implementation or
evaluation has passed. It supersedes the earlier separation between the
[investigation plan](BUG-INVESTIGATION-PLAN.md) and
[skill evaluation contract](../benchmarks/skills/README.md). Those documents retain
their detailed contracts; read only the section needed for the current step.

## One product surface

Ship four small skills: `harmony-properties`, `harmony-instrument`,
`harmony-build`, and `harmony-run`. The last covers experiment design, bounded
execution, investigation, and reporting. Share factual CLI/SDK references;
do not duplicate command specifications or teach a PostgreSQL solution recipe.

| Stage | Agent leaves behind | Product must explain |
| --- | --- | --- |
| Properties | Prioritized guarantees, assumptions, independent oracles, exercised preconditions | Safety, bounded progress, and reachability are different claims |
| Instrument | Checks, stable property IDs, bootstrap and reachability observations | Whether telemetry arrived and whether search consumes it |
| Build | Pinned recipe, runnable image, runtime and symbols, artifact identities | Which compiler/runtime capabilities are actually supported |
| Validate | Positive, negative, and missing-telemetry results | A checker that did not run has no integrity verdict |
| Search | Budgeted campaign, inputs, findings, retained checkpoints | What ran, what stopped it, and what remains untested |
| Investigate | Named branches, immutable moments, captured evidence | Original replay versus a deliberately modified continuation |
| Report | Evidence citations, verification scope, limitations, portable export | Which conclusions the evidence establishes |

Keep ordinary source editing and builds in the agent workspace. Do not invent
new CLI verbs just to mirror each skill. The skills use the same published
commands and error remedies a human uses.

The workload author supplies setup, readiness, service commands, and checks in
the image's bundle. A hook is a guest command that search may launch. Its stdout
directives become SDK reports through the fault agent. Hook IDs name commands;
assertion IDs name properties. Inspection must expose their declared meanings,
completion status, and evidence. Failure-only SDK assertions need separate
execution evidence: silence cannot mean success.

Compiler instrumentation is a required workstream for the applicable PostgreSQL
C build: complete and qualify artifact instrumentation, packaged callbacks,
source mapping, and consumed search feedback. Callback symbols or counts alone
do not establish useful coverage. Scheduling control is a separate capability
and must not be inferred from coverage. Other languages receive accurate
capability guidance, not new runtime implementations in this effort. An
unresolved PostgreSQL coverage path is an explicit incomplete deliverable.

## CLI contract

The commands below are the intended interface; several are not implemented yet.
`W` is a directory holding durable execution history, not a live VM session.
Each command opens it, performs bounded work, commits its result, and exits.
Guest time is frozen between calls.

| Command | Meaning |
| --- | --- |
| `harmony preflight` | Report host and artifact readiness, with actionable missing prerequisites |
| `harmony search --package faults app.oci --out W --seed 7 --executions 100 --wall-minutes 5` | Run a campaign with logical and host budgets; retain inspectable results |
| `harmony -w W findings` / `branches` | Discover findings and saved continuations |
| `harmony -w W inspect bug-1` | Explain the reported property, preceding inputs, evidence, and available diagnostics |
| `harmony -w W fork bug-1 --rewind 3s --name trace` | Create a continuation before the finding and return its immutable starting moment |
| `harmony -w W run trace --for 4s` | Advance the saved branch by at most four virtual seconds and save its endpoint |
| `harmony -w W run trace --until assertion:2:fail --within 4s` | Watch for a new failure report from property 2 within four virtual seconds |
| `harmony -w W exec trace --within 1s --request-id diagnostic-1 -- sh -c 'echo diagnostic'` | Execute in the guest, retain output and the resulting checkpoint |
| `harmony -w W inspect trace@head events --since MOMENT` | Read retained events starting at an immutable moment returned earlier |
| `harmony -w W export bug-1 --out shared --evidence` | Export the original recorded result and separately identified investigation evidence |

Watching an assertion does not launch its checker. `--within` is additional
virtual time; the separate host watchdog bounds host waiting. A deadline is
not a passing property. Advancement stops at the recorded continuation's end
unless `--extend` explicitly permits further execution under its final environment.
`exec --at SELECTOR` creates a probe without moving the source.

Text and versioned JSON must carry the same facts: immutable moment, branch,
virtual time, stop reason, command completion/exit status, property evaluation,
history status, verification scope, evidence references, and truncation. Bound
and paginate evidence. CLI help and errors must let an agent discover the next
valid command without reading internal source.

Determinism is a hard contract. A cold fork without intervention inherits the
remaining inputs at their original coordinates and reproduces the same state,
events, and virtual moment. Splitting execution across processes changes nothing.
`exec` makes history modified because its delivery is not in the reproducer;
retain its checkpoint. Commit checkpoint, evidence, request result, and branch
head atomically. Retrying a committed request returns its result; interrupted
execution must never be silently reinjected. Verification identifies both its
starting anchor and target; a modified checkpoint cannot establish replay from boot.

## Implement in five integrated slices

Each slice includes its commands, guidance, and executable acceptance evidence.
These are implementation checkpoints within one release, not independent projects.

1. **Qualification and contracts.** Define shared artifact identities, property
   metadata, JSON results, budgets, and capability reporting. Qualify bootstrap
   delivery and benign valid/invalid/missing-checker controls on Linux x86/KVM.
   Prove cold continuation and split-run identity before building investigation
   around snapshots. Start in `workloads/faults/src/consonance.rs`; verify actual
   endpoint export and inherited input transitions rather than setup export or
   reseeding. Record executable failures before changing these paths.
2. **Prepare and observe.** Complete the PostgreSQL build/runtime/coverage
   consumer path and evidence needed to assess it. Write the three preparation
   skills against demonstrated capabilities. Emit the property and diagnostic
   metadata that inspection will consume. Exercise missing instrumentation and
   unused observations as negative controls.
3. **Search and investigate.** Connect package outputs to a durable workspace
   and CLI dispatch. Add listing, inspection, fork, bounded run, persistent exec,
   probe, and export. Reuse package advancement during exec so scheduled inputs
   and guest service requests continue. Test cold restarts, boundary crossings,
   timeout/pending commands, lost replies, nonzero exits, and unchanged sources.
   Write `harmony-run` and the human walkthrough against the shipped interface.
4. **Agent evaluation.** Implement one runner for isolated preparation, model
   calls, tool dispatch, freezing submissions, independent evaluation, and
   reports. Exercise the actual CLI through the execution adapter; a privileged
   alternate API cannot substitute for CLI usability. Run the panels below and
   use development attempts to repair product friction before freezing a cohort.
5. **Local pilot and CI.** Run matched fresh attempts locally, inspect retained
   evidence, then have GitHub Actions invoke that same runner. PR checks exercise
   contracts and runner behavior without paid credentials. Trusted manual and
   nightly jobs run the model profiles with explicit cost and execution ceilings.

Use owning-crate checks and existing CI conventions. Linux VM acceptance is
mandatory for execution claims; unavailable hardware is unrun. Exercise any new
unsafe logic under Miri and document its invariant. Preserve old replay artifacts.

## Evaluate the whole experience without hiding the cause of failure

Use three panels with identical factual references and tool access in the skills
and documentation-only arms:

| Panel | Starting material | Independent outcome |
| --- | --- | --- |
| Integration | Source and ordinary feature contract | Meaningful properties, semantics-preserving checks, reproducible build, real telemetry and controls |
| Investigation | A qualified prepared workload and retained finding | Correct CLI use, exact original replay, preserved source, cited guest evidence, appropriately limited explanation |
| End to end | Source and ordinary feature contract | Preparation through bounded campaign, investigation when a finding exists, and trustworthy final report |

Keep integration quality, discovery, investigation, and cost separate. A bounded
campaign without a finding is not proof of correctness and need not imply bad
integration. The investigation panel ensures rare discovery cannot prevent
measurement of the CLI. Grade claims against artifacts, not model self-reports.

Give the agent a pinned container with source, dependencies, SDK/CLI references,
and development execution access. Keep evaluator fixtures, reference solutions,
historical explanations, prior transcripts, Git history, and held-out feedback
outside it. The trusted service accepts artifacts and bounded requests, exposes
the shipping CLI semantics, and owns KVM access. Do not expose provider secrets,
host shell access, a Docker socket, or evaluator mounts. Local macOS orchestration
uses the same Linux worker interface as CI.

Freeze skills and fixture before scoring. Begin with three fresh attempts per
arm per panel; record a separate result for every attempt. Match token, tool,
development-run, execution-work, and wall budgets and randomize arm order.
The requested local profile is Sonnet 5 / medium; resolve the actual provider
identifier and effective setting without substitution. CI model selection remains
open. A stronger-model control is diagnostic and reported separately. Semantic
supervisor help marks an attempt assisted; infrastructure recovery is recorded.

Use the installed Claude Code CLI's `claude -p` as the first local agent adapter;
do not require a new direct provider integration. It supports `--model`,
`--effort medium`, and `--output-format stream-json --verbose`. Resolve and pin
the Sonnet 5 model identifier before scored runs and record the Claude Code
version. Launch fresh attempts in the isolated workspace with controlled skills,
settings, and tool access. Capture the event stream, usage, exit status, and
runner-enforced budgets. Keep this subprocess adapter replaceable so CI can use
another agent/provider without changing fixtures, grading, or report formats.

Local smoke check succeeded with Claude Code 2.1.263 using
`claude -p 'Reply with exactly OK.' --model claude-sonnet-5 --effort medium
--output-format json --tools '' --no-session-persistence --strict-mcp-config`.
The result was `OK`, exit status 0, with `claude-sonnet-5` in model usage.
Codex's filesystem sandbox reported no login; the same command outside that
sandbox used the existing login successfully. Keep authentication in the trusted
runner and validate it before launching attempts; do not assume credentials are
available inside a fresh container. This smoke check establishes local access,
not container isolation or completed evaluation qualification.

Retain supplied-context manifests, revisions and artifact digests, transcripts,
patches, frozen submissions, CLI evidence, evaluator verdicts, interventions,
tokens, time, and available cost. Infrastructure failures stay distinct from
agent failures. Re-evaluation of submissions and execution replay must work;
identical future model decisions are not promised. After the pilot, schedule
matched pairs and aggregate within cohorts; changes to model, fixture, skills,
prompt, or runner start a new cohort.

## Done means

All four skills and the documented CLI workflow ship together; compiler feedback
and cold continuation have real execution evidence; all three evaluation panels
run from one local/CI runner; a completed pilot has frozen artifacts and independent
verdicts; and the nightly workflow is wired to that runner. Paid nightly activation
requires the chosen provider configuration. Report measured agent success rates
and unresolved failures rather than declaring success from plausible transcripts.
