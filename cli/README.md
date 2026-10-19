# Harmony CLI

`harmony` prepares workloads, searches their behavior, and investigates saved
executions. Workload packages own their inputs, actions, observations and
debugging capabilities. Runners own execution and runtime artifacts. The CLI owns recipes,
search budgets, named searches and branches and command dispatch.

## Start an application

```sh
harmony init
harmony prepare
harmony check
harmony search --name baseline --for 10m
harmony list
harmony show baseline
harmony show baseline --finding 1 --timeline
harmony show baseline --finding 1 --logs --contains ERROR
harmony branch baseline --finding 1 --rewind 10 --shell --name debugging
harmony search --from debugging --name neighborhood
```

`init` infers a language from recognized project files when unambiguous. Use
`--language c|rust|go|python|java` when needed. It never overwrites a recipe.
`init IMAGE` sets the application image name. `search` prepares configured builds
automatically. `prepare` builds and validates without executing; `check` checks
runner availability, provisions runtime artifacts and reports admission.
Neither command infers application readiness or correctness.

An OCI image selects the faults workload. A `.nes` input selects the NES workload.
Use `--package` to override inference. This is convenience dispatch owned by the
registered packages; the shared configuration does not contain ROM or image fields.

Commands read `harmony.toml` when no positional input is supplied. `--config PATH`
selects a file; `--config-toml TEXT` accepts the same TOML inline. Explicit flags
win over configuration. File paths are relative to the configuration directory;
inline paths are relative to the current directory. Unknown fields are errors.

## Recipe ownership

```toml
[workload]
package = "faults"
input = "my-app:local"

[workload.options.build]
language = "rust"
context = "."

[workload.options.nodes.database]
command = ["/opt/harmony/application", "server"]

[workload.options.nodes.worker]
command = ["/opt/harmony/application", "worker"]

[workload.options.hooks]
debug = ["/app/control", "log-level", "debug"]
partition = ["/app/control", "partition"]

[runner]
kind = "consonance"
backend = "auto"

[runner.options]
ram_mib = 1024

[search]
seed = 0
executions = 1000
wall_seconds = 600
```

The faults package also accepts `setup`, `ready`, `workload`, `check`, `command`
and `knobs` in `workload.options`. Lifecycle commands and node/hook commands are
argument arrays; an explicit shell is required for shell syntax. Nodes, lifecycle
commands and hooks compile into a supervisor bundle. Omit them to use the image's
bundle. Services share the image filesystem. See the
[supervisor contract](../consonance/harmony-linux/supervisor/README.md).

`check` and SDK assertions define correctness. `Sometimes` assertions measure
reachability. Admission validates executable files and instrumentation attestations;
it does not prove complete instrumentation. Event faults require the corresponding
instrumentation and symbols.

## Language preparation

| Language | Default input | Preparation |
|---|---|---|
| C | `main.c` | Clang callbacks and SDK forwarding object |
| Rust | one Cargo package with `src/main.rs` | pinned Rust, SDK dependency, coverage passes |
| Go | one main package with `go.mod` | SDK and patched compiler wrapper |
| Python | `main.py` and sources | instrumented CPython, source coverage catalog, precompiled bytecode |
| Java | prebuilt `app.jar` | instrumented HotSpot with automatic loop callbacks |

These are starter recipes. Rust workspaces, native Python extensions and additional
Java modules may need `workload.options.build.dockerfile`. C/Rust/Go can instead use
`build.command`, an argument array producing `/out/application` inside the builder.
With no language or Dockerfile, it runs on the host to produce the configured image.
Preparation preserves symbols and attestations and composes the current runtime.
It copies sources without editing them, honors `.dockerignore`, and excludes `.git`
and `.harmony`. Docker or Podman is required.

Installed releases fetch their pinned source SDK; development checkouts use local
recipes, or `HARMONY_SDK_DIR`. First builds can be substantial. `--offline` prevents
Harmony release downloads, not container-builder network access. See the
[language recipes](../workloads/languages/README.md) for callback coverage and limits.

## Runners and artifacts

Consonance chooses KVM, then HVF, then UML according to host availability.
`--runner` selects an execution implementation; `--backend` constrains its backend.
UML runs on Linux without hardware virtualization, including ordinary containers.
An explicit UML profile selects UML when the backend is automatic.
Preparation applies an explicit UML backend or profile's policy even when the
current host cannot launch UML.
UML admission rejects scanned hardware entropy instructions even if a VM
review lists them. `prepare`, `check`, `search` and plain `debug run` apply the
selected backend's policy. A saved UML run from before this policy cannot be
replayed or branched because its guest image was not admitted under this rule.

Consonance artifact overrides are `kernel`, `base_initramfs` and `uml_profile`
inside `runner.options`. `check` provisions checksummed versioned artifacts.
Development artifacts are discovered under `consonance/harmony-linux/build/ARCH`,
and installed artifacts under `share/harmony/guest/ARCH`. The cache uses
`HARMONY_DATA_DIR`, otherwise `XDG_DATA_HOME/harmony`, macOS
`~/Library/Application Support/Harmony`, or Linux `~/.local/share/harmony`.
New release assets become available when their release is published.
Automatic discovery selects the first directory containing a current-source
`runtime-manifest.json`, the architecture's kernel, and
`initramfs-oci.cpio.gz`, with both file hashes matching the manifest. It skips
stale, incomplete, or changed local bundles and applies the same check to
cached and downloaded bundles before use. UML also checks an automatically
selected base initramfs this way. An older cached release without a manifest
must be removed and fetched again. For KVM and HVF, a single artifact override
takes its counterpart from the same resolved directory and requires the same
manifest check. Supplying both paths explicitly permits a custom layout, for
which the caller must provide a qualified kernel and base initramfs pair.
Selected artifact bytes are checked again when loaded, so a concurrent rebuild
cannot replace a qualified kernel or base before execution. An explicit UML
base initramfs is likewise the caller's choice. Capturing
artifact hashes for replay identity does not qualify an older kernel for the
current boot command line. The platform source key includes the CLI sources
that supply and assemble the default x86 and Arm kernel command lines.
Workload command lines belong to each workload's execution and cache identity.

NES defaults to the QuickNES runner:

```toml
[workload]
package = "nes"
input = "game.nes"

[runner]
kind = "quicknes"

[runner.options]
core = "quicknes_libretro.so"
```

For guest execution, use `runner.kind = "consonance"` and set
`workload.options.guest_image` to the NES guest OCI image. This adapter currently
supports Linux KVM. These requirements belong to the adapter, not shared CLI
flags. The current NES package recognizes SMB. It supports search,
recorded-input execution, replay, branching, rooted search and continuation.
NES controller recordings use `--actions FILE`; console logs and virtual-time
rewinds are not available for these recordings.

## Search and investigation

```text
harmony
├── init [INPUT]                         create harmony.toml
├── prepare [INPUT]                      build and instrument
├── check [INPUT]                        check and provision the runner
├── search [INPUT]                       explore
│   ├── --resume SEARCH                  continue an existing search
│   └── --from BRANCH                    explore from a saved branch
├── list                                 saved searches and branches
├── show NAME                            summary and findings
│   ├── --finding N                      select a finding
│   ├── --logs [--contains TEXT]          application and terminal output
│   └── --timeline                       recorded actions and observations
├── branch NAME                          save a new experiment
│   ├── --finding N                      start from a finding
│   ├── --step N | --rewind N | --rewind-time 2s
│   ├── --exec 'COMMAND'                  execute inside the guest
│   ├── --exec-file LOCAL.sh              copy and execute a local script
│   ├── --shell                           interactive guest terminal
│   └── --stop                            save without the original suffix
└── diff NAME NAME                       compare configuration and outcomes
```

```sh
harmony branch baseline --finding 1 --rewind 10 \
  --exec '/app/control log-level debug' --name verbose
harmony branch baseline --finding 1 --rewind-time 2s \
  --exec-file ./investigate.sh --stop --name earlier
harmony search --from earlier --executions 2000 --name neighborhood
harmony search --resume baseline --executions 5000 --name extended
harmony diff verbose earlier
```

A **search** explores many possible executions and collects findings. A **branch**
is a saved experiment at a selected point in one execution. `show` summarizes
either; `--finding N` narrows it to one finding. `diff` compares resolved
configuration, saved artifact identities, runner identity and package outcomes.
It does not compare two directories of arbitrary files.

`--step N` selects the boundary after N recorded actions; step 0 is the prepared
initial state. `--rewind N` moves back N steps from the finding's first observed
failure, or the branch endpoint. Recovery actions are included when the failure
was observed during recovery. `--rewind-time` uses virtual time and rounds down
to an available boundary. The same selectors filter `show --timeline` and
`show --logs`. Branch output states the resolved step. With no point selector,
branching selects the finding or endpoint. A search without a selected finding
can be branched at `--step 0`.

`--exec` is shell text interpreted by `/bin/sh` **inside the guest**. Its paths
refer to guest files. `--exec-file` reads a file on the **host**, saves its bytes
with the branch and executes them in the guest. The image must provide `/bin/sh`.
Both use the workload's working directory, environment and credentials. Commands
can inspect or mutate files, processes and application controls; they are not
read-only probes. A branch reconstructs the selected prefix, runs the command,
and then executes the original suffix. `--stop` omits that suffix and recovery.
The package-specific `--actions FILE` escape hatch adds typed fault or controller
inputs before the remaining suffix.

`--shell` opens a guest PTY and implies `--stop`. Exit the shell to save the
actual guest snapshot, including filesystem and process changes. A subsequent
`search --from` restores that snapshot and explores from it. It does not rerun
shell commands. Terminal output and submitted bytes are retained as evidence.
Interactive timing is part of the new experiment. Faults branches with commands
require the updated supervisor runtime; use `check` to provision matching assets.
NES exposes typed controller actions and has no guest shell capability.

`search --from` starts fresh exploration from a branch endpoint. `search --resume`
restores the existing corpus, snapshots, scheduler and evidence from the latest
retained whole-search checkpoint. Its budgets are additional: `--executions 5000`
permits 5,000 new executions, in addition to finishing any work already queued at that checkpoint, while `--for 10m` grants a new
10-minute wall window. With neither, it adds 1,000 executions. A time-only resume
removes the previous execution ceiling. Checkpoints are written periodically and
at completion; an interrupted search can retain an earlier checkpoint. NES retains
its checkpoint journal and origin input under `package/`. A new seed intentionally
changes the draw sequence. The journal identifies the latest retained checkpoint
and binds its bytes with SHA-256. Resume refuses a missing or changed latest
checkpoint or an unjournaled retained checkpoint, including after an
interruption. Journal execution counts must be monotonic and agree with each
generated filename and the selected checkpoint header, so reversing two retained
records cannot select an older continuation. The additional execution budget
uses the verified header's reserved count, so a stale journal count cannot
change the continuation budget.
An interrupted write can leave an unjournaled newer checkpoint; resume refuses
that ambiguous state even if an older checkpoint remains. NES records the
package's canonical controller origin, so accepted extra JSON fields in an
action file do not prevent a later resume.
Each search records a fresh owner in its initial manifest and checkpoint
headers. Resume refuses a checkpoint from another saved search, including after
an interrupted run. A completed search also records the selected checkpoint's
SHA-256 in its manifest and refuses changed checkpoint bytes, even when they
remain valid for the same workload. The selected journal digest is carried to
the searcher and compared with the checkpoint bytes it actually opens, closing
the gap between selection and resume. Both NES and faults resume compare the
selected checkpoint with the manifest already verified at command start; a
replacement manifest cannot authorize a later checkpoint swap.

Names resolve beneath `.harmony/runs`; commands also accept a saved directory.
`--name NAME` chooses a name; `--out DIRECTORY` chooses an explicit fresh directory.
Existing results are never overwritten. `list` and `show` support `--json`.
Console evidence retains a bounded 64 KiB tail; adjacent observations can overlap.
Terminal output is saved separately and included by `show --logs` without a point
selector. Point-filtered logs show the console captured at that boundary.

The `nested` package preserves the nested VM search supported by the runtime.
Select it explicitly with an OCI driver input, `runner.kind = "consonance"` and
both `runner.options.kernel` pointing to a kernel built with nested KVM support
and `runner.options.base_initramfs` pointing to its matching OCI base. This
noncanonical profile pair is caller-qualified and must be supplied together.
It requires Linux x86-64 with nested KVM enabled. Search and finding summaries
are supported; branching, shell, continuation and console timelines are not yet
implemented by this package. Replay requires the saved campaign stream to match
the search report's stream digest and runs from the verified bytes. Its kernel
and initramfs are checked against the source recording when finally loaded,
before inheritance can copy a later version of either file. Each replay saves
the same verified stream bytes, so a completed replay can itself be replayed.
Failed or incomplete saved campaigns cannot become replay sources, even if a
producer wrote a report before the failure. The saved seed, execution budget and
wall limit must agree with the producer report before replay begins.

## Independent campaigns on two hosts

Run a complete local search on each host with a different seed to explore more
histories concurrently. Each search owns its coordinator, workers and cache;
this does not preserve one globally ordered campaign or exchange search
archives. This recipe is checked against the CLI source, not an executed two-
host qualification or a measured speedup.

Use the same installed release pair (`harmony` and `harmony-consonance-host`),
architecture and backend on both hosts; pin the backend in `CONFIG` rather than
using `auto`. Start with already admitted, identical workload and runtime
artifact bytes, setup and configuration; use a saved OCI file and omit build
directives to avoid rebuilding independently. `--offline` prevents Harmony
release downloads, not configured builds or their container-builder network
access. The normal strict-host and runtime admission checks still apply. Replay
requires the exact recorded host executable and runner identity. CPU provenance
warnings are advisory: matching metadata does not prove numerical or whole-
state portability, and different metadata can explain replay divergence.

On each host, choose a distinct `SEED`, an execution ceiling `EXECUTIONS`, a
wall budget `WALL` (such as a duration ending in `s` or `m`), and a fresh `RUN`
directory. Set `CONFIG` to the prepared configuration file. Choose budgets and
available cores and memory before starting; the CLI derives workers and cache
capacity from local resources, so different hosts can explore different work.
Run these existing commands separately on each host:

```sh
harmony check --config "${CONFIG:?}" --offline --json || exit $?
harmony search --config "${CONFIG:?}" --offline --seed "${SEED:?}" \
  --executions "${EXECUTIONS:?}" --for "${WALL:?}" --out "${RUN:?}"
harmony show "${RUN:?}" --json
```

Retain stdout, stderr and each command's exit status. A faults search report
returns `0` without confirmed bugs, unmet reachability assertions, execution
failures or watchdog cutoffs; unconfirmed hits can still appear. `1` means a
confirmed bug or unmet reachability assertion. Failures or watchdog cutoffs
take precedence with `2`; CLI, admission and setup errors also exit `2`,
sometimes without a run directory. Inspect the report and manifest as well as
the exit status: a completed manifest can accompany any of these codes. `show`
displays saved evidence; it does not verify artifacts or perform a fresh
replay. A search wall budget is not a hard process-cleanup deadline. Use the
host's process supervisor to track the campaign and its children, and confirm
their exit before reusing resources after completion or interruption. The CLI
does not supply a process-tree cleanup receipt; uncertain shutdown must not be
treated as confirmed cleanup.

Only a complete recording can serve as a replay source. After the run has
stopped and its files are stable, copy the **whole run directory** to a
compatible replay host, retaining the manifest, report, artifacts, streams and
checkpoints. Create a checksum list of all regular run files on the producer
with host tools such as `sha256sum`, retain it outside the run, and check it
after transfer; do not rewrite manifests or copy only `first-bug-input.json`.
Keep each host's directory distinct. For a producer-confirmed finding, set
`COPIED_RUN`, its one-based `FINDING`, a positive repeat count `REPEATS` and a
fresh `REPLAY_OUT`, then use:

```sh
harmony debug replay "${COPIED_RUN:?}" --finding "${FINDING:?}" \
  --repeat "${REPEATS:?}" --out "${REPLAY_OUT:?}"
```

Replay verifies the saved recording and runs fresh executions. Keep refusals,
divergence and CPU warnings with the result; do not weaken identity checks to
make another host accept it. Search confirmation reproduces bug evidence; exact
replay is a separate status. UML additionally binds its admitted physical
processor; its unavailable full-state identity cannot certify exact
reproduction. Replay has no wall-budget flag, so plan its resource and cleanup
limits separately.

Report per-host seeds, artifacts, CPU/backend identity, actual worker and cache
sizes, completed executions, errors, watchdogs and confirmed finding records.
Keep exact replay outcomes separate. Summing finding records does not count
unique bugs: independent runs can rediscover the same input or cause. Compare
useful confirmed outcomes and observed elapsed time across a seed panel before
claiming a scaling benefit. Cooperating campaigns and globally ordered remote
shards require additional protocols; these commands implement neither.

## Developer qualification

Hidden `harmony debug run` executes a fixed action input or an OCI command;
`harmony debug replay NAME [--finding N] [--repeat N]` checks exact reproduction.
For supervised runs it compares the final state and outcome as well as every
recorded action boundary, including the action, observation, settlement marker,
and bounded console evidence. The repeat ordinal and `guest_horizons` cache
diagnostic are not replay invariants.

Finding replay refuses an unconfirmed saved finding, because replay did not
establish its reported failure. Branching can still inspect that evidence.
Faults replay also refuses a failed or incomplete recording. A saved supervised
run with multiple replays must have matching outcomes before one can serve as
the reference for a fresh replay.
These are developer qualification tools. Ordinary investigation uses a branch,
and further exploration uses `search`.

## Build the paired binaries from source

A source build for Consonance needs both executables in one directory. Release
archives already contain the pair. On Linux, install the host's musl linker
(for example, `musl-tools` on Ubuntu), then build the native frontend and the
static host from the repository root. Choose the musl target for the host
architecture:

```sh
musl_target=x86_64-unknown-linux-musl  # use aarch64-unknown-linux-musl on Arm64
rustup target add "$musl_target"
cargo build --locked --release -p harmony-cli --bin harmony
cargo build --locked --release -p harmony-cli --target "$musl_target" \
  --no-default-features --features consonance-host --bin harmony-consonance-host
cp "target/$musl_target/release/harmony-consonance-host" target/release/
./target/release/harmony check
```

On macOS, build the second executable for the native target and sign it with
the same hardened runtime, Hypervisor entitlement and OS-only library constraint
used by the release package:

```sh
cargo build --locked --release -p harmony-cli --bin harmony
cargo build --locked --release -p harmony-cli --no-default-features \
  --features consonance-host --bin harmony-consonance-host
codesign --force --sign - --identifier org.harmony.consonance-host \
  --options runtime,hard,kill \
  --entitlements consonance/vmm-backend/hvf.entitlements.plist \
  --library-constraint scripts/macos-os-only-library-constraint.coderequirement \
  target/release/harmony-consonance-host
./target/release/harmony check
```

These commands leave the native QuickNES frontend beside its strict Consonance
host. A Consonance command refuses an absent or unqualified companion.

## Storage and boundaries

The Linux and macOS CLI installations pair `harmony` with
`harmony-consonance-host` in the same `bin` directory. `harmony` owns workload
builds, language and runtime downloads, OCI staging, native QuickNES and saved-run
browsing. Search still prepares its input automatically. Consonance execution
receives a captured prepared request after this work completes. The guarded host
contains the coordinator and its same-image session worker, including fault,
NES Consonance, nested KVM and reviewed UML execution. Its dedicated entrypoint
accepts prepared execution, readiness probes and the internal session-worker
role; it does not parse the frontend command tree or construct its package
method table. The shared application library exposes separate frontend and guarded entrypoints;
NES saved execution selects its native or guarded backend through static generic
dispatch, so the guarded entrypoint has no native QuickNES callback. Role
selection uses independent bin entrypoints. The default `frontend` capability
enables command parsing, package preparation/browsing, runtime acquisition and
native dispatch; the `harmony` bin requires it. The isolated host build uses
`--no-default-features --features consonance-host`, omitting those modules and
the direct clap/tar/compression dependencies used for recipe/download handling.
OCI archive parsing and compression remain required transitively for independent
prepared-image admission. Both default and `--all-features` builds preserve
native frontend behavior; enabling `consonance-host` does not change its role.
The Linux x86-64 and Arm64 CI jobs lint the isolated guarded role with warnings
as errors; nested KVM execution remains limited to Linux x86-64, while other
hosts retain its explicit unsupported-execution refusal. On Linux, it copies
the static-musl host into a fresh executable memfd, seals its complete contents against writes and resizing, then executes it. The host checks those
seals, the full loaded ELF bytes (including writable initializers), and its
executable mappings before it parses a command. On macOS, the paired host is a
hardened signed executable with the OS-only library constraint and Hypervisor
entitlement. It checks the kernel-loaded code hash, exact signing policy and
loaded system images before command dispatch. A missing or invalid host stops
Consonance execution and readiness checks. `harmony prepare` performs its build
outside the guarded host. The frontend checks companion admission before implicit
preparation, then unlinks the captured request file and execs the host with its
held read-only descriptor. The execution keeps the command's original PID,
process group and terminal, including for guest shells. The captured backing
remains held for the host's lifetime; no request pathname survives the handoff.

The `harmony-prepared-execution-v1` handoff is a self-contained postcard file
with JSON request metadata and a trailing SHA-256. Its total size is at most
1 GiB, metadata at most 1 MiB, and a captured UML profile at most 4,096 ordinary
files. Actions and host scripts are captured in that file as well. The host checks
the checksum before decoding, requires exactly one fully consumed postcard record,
rejects another release/platform-source/protocol,
validates the operation, pinned backend, options and runtime qualification, and
reconstructs canonical OCI control bytes from the prepared record. It independently
admits the actual archived application image and supervisor vocabulary. Faults
supervision must agree with the requested search, captured actions and configured
nodes; a plain execution cannot discard requested actions or replace supervision.
Execution-bearing fields are accepted only by operations that consume them:
actions by Faults/NES runs and branches, guest commands by Faults branches,
console output by plain Faults runs, and additional budgets by saved searches.
Raw command overrides must agree with the effective Faults run configuration.
Saved replay, branch and resume require a selection; incompatible point selectors,
commands, captures and repeat counts are refused before execution.
Prepared rootfs inflation is bounded to 1 GiB. Source recipe/image changes after capture
cannot replace those bytes. Captured UML files are independently loaded through
the reviewed-profile validator. The temporary profile and captured inputs remain
owned until execution finishes. Saved execution reloads and verifies its original
manifest and artifact identities in the host; a prepared request cannot supply
replacement artifacts for replay, branching or resume. Saved Consonance recordings
retain this strict host identity. Native
QuickNES recordings retain their native loaded-image identity.
Linux hosts must permit executable memfds; `vm.memfd_noexec=2` prevents
Consonance execution and produces an explicit error.

The Linux release check exercises the same packaged frontend, sealed host and
guest runtime archives that publication uses. The bounded fixture emits a
reviewed readback, replays a completed command, and runs one public search with
a traced production worker child. This checks those local public paths; it
does not establish whole-state or history equivalence, restore portability,
macOS composition or cross-host behavior. The release retains the finite matrix
and exact artifact digests beside the executable and runtime assets.

The v4 manifest contains the loaded Harmony executable identity, resolved
workload and runner identities, artifact hashes, parent recording and a
package-owned payload. Shared recording storage never decodes
fault actions or controller inputs. Package adapters own payload schemas and
operation capabilities. Runtime identity checks belong to runners. Saved replay
never repeats automatic runner selection or resolves a mutable image tag.

Saved runs also retain `cpu_provenance`, an advisory history of observed CPU
implementation metadata. Replay, branching, and continued search print a warning
to stderr if the recording and current CPU differ or either identity is unknown,
incomplete, or heterogeneous. Older manifests without this field warn as unknown;
derived runs retain that uncertainty. Standard math libraries remain supported,
and the warning does not block execution. Native CPU behavior can change result
bits or saved state, including x87 instruction-pointer metadata. Matching
metadata is not proof of numerical or whole-state equivalence. These warnings
belong to CLI dispatch; lower-level APIs
do not automatically emit them. Snapshot, cache, and runner identities are
unchanged, and existing incompatibility errors still apply.

Replay verifies the recorded host executable identity, architecture and saved artifact hashes.
Consonance recording and replay require the strict host at the manifest boundary,
including when package code is called without the frontend. Linux worker reexec
uses the sealed image through `/proc/self/exe`, so a mutable pathname cannot
replace the worker executable. If the host cannot provide its strict identity,
recording and replay fail closed.
Faults replay, branching and search from a branch also compare the saved
`report.json` or `run.json` outcome with its manifest copy before using that
evidence. Replay and branch selection use that verified manifest copy, even
if a source outcome file changes later. Search resume restores its
checkpoint and does not consume the report;
an interrupted report write does not invalidate an earlier checkpoint.
Faults `show`, `--timeline` and report-backed `--logs` display the manifest's
saved outcome, so later edits to an outcome file cannot change the displayed
recording. Command runs with a saved serial digest check it before displaying
`--logs`; a completed command without one refuses display. Other raw console
and terminal logs remain diagnostic output. A supervised recording always uses
its saved action-boundary console; a stray `serial.log` cannot replace it.
After a faults package returns, completion compares the saved report with the
in-memory result before recording its outcome. A replaced or missing report
cannot become a completed search, replay, or branch result. Command replay
and standalone command runs likewise bind the record built by the execution
producer before completion.
Derived runs also verify the bytes copied from each source artifact against its
recorded hash, so a replacement between source verification and inheritance is
rejected. Faults recordings capture the kernel and initramfs actually prepared for
execution, and completed reports must name those saved digests. Saved faults
execution checks the kernel, initramfs and search vocabulary at their final
read against the manifest; rooted search passes the verified root bytes to its
workers. Fault search and plain command execution admit executable bytes from
the same packed OCI rootfs segment that becomes the guest initramfs. UML search
and command execution also check the loaded profile's
identity against the verified recording before guest launch. The saved UML
kernel digest must match that profile's executable before a run can complete.
Nested replay and NES guest replay likewise compare their saved kernel and
initramfs bytes with the recording at the final read before execution.
UML also pins the exact physical processor identity selected by the fastest
core pool's coordinator placement; the manifest and faults campaign use the
same processor.
Standalone command runs and replays bind that saved identity before launching
the guest. KVM and HVF application replay compare final state,
assertions, actions and recovery evidence. UML application replay records
assertions, actions and recovery evidence but cannot establish full-state
identity. Plain UML command replay compares application output and
bridge events while retaining raw boot logs as diagnostics. Hardware command
replay also compares the full serial digest. NES replay compares its typed witness
and snapshot digest; failed recordings and disagreeing saved repeats cannot
certify exact replay. NES search binds the saved witness input and result to the
bytes returned by the package, then saves a finding only when the search result
and its fresh replay witness agree on victory. Removing a saved directory
removes its saved evidence.

Exit 0 means successful operation, including verified reproduction of a finding.
Application search exits 1 for findings or unmet reachability assertions; a failed
application command also exits 1. UML application replay records observed
assertions and stops, but exits 2 because a canonical full-state digest is not
available to verify exact reproduction. Exit 2 also denotes invalid input, infrastructure
failure or replay divergence. Search budgets and seeds must fit TOML's signed
64-bit integer range.

## Verification

Run the CLI tests, package tests, Clippy and registered CI checks. The test-only
counter package exercises preparation, search, replay and branching through the
same dispatch and storage interfaces without application or NES types.

- `bash cli/tests/investigation.sh DIRECTORY` exercises preparation, guest commands,
  local scripts, interactive shells, snapshot restoration, continuation and tamper refusal.
- `bash cli/tests/uml-command.sh IMAGE PROFILE INITRAMFS DIRECTORY [REJECTED_IMAGE]`
  exercises plain command replay as an unprivileged UML user, rejects planted
  output divergence, and optionally verifies that a reviewed hardware-entropy
  image is refused before launch.

Fresh prepared NES searches persist the canonical genesis input before launching
the package. Resume verifies that captured artifact against the package origin
input before admitting its checkpoint; it never reconstructs an omitted origin.

The reviewed x86 UML native transaction fixture also imports VM-accepted prepared
records through the host handoff, covering both plain and supervised executions.
XBEGIN/XACQUIRE refusals and a harmless control retain independent admission after
frontend capture; the existing native control still launches the reviewed UML
kernel. CI requires every frontend, prepared-boundary and native control witness.
