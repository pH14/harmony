<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Harmony developer-skill evaluation

This directory holds the evaluation contract and the runner that carries it
out. It evaluates the four [skills](../../skills/README.md) against an existing
application: PostgreSQL 14.3, scoped to `CREATE INDEX CONCURRENTLY`. Execution
goes through a replaceable adapter that drives the shipping CLI, which is
itself an evaluation target.

## Running it

`eval.py` is a `uv` script; it needs no installed dependencies.

```sh
cargo build --release -p harmony-cli
./benchmarks/skills/eval.py qualify --out runs/qualify
./benchmarks/skills/eval.py run --panel investigation --attempts 3 --out runs/pilot
./benchmarks/skills/eval.py report --out runs/pilot
```

`qualify` checks the fixture against the shipping CLI and reports whether the
agent adapter is available. `run` prepares each attempt's isolated directory,
launches the agent, freezes the submission, grades it, and writes
`report.json` and `report.md`. Arms alternate in a shuffled order under
matched budgets, and every attempt keeps its own record.

| File | Owns |
| --- | --- |
| [`eval.py`](eval.py) | Preparation, budgets, freezing, evaluation, reports |
| [`adapters.py`](adapters.py) | The agent adapters. `claude-code` drives the installed CLI; `echo` runs no model so CI can check the runner without credentials |
| [`panels.py`](panels.py) | Each panel's assignment, tool access, and artifact checks |
| [`fixture.py`](fixture.py) | The investigation panel's workspace, either adopted from a real search or derived from the historical case, and the preparation panels' source and contract |

Three panels ship:

| Panel | Starts from | Needs |
| --- | --- | --- |
| `investigation` | A workspace holding a recorded finding | Nothing beyond the CLI |
| `integration` | The pinned PostgreSQL release and a contract for `CREATE INDEX CONCURRENTLY` | Docker and KVM |
| `end-to-end` | The same, plus the workspace commands | Docker and KVM |

The preparation panels stage the release from
`bugs/historical/postgres-cic-corruption/case.json`'s pin and verify its
checksum; pass `--source-tarball` to use a local copy. Their grader reads the
submitted bundle through `harmony preflight --bundle`, so a submission is
graded by the same parser the product uses.

`--adapter` selects the agent, `--model` and `--effort` its settings. Nothing
about a fixture, a skill, or a grading rule names a model, so a different
provider is a different adapter and nothing else.

A derived fixture is assembled from the historical case's bundle, witness, and
detector output without executing a VM. It measures whether an agent can drive
the CLI over a recorded finding. It does not measure a real investigation, and
every attempt record and report says which source it used. Pass
`--recorded-workspace W` on a KVM host to use a workspace a real search wrote.

[`.github/workflows/skill-eval.yml`](../../.github/workflows/skill-eval.yml)
calls this runner. Pull requests run the `echo` adapter, which exercises
preparation, isolation, freezing, grading, and reporting without a model
credential. A scheduled or manual run calls a real model under explicit token,
tool-call, and wall ceilings, and refuses to start when no provider credential
is configured.

## Target

Use PostgreSQL 14.3 and scope the assignment to `CREATE INDEX CONCURRENTLY`
under concurrent updates and maintenance. Harmony already has an independently
qualified case: a 14.3 vulnerable arm, a 14.4 fixed control, `pg_amcheck` as an
external oracle, a probe, a search-produced witness, and a hosted nightly search.
Those materials are evaluator ground truth and must not enter the agent workspace.

Pin official PostgreSQL source archives and their hashes. The visible application
contract should describe the intended behavior of concurrent index construction
without identifying the historical defect, its mechanism, affected versions, or
fix. The task is to prepare an existing application for Harmony, not to modify
PostgreSQL semantics or repair PostgreSQL.

This initial evaluation is intentionally feature-scoped. Asking a bounded model
to discover and prioritize every PostgreSQL property would measure navigation of
a very large codebase more than the three Harmony skills.

## Agent assignment

Give each attempt this task, with concrete workspace paths supplied by the runner:

The shipped wording is `INTEGRATION_PROMPT` in [`panels.py`](panels.py), which
is what the runner freezes and what `qualify` digests.

Supply source, ordinary application documentation, pinned build dependencies,
Harmony SDK references, and development execution access. In the skills arm,
also supply the three skills. The documentation-only arm receives the same
tools and factual references. Do not supply evaluator expectations, reference
instrumentation, prior attempts, Git history, or the surrounding Harmony checkout.

Reading source and discovering limitations is legitimate. The blindness claim
is withheld evaluation context, not absence of model training familiarity.

## Compiler and runtime instrumentation

Treat semantic SDK calls, compiler/runtime coverage, and external oracles as
three independent layers. An application may need all three, and success in one
does not establish the others.

The build skill should first inventory each executable and shared library by
language, compiler, runtime, and build system. Use compiler or runtime coverage
instrumentation when Harmony has a supported path for that language and the
resulting observations are consumed by the chosen execution/search adapter.
Instrument the artifact that actually runs in Harmony, retain the runtime it
needs, preserve usable symbols, and validate callbacks during execution. Record
an explicit reason for relevant components left uninstrumented.

Do not prescribe compiler instrumentation for scripts, data-only components,
unsupported toolchains, or binaries whose instrumentation cannot reach a
Harmony consumer. Do not infer working search guidance merely from successful
compilation or the presence of callback symbols.

Antithesis follows the same conditional shape. Its setup process inventories
each service as instrumented, cataloged-only, or deliberately uninstrumented.
For C/C++, its current path uses Clang 13 or newer, one instrumentation runtime
definition at link time, `-fsanitize-coverage=trace-pc-guard -g`, a GNU build ID,
and retained DWARF symbols. Antithesis uses those callbacks for continuous
coverage feedback, report source mapping, and thread pausing.

PostgreSQL is therefore an applicable C target in principle. Harmony currently
implements the sanitizer coverage callback ABI and records threshold decisions,
but the existing PostgreSQL fault-search case does not compiler-instrument the
server, does not place the callback runtime in its OCI rootfs, and its search
adapter does not consume basic-block identities. Qualification must establish
the missing build, runtime, symbolization, and search-feedback path before the
eval requires compiler instrumentation. Until then, score accurate capability
assessment and the semantic oracle path; do not award credit for flags alone.

## Execution boundaries

| Component | Owns | Must not expose to the agent |
| --- | --- | --- |
| Model runner | Provider calls, tool dispatch, attempt budget | Provider credentials, host shell access |
| Workspace container | Application source, edits, builds, local tests | Host Docker socket, other workspaces, evaluator mounts |
| Execution service | Bounded Harmony runs of submitted artifacts | Arbitrary host commands, mutable evaluator state |
| Evaluator | Independent checks, submission verification, results | Held-out cases or their feedback during the attempt |

Use pinned OCI images for the workspace. Preinstall dependencies, restrict
workspace egress, and enforce CPU, memory, storage, process, and time limits.
Run submitted code in disposable isolation: its build scripts and workload are
untrusted even when its source project is familiar. The execution service must
accept artifacts and bounded run settings, not host paths or shell commands.

Initially qualify execution on Linux x86/KVM. Local macOS may orchestrate a
Linux worker through the same interface as CI; Docker alone does not provide
equivalent KVM execution on macOS. Do not label checks that omit Harmony
execution as end-to-end evaluations.

## Qualification before scoring

First verify the evaluator with a reference integration on the pinned target:

- A clean build executes the intended artifact inside Harmony.
- A bootstrap observation arrives through the real SDK path.
- Benign positive and negative checker fixtures distinguish valid and invalid
  results, including a test-only deliberate assertion failure.
- Missing instrumentation is detected rather than scored as a clean run.
- Independent property checks agree with known valid application histories.
- Recorded artifacts replay under the declared execution identity.
- If compiler instrumentation is declared supported, the executed PostgreSQL
  artifact emits callbacks through the packaged runtime, symbols resolve to
  source, and disabling instrumentation changes the intended observation or
  scheduling signal in a controlled comparison.

Do not require or claim an undiscovered application failure to qualify these
integration checks. A later discovery panel must have independently established
ground truth, valid controls, and a measured budget before grading agent attempts.

## Scoring

Keep integration quality, discovery outcomes, and cost separate. Start with
explicit verdicts and evidence rather than an arbitrary weighted total.

| Dimension | Acceptance evidence |
| --- | --- |
| Properties | Precise guarantees, assumptions, priorities, independent checks, and exercised preconditions |
| Instrumentation | Correct assertion semantics, stable identities, real delivery, observations consumed by the selected adapter |
| Build | Pinned inputs and toolchain; executed artifact is demonstrably instrumented |
| Validation | Positive and negative controls behave correctly; absent telemetry is recognized |
| Reporting | Claims match recorded evidence; uncertainty and failures are retained |

Freeze the submitted workspace before held-out evaluation. Check for semantic
application changes, fabricated verification, vacuous checks, and modification
of evaluation machinery. Do not let a model's own completion claim determine
the verdict. Human/model review may supplement executable checks but must be
reported separately with its rubric and reviewer identity.

## Local and nightly protocol

One runner owns preparation, model execution, submission freezing, evaluation,
and report generation. GitHub Actions invokes that runner rather than duplicating
its logic. Provider configuration is external to fixtures and skills.

The requested local profile is Sonnet 5 with medium reasoning. Resolve and record
the provider's exact model ID and supported effective settings during adapter
implementation; never silently substitute a model or ignore a requested setting.
The CI provider/model remains undecided. Model selection does not alter grading.

Begin with three fresh attempts per arm on the qualified integration fixture.
Pair skills and documentation-only arms under matched budgets, randomize their
execution order, and retain unsuccessful attempts. Use separate ceilings for
model tokens, tool calls, development runs, execution work, and wall time.
Provider errors and broken infrastructure produce an infrastructure verdict,
not an application or skill failure; retain retries and their costs.

After the manual pilot, schedule one matched pair nightly and aggregate results
over repeated nights. Changes to fixture, skills, prompt, model, or runner create
a new comparison cohort. Freeze skills within each batch. Once a held-out case
informs skill changes, it is development evidence for subsequent comparisons.

Paid runs use trusted scheduled/manual workflows. PR CI checks runner behavior,
fixture qualification, isolation, and reporting without model credentials.
Reuse guest build artifacts only with verified input identities. Cache build
dependencies separately from writable attempt workspaces.

Supervision may recover infrastructure according to a recorded policy. Semantic
help about properties, instrumentation, or builds marks an attempt assisted;
report it separately. Do not feed grading feedback back into a scored attempt.

## Retained evidence

Each attempt retains prompt and supplied-file manifest; source, skill, toolchain,
image and Harmony identities; provider/model and effective settings; transcript
and tool calls; patch and final workspace digest; build hashes; run evidence;
checks and verdicts; tokens, elapsed time, cost when available, and interventions.
Never include provider secrets in transcripts or published artifacts. Separate
agent-visible development logs from evaluator-only results.

Reproducibility means re-evaluating the frozen submission and replaying workload
artifacts. It does not promise identical model decisions on a fresh attempt.

## Where the runner falls short of this contract

All three panels run from one runner and are graded on artifacts. Two parts of
the contract above are met differently, and every attempt record says so:

- **Execution access is the CLI on the runner, not a separate service.** The
  contract asks for a service that owns KVM and accepts artifacts and bounded
  run settings. A preparation attempt instead gets `harmony` on its PATH and a
  per-panel command allowlist, on a runner that has Docker and `/dev/kvm`. The
  budget in `EXECUTION.md` is stated to the attempt and checked afterwards from
  the transcript rather than enforced by a service.
- **Isolation is a directory, not a pinned container.** The attempt's file
  tools are confined to it and its commands are the allowlist, but it shares
  the runner and the operator's agent configuration. Each attempt records
  `isolation`, and the report prints it.

PostgreSQL compiler instrumentation stays unqualified. The build, runtime,
symbolization, and search-feedback path are not established, and the fault
search consumes no basic-block identities, so the references tell an attempt
not to claim coverage-guided exploration from a flag. Score capability
assessment and the semantic oracle path; award nothing for flags alone.
