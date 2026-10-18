# Continuous integration contract

`scripts/ci_contract.py` is the registry: every workflow, its owner, its jobs,
their trigger classes and their budgets. `scripts/custom-lints.py` checks the
checked-in workflows against it, `scripts/semantic-lints.py` asks a judge the
questions a parser cannot answer, and this document explains what the registry
means. A new workflow is registered once, in the registry.

## Organization

A workflow name reads `Category / Owner` or `Category / Owner / Workload`.

- **Category**: `Checks`, `Benchmarks`, `Release` or `Maintenance`.
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
| `Checks / Consonance / Native Math Qualification` | `consonance-native-math-qualification.yml` | schedule, workflow_dispatch |
| `Checks / Consonance / Guest Runtime Qualification` | `consonance-runtime-qualification.yml` | schedule, workflow_dispatch |
| `Checks / Consonance / Nested Host Qualification` | `consonance-nested-host-qualification.yml` | push, schedule, workflow_dispatch |
| `Checks / Consonance / Kernel XSAVE Qualification` | `consonance-kernel-xsave-qualification.yml` | workflow_dispatch |
| `Checks / Consonance / UML` | `consonance-uml.yml` | pull_request, push, workflow_dispatch |
| `Checks / Dissonance` | `dissonance-checks.yml` | pull_request, push |
| `Checks / Dissonance / Analysis` | `dissonance-analysis.yml` | schedule, workflow_dispatch |
| `Checks / Harmony` | `harmony-checks.yml` | pull_request, push |
| `Checks / Harmony / Analysis` | `harmony-analysis.yml` | pull_request, push, schedule, workflow_dispatch |
| `Checks / Dissonance Workloads / NES` | `dissonance-workloads-nes-checks.yml` | pull_request, push |
| `Checks / Dissonance Workloads / Tiny Worlds` | `dissonance-workloads-tiny-worlds-checks.yml` | pull_request, push |
| `Checks / Harmony Workloads / Languages` | `harmony-workloads-languages-checks.yml` | pull_request, push, schedule, workflow_dispatch |
| `Checks / Harmony Workloads / OCI` | `harmony-workloads-oci-checks.yml` | pull_request, push, schedule, workflow_dispatch |
| `Benchmarks / Dissonance Workloads / NES` | `dissonance-workloads-nes-benchmarks.yml` | schedule, workflow_dispatch |
| `Benchmarks / Harmony Workloads / Historical Bugs` | `harmony-workloads-historical-bugs.yml` | schedule, workflow_dispatch |
| `Benchmarks / Harmony Workloads / UML` | `harmony-workloads-uml-campaign.yml` | workflow_dispatch |
| `Release / Harmony` | `release.yml` | push (version tags) |
| `Maintenance / Repository` | `repository-maintenance.yml` | schedule, workflow_dispatch |

The release CLI archive contains both `harmony` and `harmony-consonance-host`.
Linux builds the Consonance host as a static musl executable; the frontend
copies and seals it before execution. macOS signs the separate host with the
Hypervisor entitlement, hardened runtime, and an operating-system-only library
constraint. Each release build tests the paired launch before it packages the
two executables together, so the frontend can hand Consonance commands to the
admitted host.
`Checks / Harmony` builds both Linux executables on x86-64 and Arm64 in
separate 15-minute pull-request jobs. Both package jobs prove that a
disk-backed host is refused, the paired frontend enters the sealed host, and
native QuickNES remains on the frontend. The package fixture uses the Faults
runner on both Linux architectures. A separate x86-64 musl job runs the public
Session, worker reexec and failure recovery, UML shutdown, timeout, branch,
handler, and root-import commitment tests, executable-map rejection and cache
revocation, Recording, and terminal console limits through
`scripts/linux-sealed-test-runner.py`, which copies each test binary into a
fully sealed executable memfd before execution. All 16 exact tests require
one test to start and pass. The musl positive tests fail if strict host
admission is unavailable, so a missing runner cannot produce a green skip.
`Checks / Harmony Host Compatibility` has a separate macOS-14 package job
that builds the frontend and Consonance host, signs the host with the release
entitlement and operating-system-only library constraint, then checks signed
host admission, paired frontend routing, native QuickNES routing, and CLI
readiness. It runs on pull requests so a broken macOS package is caught before
release. The Arm64 Linux package job likewise checks readiness with the musl
host beside its frontend.

`Checks / Harmony Host Compatibility` selects all three Linux Arm64 jobs through
`rust_checks`. `Linux Arm64` compiles the backend and runs the portable
Consonance and Harmony tests; `Linux Arm64 Dissonance` runs the portable
Dissonance tests; `Linux Arm64 Qualification` runs the SHA-256 checks.
Each suite enforces its own portable-test floor. Splitting the Dissonance suite
keeps the full Arm test coverage within separate 15-minute pull-request jobs.
The jobs use separate Rust build caches so each can save its own build artifacts
on main. The macOS Arm64 job continues to run both portable suites and the
qualification checks in one job; the signed-package check has its own cache.

`Checks / Dissonance / Analysis` ships coverage only. The searcher has no
mutation baseline, and adding one is separate work.

A manual dispatch of `Checks / Consonance / Analysis` or `Checks / Harmony /
Analysis` runs every scheduled job by default. The `job` input narrows it to
`miri-whole-crate`, `coverage` or `mutants`; `crate` picks one whole-crate Miri
target and `shard` picks one mutation shard. Unselected matrix rows skip their
steps and finish in seconds, so verifying one failing job holds one runner:

```bash
gh workflow run consonance-analysis.yml --ref <branch> -f job=miri-whole-crate -f crate=vmm-core
```

`Checks / Consonance / UML` owns the User-mode Linux profiles for x86-64 and
arm64. Artifact and qualification checks run on both architectures. `UML Launcher`
lints and tests `uml` and `host-code-identity` on x86-64, then runs
`scripts/test-loaded-code-identity.py` on Linux. That public regression builds
ordinary `$ORIGIN` linked library A/B variants, a constructor-cleared preload,
an anonymous executable mapping, and read-only library variants. Each must be
refused by the strict public Session identity before it can record or replay a
guest. The witness needs no guest profile. `UML Artifacts —
<Architecture>` builds the profile twice from the locked Nix toolchain and
fails unless every artifact is byte-identical and both profile manifests
match the source-reviewed architecture-specific tuple embedded in the
launcher. The release, language and UML campaign profile jobs check that
same tuple before packaging or uploading. The artifact jobs also load the
approved bundle and refuse self-consistent kernel, config and fixture edits
through the production verifier. They bind the CLI recording to the reviewed
kernel and run the guest panic, reserved exit-status and host-clone failure
regressions; x86-64 also checks native FS and GS bases across restores. A
failed profile comparison retains both generated configs and profiles for
diagnosis. Two cold builds have a
registered 45-minute exception. The arm64 profile builds from the pinned RFC
port.

The root Nextest configuration starts Linux test binaries through
`scripts/linux-clean-loader-test.sh`, which removes Cargo's code-selecting
loader variables before the test begins. The Fault Injection, NES Adapter,
and Nested Host Qualification jobs use that runner for their Linux Cargo test
steps. Container Execution, extended guest runtime qualification, and release
runtime qualification instead build static-musl OCI tests and launch them
through `scripts/linux-sealed-test-runner.py`; their public Session calls
require a fully sealed executable host. These test runners do not change
production identity admission or Miri execution.

`UML Qualification — <Target>` runs three suites of the static-musl
`harmony-uml-qualify` in sequence as the runner's ordinary UID. The qualifier
copies itself into a fully sealed executable memfd before using the public
Recording and Session APIs, both natively and under Docker's default seccomp
profile with every capability dropped. Its checkpoint suite injects a host
one-page unprotection failure after reaching a normal guest event and requires
infrastructure abandonment without a checkpoint or additional guest events. A failing suite does not stop the
others, and the job fails if any suite fails. The qualifier denies ptrace and
KVM ioctls to itself and every guest, and fails when the host is root or has
effective capabilities.

- `launch` runs repeated boots, a hanging guest, the console limit, child
  cleanup, a refused seccomp filter and guest init exits.
- `replay` runs the replay suite: 100 replays of each fixture under host load,
  CPU pinning, process stops and host address randomization must produce one
  event hash, and recordings must replay to each cut from a fresh process.
- `checkpoint` runs the restore suite: in-place and fresh-process restores of
  each fixture must reach the cold event hash, and an image without host memory
  or a restore without the bridge state must diverge.

Reports remain workflow artifacts.

## Dissonance Workloads

**`Dissonance Workloads / NES`** drives native QuickNES through the Dissonance
adapter. It exercises the search loop, the archive and the game's own
interpretation without a virtual machine.

`scripts/ci_contract.py` registers the composition in `NES_COMPOSITIONS` with
the backend it runs, and the linter requires it to keep a bounded Checks
workflow and a full Benchmarks workflow, and requires each workflow to actually
run its registered backend.

`Checks / Consonance` and `Checks / Harmony Workloads / OCI` each start with an
`Exact Runtime Artifacts` job. It runs the shared `exact-platform-runtime`
action, which restores the guest runtime built from this source or builds it,
then saves it to the cache and publishes it as an artifact. Every job that
restores the runtime with `platform-runtime` and requires an exact match needs
that job, so it never starts before the runtime exists. Guest Runtime
Qualification uses the same action and adds the extended platform replay. The
runtime handoff ignores artifacts from fork repositories, except those published
earlier in the same run.

`Checks / Dissonance Workloads / Tiny Worlds` builds the standalone workload and
runs mechanics, archive-retention, replay, work-accounting, formatting, and Clippy
checks with one build worker and one test thread.

## Bounded checks and full benchmarks

Every job declares a trigger class.

- **`pr`** jobs run on pull requests and on pushes to main, and finish inside 15
  minutes. This bound holds for `pull_request`, `pull_request_target` and
  `merge_group`. Artifact-build prerequisites are the exceptions, each capped
  at 45 minutes for a cold build: `UML Artifacts — <Architecture>` builds the UML kernels,
  `Exact Runtime Artifacts` builds the exact guest runtime,
  `Language Guest Runtime` builds the guest for the language checks,
  `Language UML Profile` builds the pinned UML profile for the language checks,
  `PostgreSQL CLI Package` and `Language CLI Tools` build the paired native
  frontend and static Consonance host once for their consumers, and
  `Language Image — <Language>` builds each language workload image. Their
  consumers keep the 15-minute budget. The separate `C Investigation` job has
  a 35-minute exception for its bounded branch and search continuation under
  sealed-host identity checks.
  The selected `vmm-backend` Miri checks have a 30-minute exception for
  interpreted snapshot and register-inventory coverage; every other Miri
  matrix target keeps 15 minutes. The registry binds the exact timeout
  expression, so the extended budget cannot silently cover every variant.
  `PR_ARTIFACT_BUILD_BUDGETS` and `PR_EXTENDED_CHECK_BUDGETS` register these
  named exceptions; neither permits a full capability search on pull requests.
- **`full`** jobs run on a schedule, a manual dispatch, or a path-filtered push
  to main, and declare their own ceiling.

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
| `Checks / Harmony Workloads / OCI` | `Docker` | 90 | Building the pinned Docker workload image and booting it twice under nested KVM exceeds the pull request budget. |
| `Checks / Harmony Workloads / OCI` | `K3s` | 90 | Building the pinned K3s workload image and bringing a cluster up twice exceeds the pull request budget. |

A trigger-class exception does not let a `pr` job run longer. Artifact build
budgets are registered separately and apply only to the named prerequisite.
`Language Guest Runtime` builds the exact guest runtime when no cached copy
exists. Each `Language Image — <Language>` job builds one language layer from
source when its cache misses, then composes the current shared runtime onto it.
`Language CLI Tools` builds and verifies the native frontend and static host
once for all language and UML command consumers; the 15-minute language smokes
and separate C investigation restore the pair. The scoped
`PostgreSQL CLI Package` does the same before the 15-minute PostgreSQL search.
These artifact jobs each have a 45-minute cold build budget. A language layer's
cache key is the hash of its own recipe
inputs. Weekly runs and `rebuild_images` dispatches rebuild every layer.

Scheduled OCI, historical-bug, UML campaign, and nested-host workflows also
build or restore both executables together. A nested historical rebuild uses
the archived source for both binaries; an archive predating the strict host
fails explicitly rather than replaying with a different or dynamic host.

## Change selection

`.github/actions/ci-scope` is the single selector. A workflow whose jobs are
wholly decided by the changed files has one `Change Selection` job with the id
`scope`. It checks out the complete history and runs the selector once per
kind, under an id equal to the kind, and publishes each result as a job output
of the same name. Each selected job needs `scope` and starts only when its
output is true, joined to any other condition by `&&`, so an unselected job is
skipped and never starts a runner. The `miri_matrix` kind publishes the Miri
targets a change reaches for one component as the matrix of the Miri job;
`scripts/miri_scope.py` is its only list.

The `rust_checks` kind selects a workflow's Rust build and test jobs. Its
target is the file name of the workflow. A change selects the jobs unless every
changed path is one of these:

- Documentation: Markdown, `docs/`, `LICENSE`, `.agents/`, `.claude/`,
  `.githooks/` and `benchmarks/`.
- A tool only the Repository workflow runs, listed in
  `scripts/rust_scope.py`. `scripts/test_rust_scope.py` fails when another
  workflow or action starts naming one.
- The file of another workflow.

A path that selects any scenario in `scripts/ci_scope.py` always selects, and so
does any path the selector does not recognize. A scheduled or manual run
selects the whole tracked tree, and so does a run whose diff cannot be read, as
after a force push to main.

A job that always runs part of its work and selects only the rest, such as the
hardware steps of `CPU State`, runs the selector itself once, unconditionally,
under the id `scope`, after a complete-history checkout, and guards the
selected steps with `steps.scope.outputs.enabled` joined by `&&`.
`ci_contract.SCOPE_KINDS` records which workflow owns each kind. A scheduled or
manual run of the Languages and NES workflows selects the whole tracked tree.

On a push to main, change selection and `Semantic Lints` read only what that
push changed, so every push runs to completion. A workflow a push reaches keys
its concurrency group by `ci_contract.PUSH_CONCURRENCY_KEY`, which gives each
push a group of its own and keeps pull requests and scheduled runs grouped by
ref. In a group that pushes share, a later push cancels a running push run and
replaces a pending one, and the commits of the dropped push are never selected
or judged on main.

A job that selects its own steps and finds none finishes successfully with a
summary saying it was not applicable. A selection or diff error fails closed. A selected test that fails
still fails its job and still uploads the evidence it produced. A required
hardware test is never downgraded to a successful skip: missing artifacts fail
the job.

## Caches

GitHub keeps 10 GiB of caches per repository and evicts the least recently used
entry past that. A pull request cannot read another pull request's caches, so
every Rust build cache it saved was dead weight for everyone else. Each
`Swatinem/rust-cache` step saves only on `main` and restores on pull requests
from the entry `main` saved. The language layer caches save on any ref,
because a missed layer costs a build of up to sixteen minutes and the entry is
small.

## Ignored tests

The Nested Host Qualification workflow's `Nested Host` job runs on its
schedule, on manual dispatch, and on pushes to main that change anything under
`consonance/`, the workspace manifests, the toolchain pin, or a crate that
`vmm-core`, `nested-driver` or `harmony-cli` depend on by path.
`test_nested_host_qualification_runs_when_main_changes_its_inputs` recomputes
that dependency closure from the Cargo manifests. The job caches the nested-host
kernel, fixture and OCI runtime under the guest runtime source key and the
hash of the workflow file and toolchain pin, so a push that changes only Rust crates skips the kernel
build and installs only the packages the fixture rebuild needs. Test binaries
compile on every CPU before each test runs pinned to one, and `harmony-cli`
builds in the background on the other CPUs while those tests run.
Scheduled and push runs accept whichever nested vendor the runner
provides; a dispatch can require VMX or SVM. The job first
requires `KVM_CAP_NESTED_STATE` and KVM-supported VMX or SVM with NPT on its selected Ubuntu
x86 runner, then builds the separate
nested-host kernel in the pinned Debian GCC 14 build container and boots L1
under the named nested-host contract. The
`Nested Host` job runs `x86_kvm_nested_host::l1_creates_kvm_vm`, builds the
static inner driver and matching OCI runtime, then runs
`nested-driver::live::inner_consonance_runs_inner_guest` and
`nested-driver::nested_restore::outer_nested_state_snapshot_matrix`. The latter
runs `cold_snapshot_child` as a capture process that is killed while holding
its live VM, then imports its artifact in a new process. It compares eight
detour restores and cold continuation with uninterrupted and capture-only
execution. Its sparse detour capture and every restored RAM page must match
the live source or captured cut, respectively, with no in-place fallback.
A separate `harmony_omit_nested_state` compiler configuration must pass those two controls
and fail its first restore. VMX fails its continuation; SVM fails immediate
GIF readback before another nested entry can change it. Missing nested
VMX/SVM or a guest that cannot create a KVM VM fails the job. Kernel publication
still requires the instruction audit to pass.
The direct fixture also runs
`vmm-core::vendor::x86::contract::nested::tests::nested_restore_preserves_unsynchronized_vmcs_fields`.
It stops a minimal L2 runner after three exits, runs a detour through exit five
without reading L2 segment state, and compares the complete nested payload
immediately after direct outer restore. L2 counts its steps in DS, and the
restored fixture must reach exit twelve, so a host that keeps L2 segment state
cached from the detour fails the check. This exercises a cached VMCS boundary
that the production inner VMM's segment-state reads would otherwise synchronize.
It also runs
`vmm-core::vendor::x86::contract::nested::tests::interrupt_raised_before_nested_entry_reaches_the_nested_host`.
That test raises vector 0xFF whenever L1 reads DEBUGCTL, which Linux does with
interrupts disabled just before it enters L2. L1 must report each delivered
vector as a spurious interrupt, and the cache fixture must finish all twelve L2
exits.
`vmm-core::vendor::x86::contract::nested::tests::restore_before_nested_operation_onto_a_nested_host`
restores a cut taken during early boot onto a vCPU that has since enabled VMX
or SVM, compares the restored registers and nested payload, and runs the
restored guest to the fixture's last exit.
If a compiled kernel fails qualification, the evidence artifact retains its
unpublished `vmlinux`, matching boot components, configuration, alternatives and
KVM disassembly for review.
On Intel VMX, the job also runs `inner_operation_api_smoke` and
`sdk_operations::outer_operation_sdk_smoke`, then runs the standard
`harmony search --package nested` with 100 executions and a five-minute wall
budget, followed by a replay of its recorded campaign and a replay of that
replayed run. Intel evidence includes the 36 ordered
operation-pair coverage mask, assertion failures and their layers, the
campaign stream, and both replay reports. The final evidence check requires
nonzero executed work, a valid 36-bit pair mask, no failures, and matching
execution counts, work, stream digest and coverage/failure evidence across
search and both replays.
It writes `qualification.json` with the tested commit and measured budget use;
archive-entry lists may differ because replay materializes final artifacts.
The nested-host job accepts Intel VMX or AMD SVM with NPT, selected from
KVM-supported CPUID. On AMD SVM it runs the L1/L2 boot, nested-state restore
matrix, and omission negative control, then records that SDK operation search
requires Intel VMX. AMD restore of an entered vCPU fails closed and uses a fresh
VM where a factory is available. The matching kernel includes both backends. The state
format and exposed vendor capabilities bind snapshot identity; snapshots cannot
cross vendors. SVM restores also compare GIF immediately at the lifecycle cut.
`nested_history_run` optionally reuses a prior run's retained kernel, base and
OCI image, rebuilds that run's source, and generates a new campaign with its own
recorded replay. The historical arm must reproduce the original failure's layer,
assertion and action lineage before the current-source qualification proceeds.
It retains both binaries' input provenance, the historical source, host KVM
parameters, kernel logs and nested-entry trace events. This is a rebuild control,
not a replay claim about the original unretained outer binary.
Current qualification reuses that kernel and OCI base, builds its own inner
driver image, and rebuilds the direct cache fixture from current source.
Manual runs can require `nested_vendor=vmx` or `nested_vendor=svm`; the default
`auto` accepts either. `nested_runner` selects the standard x86 Ubuntu 22.04
or 24.04 image; its label does not guarantee a CPU vendor. A vendor mismatch
fails before compilation. The early
`nested-host-preflight-RUN` artifact retains the CPU and KVM evidence while
the build is still running, so a requested vendor is observable immediately.
After runner setup and compilation, the job records KVM device permissions,
refreshes access, and repeats the capability probe before booting L1. Its
result must match the early probe.

A test marked `#[ignore]` needs something a plain `cargo test` lacks, such as a
hypervisor or a built guest image. Each one has a runner in
`scripts/ci_contract.py`, named as `<binary-id> <test>` the way
`cargo nextest list` prints it, with `*` for a whole binary:

- `Job.ignored_tests` lists what a CI job runs with `--ignored`.
  `ci-ignored-tests` checks that the job's steps pass `--ignored` and name each
  binary and test it lists.
- `HOST_TESTS` lists tests that need a hypervisor no hosted runner offers, keyed
  by the `<os>-<arch>` of a machine that has one. The `linux-x86_64-hybrid` key
  additionally requires accessible P-core and E-core pools in the caller's
  affinity. The pre-push hook runs the entry for the machine it is on, selected
  by `python3 scripts/ci_contract.py host-filter`.

`Checks / Repository` runs `python3 scripts/check-test-partition.py ignored` on
x86-64 Linux, arm64 Linux and arm64 macOS. It lists every ignored test that
builds on that host and fails on one with no runner. A test that no job or
machine runs is deleted.

## Cross-host execution qualification

The separate `Native Math Qualification` workflow builds one static baseline
x86-64 executable and runs three pinned repetitions on each of six hosts. Its
comparator requires AMD/Intel diversity and retains exact input/result/status
bits for every differing pair, plus matching subsets by operation and floating
point mode. Successful evidence collection may include numerical differences;
missing diversity, instability, or artifact mismatch fails. The bounded scope,
compiler/library provenance, and exclusions are documented beside the
[corpus](../scripts/qualification/native-math/README.md). This does not alter
standard-library admission or advisory CPU replay warnings.

The hardware workflow builds `x86_kvm_virtual_time` once as a static musl test
executable and distributes the exact retained bytes to both X1 replicas. Each
replica runs the registered repeatability, placement and negative-control tests
and retains their logs, executable identity, source/contract digests and host
identity. Results requires every hardware job to succeed, checks every required
receipt against its retained logs, and compares X1 state/event digests across
the two hosts. A missing replica or a contradiction fails the result.
`scripts/qualification/consonance.json` defines the bounded claim; it excludes
whole-VMM identity and cross-host snapshot migration. Independently rebuilt
Linux and snapshot suites retain their narrower existing scopes.

## Audiovisual evidence

The NES composition publishes video with game audio: a bounded capture in the
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

A case declares no seed. The run supplies one and the report records it, so a
case's search budget is what has to reach the bug rather than a committed seed
landing where it once landed. `ci-pinned-seed-outcome` rejects a literal seed
value in an expected-output pattern and a case manifest that commits a seed.

There is no fixed-version comparison. A case records which upstream versions the
bug affects and which fixed it as provenance; it never declares an execution
arm, a control version or a replay mode over a second build.
`ci_contract.FORBIDDEN_HISTORICAL_KEYS` lists the keys a case may not carry and
`ci-historical-arms` rejects them, along with any matrix dimension that would
restore the arm. A separate Historical Bugs Checks workflow does not exist; the
full search lives in Benchmarks and pull requests do not run it.
Once a historical search writes `report.json`, selector errors and missing
selected evidence still write an infrastructure-failure panel record without a
finding or reproducer.

`Benchmarks / Harmony Workloads / UML` runs one historical case, etcd by
default, on the User-mode Linux profile when an operator dispatches it.
`scripts/historical-search.sh` and `scripts/historical-replay.sh` take
`BACKEND=uml`, which runs the CLI through `harmony-uml-qualify exec`: the
runner's ordinary UID with ptrace and KVM ioctls denied, with the credentials
and denial recorded beside the report. The search fails when the campaign
captured no snapshot or restored none. The same job then selects the first
confirmed finding whose saved replay also carried the case's assertion and
evidence and applied every action, then replays it from genesis in fresh
processes before uploading the run. Both panel records name the selected
finding; the search panel points to its `bug-N.json` evidence.
This preserves the recorded UML host identity and executable artifact modes.
The UML search wall is capped at 280 minutes within a 360-minute job, reserving
20 minutes for search shutdown, 50 for finding replay and 10 for setup and uploads.
Every replay must violate the case's assertion with its evidence. KVM replays
must also reach one state digest. UML has no canonical full-state hash yet:
its exact-replay command records the observations and exits 2, and this
workflow recognizes that specific refusal only when every reported hash is
explicitly unavailable. The oracle then checks observed evidence, reports
`state identity unavailable`, and does not claim whole-state equivalence.

The language workflow also builds a pinned UML profile for `UML Command Replay`.
That bounded check rejects the reviewed hardware-entropy sites in the Bookworm
C image, then runs a small image without those sites and fresh replays as an
ordinary user. It compares application and bridge evidence and rejects a
planted divergence. Current Bookworm historical case images have the same
reviewed sites, so the manually dispatched UML campaign refuses them at image
admission until they are rebuilt without those instructions.

## Repository maintenance

`Maintenance / Repository` runs `scripts/prune-branches.py` every day. It
deletes a remote branch when no open pull request uses it and either a merged
pull request's head is exactly the branch tip or the tip is reachable from
`main`. It keeps every other branch and lists it in the job summary with the
reason: closed without merging, commits after the merged head, no pull
request, or GitHub could not list the branch's pull requests. The script lists
branches first and then asks for their pull requests in batches, splitting a
batch GitHub cannot execute until the failing branch is isolated. A manual dispatch lists the deletions and performs them only with the
`apply` input.

## Naming

- Title Case, with the canonical spellings in `ci_contract.CANONICAL_TERMS`
  (`macOS`, `etcd`, `K3s`, `QuickNES`, `PostgreSQL`, `NES`, `OCI`, `API`,
  `Arm64` and the rest) preserved exactly.
- `ci_contract.SMALL_WORDS` stay lowercase unless they open or close a name.
- A display name identifies a responsibility or a workload scenario. It never
  names a trigger, and generic names such as `Unit Tests`, `Build` or `Verify`
  are rejected. Contextual names such as `STB`, `Coverage` and `Results` are
  kept.
- A job name never repeats the workflow hierarchy; the workflow name already
  carries the owner.
- A matrix variant is a ` — <Variant>` suffix with an explicit label:
  `UML Artifacts — <Architecture>`, `Mutation Testing — Shard <N>/16`. A bare `(1)` is
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
duplicate mapping keys, a malformed trigger or an unregistered file.

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
| `ci-scope-routing` | One selector per workflow or job, complete checkout, selected work requires its selection. |
| `ci-push-concurrency` | Each push to main has its own concurrency group. |
| `ci-ignored-tests` | A job that runs ignored tests registers them, and its steps name each one it registers. |
| `ci-analysis-grouping` | Coverage, Miri, mutation and proofs sit in the owning component's Analysis workflow. |
| `ci-host-compatibility` | Each supported host keeps a bounded job. |
| `ci-nes-compositions` | Both NES compositions keep a check and a benchmark and run their registered backend. |
| `ci-nes-media` | Both compositions film their scenarios and check the media, in steps the registered job always runs. |
| `ci-nes-case-jobs` | The public case roster maps one-to-one onto independent jobs. |
| `ci-miri-coverage` | Each Analysis workflow lists exactly the Miri targets it owns, in its matrices and its `crate` dispatch input. |
| `ci-historical-arms` | No case or matrix restores a fixed-version comparison arm. |
| `ci-pinned-seed-outcome` | No expected-output pattern pins a literal seed value. |

`scripts/semantic-lints.py` asks a judge what a parser cannot decide. Its
judgments skip without `TYPESAFE_API_KEY`, so deterministic correctness never
depends on them. A workflow is judged alongside its registry entry, the
ownership policy, the local actions, scripts and manifests it runs, the
renderers its jobs register as media, and `docs/WORKFLOWS.md`. A composite
action is followed into its own file, so a script the workflow reaches only
through an action counts too. The prompt carries a bounded excerpt of each of
those files and the digest of the whole file, so a change anywhere in one
reselects the workflow and invalidates its cached judgment.
Guest kernel patches, launchers, and nearby build inputs are also judged
for runtime switches that contradict the dedicated Harmony guest contract.
The judge sees the final numbered patch in each kernel series when checking
an earlier patch.
The standalone-program rule applies to Cargo binary entrypoints at
`src/bin/NAME.rs` or `src/bin/NAME/main.rs`. Rust helper modules remain subject
to the other content rules and are assessed as part of their binary's code.

The subject file is judged in overlapping character ranges covering its entire
content. A service token-budget rejection splits that range into smaller,
overlapping ranges while retaining the same questions and composed context.
Any range's finding applies to the file; a clean range cannot cancel it. The
cache stores the complete set of judgments and their coverage. A range that
still cannot be judged, a malformed answer, or another service error fails the
check. Findings identify their character range. Raw answer dumps contain one
row per range and question, with `start_character` and `end_character` columns.
Offsets are zero-based characters and the end is exclusive.

| Rule | Asks |
| --- | --- |
| `ci-owner-mismatch` | Does the work belong to the owner the name claims? |
| `ci-job-name-meaning` | Do job names describe a method instead of a responsibility? |
| `ci-disguised-search` | Is a full capability search presented as a bounded check? |
| `ci-duplicate-suite` | Do two suites assert the same thing over the same inputs at the same budget? |
| `ci-media-disconnected` | Is claimed video evidence produced from the run's own recorded input? |
| `ci-fixed-release-run` | Does a historical case, its documentation or CI build, run or compare a version other than the affected one? |
| `ci-boundary-contradiction` | Does documentation contradict the component and composition boundaries? |
| `ci-pinned-seed-outcome` | Does a check require a particular search outcome from one fixed seed? |
| `guest-runtime-opt-in` | Does a guest kernel patch, launcher, or adjacent build path make required Harmony guest behavior optional to run the shipped image outside Harmony? Build-only test controls remain valid. |

The `Semantic Lints` job in `Checks / Repository` judges a pull request against
the tip of its base branch. It judges a push to main against the commit before
the push, so every commit in the push is covered. When that commit is missing
from the checkout, as after a force push, the job fetches it from GitHub by SHA
and fails if GitHub no longer has it. When that commit is all zeros, as when
the branch is new, the job judges every tracked file, and that run can exceed
the 15-minute limit.

The job keeps its judgments in `.semantic-lints-cache.json` and restores the
newest copy from the Actions cache before it judges. A judgment is keyed by the
file's path and content, the model, the questions and the composed context, so
a later push to a pull request judges only the files whose content or context
changed. The job saves the file under a key derived from its own hash, so a run
that changed nothing adds no cache entry. A pull request run reads the caches
of its own pull request and of its base branch, and main cannot read a pull
request's cache. A push to main therefore judges the merged diff once, then
reuses that cache for later pushes. A change
to `docs/WORKFLOWS.md` or the registry changes the context of every workflow
judgment and judges all workflows again. The file keeps at most
`CACHE_ENTRY_LIMIT` judgments and drops the oldest first.

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

The Guest Memory job includes the nested driver’s portable arithmetic and
mapping checks. Its mapping composition runs in the bounded Consonance Miri
matrix; the full matrix interprets the driver library.
The host-code-identity Miri target exercises the loader callback's unsafe
program-header copy with a synthetic record on pull requests that change that
crate. Its whole library runs in scheduled Consonance analysis.

The release workflow publishes the CLI, a guest kernel and base initramfs with
their source-bound runtime manifest,
a pinned UML profile, and the source SDK for language preparation. Guest, UML
and SDK archives carry SHA-256 sidecars. `harmony check` and `harmony prepare`
fetch assets for the CLI version and host architecture into the user data
cache; development builds can supply explicit runtime paths and `HARMONY_SDK_DIR`.
