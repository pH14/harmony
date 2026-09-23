# Continuous integration contract

`scripts/ci_contract.py` is the registry: every workflow, its owner, its jobs,
their trigger classes and their budgets. `scripts/custom-lints.py` checks the
checked-in workflows against it, `scripts/semantic-lints.py` asks a judge the
questions a parser cannot answer, and this document explains what the registry
means. A new workflow is registered once, in the registry.

## Organization

A workflow name reads `Category / Owner` or `Category / Owner / Workload`.

- **Category**: `Checks`, `Benchmarks` or `Release`.
- **Owner**: a component (`Repository`, `Consonance`, `Dissonance`, `Harmony`)
  or a composition (`Harmony Host Compatibility`, `Dissonance Workloads`,
  `Harmony Workloads`).
- **Workload**: the workload family a composition workflow covers, such as
  `NES`, `OCI` or `Historical Bugs`.

A component owns its own code. A composition owns an assembly of components
executing a workload. `Analysis` groups a component's coverage, Miri, mutation
testing and proofs under that component, so `Checks / Consonance / Analysis`
analyses Consonance and nothing else.

`Smoke`, `Acceptance`, `Nightly`, `Validation`, `Test` and `Quality` are retired
categories. They described when a workflow ran or how thorough it was instead of
what owns it, and the linter rejects them.

## The workflow tree

| Workflow | File | Automatic triggers |
| --- | --- | --- |
| `Checks / Repository` | `repository-checks.yml` | pull_request, push |
| `Checks / Harmony Host Compatibility` | `harmony-host-compatibility.yml` | pull_request, push |
| `Checks / Consonance` | `consonance-checks.yml` | pull_request, push |
| `Checks / Consonance / Analysis` | `consonance-analysis.yml` | pull_request, push, schedule, workflow_dispatch |
| `Checks / Consonance / Hardware Qualification` | `consonance-hardware-qualification.yml` | schedule, workflow_dispatch |
| `Checks / Consonance / Guest Runtime Qualification` | `consonance-runtime-qualification.yml` | schedule, workflow_dispatch |
| `Checks / Consonance / Kernel XSAVE Qualification` | `consonance-kernel-xsave-qualification.yml` | workflow_dispatch |
| `Checks / Dissonance` | `dissonance-checks.yml` | pull_request, push |
| `Checks / Dissonance / Analysis` | `dissonance-analysis.yml` | schedule, workflow_dispatch |
| `Checks / Harmony` | `harmony-checks.yml` | pull_request, push |
| `Checks / Harmony / Analysis` | `harmony-analysis.yml` | pull_request, push, schedule, workflow_dispatch |
| `Checks / Dissonance Workloads / NES` | `dissonance-workloads-nes-checks.yml` | pull_request, push |
| `Checks / Harmony Workloads / NES` | `harmony-workloads-nes-checks.yml` | pull_request, push, schedule, workflow_dispatch |
| `Checks / Harmony Workloads / OCI` | `harmony-workloads-oci-checks.yml` | pull_request, push, schedule, workflow_dispatch |
| `Benchmarks / Dissonance Workloads / NES` | `dissonance-workloads-nes-benchmarks.yml` | schedule, workflow_dispatch |
| `Benchmarks / Harmony Workloads / NES` | `harmony-workloads-nes-benchmarks.yml` | schedule, workflow_dispatch |
| `Benchmarks / Harmony Workloads / Historical Bugs` | `harmony-workloads-historical-bugs.yml` | schedule, workflow_dispatch |
| `Release / Harmony` | `release.yml` | push (version tags) |

`Checks / Dissonance / Analysis` ships coverage only. The searcher has no
mutation baseline, and adding one is separate work.

## Dissonance Workloads and Harmony Workloads

The same game runs two ways, and neither substitutes for the other.

- **`Dissonance Workloads / NES`** drives native QuickNES through the Dissonance
  adapter. It exercises the search loop, the archive and the game's own
  interpretation without a virtual machine.
- **`Harmony Workloads / NES`** runs the same game inside a Consonance guest. It
  exercises snapshot and restore, the guest protocol and the assembled product.

`scripts/ci_contract.py` registers the pair in `NES_COMPOSITIONS` with the
backend each one runs, and the linter requires each composition to keep a
bounded Checks workflow and a full Benchmarks workflow, and requires each
workflow to actually run its registered backend. A Harmony composition reduced
to native execution alone fails `ci-nes-compositions`.

## Bounded checks and full benchmarks

Every job declares a trigger class.

- **`pr`** jobs run on pull requests and on pushes to main, and finish inside 15
  minutes. This bound holds for `pull_request`, `pull_request_target` and
  `merge_group`.
- **`full`** jobs run on a schedule or a manual dispatch and declare their own
  ceiling.

A job a pull request reaches never starts a full capability search, whatever
budget it declares. `ci_contract.FULL_SEARCH_COMMANDS` lists the commands that
start one, and `ci-trigger-routing` rejects them in a `pr` job.

A Checks workflow holds `full` work only through an exception registered beside
the job, which states why the work cannot fit the bound:

| Workflow | Job | Minutes | Reason |
| --- | --- | --- | --- |
| `Checks / Consonance / Analysis` | `Miri — <Crate> (Whole Crate)` | 320 | Interpreting a whole unsafe crate under Miri takes hours, so pull requests get the tests the change reaches. |
| `Checks / Consonance / Analysis` | `Coverage` | 30 | An instrumented build and run of every Consonance crate exceeds the pull request budget. |
| `Checks / Consonance / Analysis` | `Mutation Testing — Shard <N>/16` | 320 | Mutation testing rebuilds the component once per mutant. |
| `Checks / Dissonance / Analysis` | `Coverage` | 30 | An instrumented build and run of the searcher exceeds the pull request budget. |
| `Checks / Harmony / Analysis` | `Miri — <Crate> (Whole Crate)` | 240 | Interpreting a whole adapter crate under Miri takes hours, so pull requests get the tests the change reaches. |
| `Checks / Harmony / Analysis` | `Coverage` | 30 | An instrumented build and run of the CLI exceeds the pull request budget. |
| `Checks / Harmony / Analysis` | `Mutation Testing — Shard <N>/4` | 320 | Mutation testing rebuilds the CLI once per mutant. |
| `Checks / Harmony Workloads / NES` | `Backend Equivalence` | 75 | Comparing the native and Consonance backends builds the exact guest runtime and both game images. |
| `Checks / Harmony Workloads / OCI` | `Docker` | 90 | Building the pinned Docker workload image and booting it twice under nested KVM exceeds the pull request budget. |
| `Checks / Harmony Workloads / OCI` | `K3s` | 90 | Building the pinned K3s workload image and bringing a cluster up twice exceeds the pull request budget. |

An exception narrows which jobs may mix trigger classes; it does not let a `pr`
job run longer.

## Change selection

`.github/actions/ci-scope` is the single selector. A job that selects work runs
it once, unconditionally, under the id `scope`, after a complete-history
checkout, and guards its own steps with `steps.scope.outputs.enabled` joined by
`&&`. `ci_contract.SCOPE_KINDS` records which workflow owns each kind.

An unselected job finishes successfully with a summary saying it was not
applicable. A selection or diff error fails closed. A selected test that fails
still fails its job and still uploads the evidence it produced. A required
hardware test is never downgraded to a successful skip: missing artifacts fail
the job.

## Ignored tests

A test marked `#[ignore]` needs something a plain `cargo test` lacks, such as a
hypervisor, a built guest image, or a person reading its output. Each one has a
runner in `scripts/ci_contract.py`, named as `<binary-id> <test>` the way
`cargo nextest list` prints it, with `*` for a whole binary:

- `Job.ignored_tests` lists what a CI job runs with `--ignored`.
  `ci-ignored-tests` checks that the job's steps pass `--ignored` and name each
  binary and test it lists.
- `HOST_TESTS` lists tests that need a hypervisor no hosted runner offers, keyed
  by the `<os>-<arch>` of a machine that has one. The pre-push hook runs the
  entry for the machine it is on, selected by
  `python3 scripts/ci_contract.py host-filter`.
- `MANUAL_TESTS` lists tests a person runs by hand, each with what it is for.

`Checks / Repository` runs `python3 scripts/check-test-partition.py ignored` on
x86-64 Linux, arm64 Linux and arm64 macOS. It lists every ignored test that
builds on that host and fails on one with no runner.

## Audiovisual evidence

Both NES compositions publish video with game audio: a bounded capture in the
Checks workflow and every scenario in the Benchmarks workflow.

`workloads/nes/src/bin/nes-film.rs` renders every evaluation matrix. It reads
the input and `result.json` a run already recorded, replays them, and writes
`film.json` beside `witness.mp4`. The Super Tilt Bro campaign renders its own
`witness.mp4` through `stb-campaign` against the champion it recorded. Nothing
renders from a second search.

- A native run's film requires the replayed witness to equal the one the run
  recorded.
- A VM-backed run's film is rendered natively with `--recorded-backend
  consonance`. It compares the recorded semantic endpoint, because a whole-VM
  run's snapshot digest differs from a native one by construction. `film.json`
  sets `endpoint_bridged` and leaves `evidence_verified` false. Nothing is
  captured inside the guest, and no report claims otherwise.
- `--max-frames` bounds the render. Frames past the ceiling are emulated and
  left out of the video, so the film is a trailing window ending at the recorded
  endpoint plus `--tail-frames`. The ceiling, the clip policy and the dropped
  frame count are in `film.json` under `clip` and in the published report.
- `scripts/verify-nes-films.py` checks every film, whether a matrix produced it
  or a workload wrote it directly. It checks the digest, frame count, audio
  stream, mean volume, duration floor, and that the decoded audio covers the
  video rather than a padded fragment of it. Against a `films.json` index it
  also checks each film against the digest, input and identity that index
  recorded for the cell, and against the input the cell's own run recorded, so
  one cell's film cannot stand in for another's. A silent track fails.
- The verifier writes its verdict per cell to `film-verification.json`, and the
  published roster and report count a film as media only when that record
  accepts it.

A scenario that produced no renderable input is recorded as unavailable with a
reason and appears in the report that way. A failed render fails its job. No run
reports a success film it did not produce.

Public exports carry the asset licence notices for the artifacts they contain
and exclude ROMs and emulator binaries.

## Historical bugs

`Benchmarks / Harmony Workloads / Historical Bugs` searches the current build of
each affected workload for the bug its case describes, through Consonance. Each
scenario job is named after the bug.

There is no fixed-version comparison. A case records which upstream versions the
bug affects and which fixed it as provenance; it never declares an execution
arm, a control version or a replay mode over a second build.
`ci_contract.FORBIDDEN_HISTORICAL_KEYS` lists the keys a case may not carry and
`ci-historical-arms` rejects them, along with any matrix dimension that would
restore the arm. A separate Historical Bugs Checks workflow does not exist; the
full search lives in Benchmarks and pull requests do not run it.

## Naming

- Title Case, with the canonical spellings in `ci_contract.CANONICAL_TERMS`
  (`macOS`, `etcd`, `K3s`, `QuickNES`, `PostgreSQL`, `NES`, `OCI`, `API`,
  `Arm64` and the rest) preserved exactly.
- `ci_contract.SMALL_WORDS` stay lowercase unless they open or close a name.
- A display name identifies a responsibility or a workload scenario. It never
  names a trigger, and generic names such as `Unit Tests`, `Build` or `Verify`
  are rejected. Contextual names such as `Nova`, `Coverage` and `Results` are
  kept.
- A job name never repeats the workflow hierarchy; the workflow name already
  carries the owner.
- A matrix variant is a ` — <Variant>` suffix with an explicit label:
  `Nova — Replica <N>`, `Mutation Testing — Shard <N>/16`. A bare `(1)` is
  rejected, and so is a job whose matrix would display one name twice.
- Matrix values and machine identifiers pass through unchanged. The linter
  checks static text and the values a statically declared matrix supplies; it
  does not guess the capitalization of data a runner produces.

## Extending the registry

1. Add or change the `Workflow` and `Job` entries in `scripts/ci_contract.py`:
   the path, the qualified name, the owner, each job's display name, trigger
   class, budget, any exception, its `ci-scope` kind, the ignored tests it
   runs and any media it must capture.
2. Write the workflow file so its name, triggers and job display names match.
3. Run `python3 scripts/custom-lints.py`. Every difference between the file and
   the registry is reported with the rule that owns it.

A new NES scenario also needs its case in `benchmarks/search/nightly.json` and a
matrix entry in the owning benchmark workflow; `ci-nes-case-jobs` requires the
two to match exactly.

## Where each rule is enforced

`scripts/custom-lints.py` checks what a parser can decide. It parses every
workflow with PyYAML 6.0.3 and fails closed on a missing parser, invalid YAML,
duplicate mapping keys, a malformed trigger or an unregistered file. Findings
under a `ci-` rule cannot be recorded in the lint baseline.

| Rule | Covers |
| --- | --- |
| `ci-workflow-registration` | Every workflow file is registered, once, under one name and path. |
| `ci-workflow-parse` | Parsing fails closed. |
| `ci-workflow-name` | Category, owner and retired categories. |
| `ci-workflow-triggers` | Declared triggers match the registry. |
| `ci-display-name` | Title Case, responsibility, variant suffixes, uniqueness inside a workflow. |
| `ci-pr-job-timeout` | Declared budgets, and the 15-minute bound on pull request work. |
| `ci-trigger-routing` | A `pr` job runs on pull requests, a `full` job proves it does not, and no pull request job starts a full search. |
| `ci-trigger-exception` | Mixed trigger classes carry a registered reason. |
| `ci-scope-routing` | One selector per job, complete checkout, guarded steps. |
| `ci-ignored-tests` | A job that runs ignored tests registers them, and its steps name each one it registers. |
| `ci-analysis-grouping` | Coverage, Miri, mutation and proofs sit in the owning component's Analysis workflow. |
| `ci-host-compatibility` | Each supported host keeps a bounded job. |
| `ci-nes-compositions` | Both NES compositions keep a check and a benchmark and run their registered backend. |
| `ci-nes-media` | Both compositions film their scenarios and check the media, in steps the registered job always runs. |
| `ci-nes-case-jobs` | The public case roster maps one-to-one onto independent jobs. |
| `ci-miri-coverage` | Each Analysis workflow lists exactly the Miri targets it owns. |
| `ci-historical-arms` | No case or matrix restores a fixed-version comparison arm. |

`scripts/semantic-lints.py` asks a judge what a parser cannot decide. It skips
without `TYPESAFE_API_KEY`, so deterministic correctness never depends on it. A
workflow is judged alongside its registry entry, the ownership policy, the local
actions, scripts and manifests it runs, the renderers its jobs register as
media, and `docs/WORKFLOWS.md`. A composite action is followed into its own
file, so a script the workflow reaches only through an action counts too. The
prompt carries a bounded excerpt of each of those files and the digest of the
whole file, so a change anywhere in one reselects the workflow and invalidates
its cached judgment. Findings under these rules are fixed rather than recorded
in the semantic baseline.

| Rule | Asks |
| --- | --- |
| `ci-owner-mismatch` | Does the work belong to the owner the name claims? |
| `ci-job-name-meaning` | Do job names describe a method instead of a responsibility? |
| `ci-disguised-search` | Is a full capability search presented as a bounded check? |
| `ci-duplicate-suite` | Do two suites assert the same thing over the same inputs at the same budget? |
| `ci-media-disconnected` | Is claimed video evidence produced from the run's own recorded input? |
| `ci-fixed-version-direction` | Does documentation direct a fixed-version comparison campaign? |
| `ci-boundary-contradiction` | Does documentation contradict the component and composition boundaries? |

## Verification

```sh
python3 -m unittest discover -s scripts -p 'test_custom_lints.py'
python3 -m unittest discover -s scripts -p 'test_semantic_lints.py'
python3 -m unittest discover -s scripts -p 'test_ci_*.py'
python3 -m unittest discover -s scripts -p 'test_*scope.py'
python3 -m unittest discover -s benchmarks/search -p 'test_*.py'
python3 scripts/custom-lints.py
python3 scripts/check-test-partition.py ignored
node --test scripts/benchmark-report.test.cjs
```

These checks read declared structure. They do not measure the cost of an
arbitrary script, prove a change selector correct, or see GitHub's retained
registry of branch-only workflows. Inspect `gh workflow list --all` before
disabling an obsolete registry entry; disabling one preserves its old runs and
is separate from repository lint.
