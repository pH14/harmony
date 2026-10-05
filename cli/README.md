# Harmony CLI

`harmony` prepares workloads, searches their behavior, and investigates saved
executions. Workload packages own their inputs, actions, observations and
interventions. Runners own execution and runtime artifacts. The CLI owns recipes,
search budgets, named runs and command dispatch.

## Start an application

```sh
harmony init
harmony search --name baseline --for 10m
harmony runs
harmony findings baseline
harmony inspect baseline --finding 1
harmony timeline baseline --finding 1
harmony logs baseline --finding 1 --contains ERROR
harmony replay baseline --finding 1 --repeat 3 --name confirmed
```

`init` infers a language from recognized project files when unambiguous. Use
`--language c|rust|go|python|java` when needed. It never overwrites a recipe.
`init IMAGE` sets the application image name. `search` and `run` prepare configured
builds automatically. `prepare` builds and validates without executing; `doctor`
checks runner availability, provisions runtime artifacts and reports admission.
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

[[workload.options.interventions.verbose]]
kind = "hook"
name = "debug"
for = "1s"

[[workload.options.interventions.delayed]]
kind = "pause"
node = "database"
for = "100ms"

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

Consonance artifact overrides are `kernel`, `base_initramfs` and `uml_profile`
inside `runner.options`. `doctor` provisions checksummed versioned artifacts.
Development artifacts are discovered under `consonance/harmony-linux/build/ARCH`,
and installed artifacts under `share/harmony/guest/ARCH`. The cache uses
`HARMONY_DATA_DIR`, otherwise `XDG_DATA_HOME/harmony`, macOS
`~/Library/Application Support/Harmony`, or Linux `~/.local/share/harmony`.
New release assets become available when their release is published.

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
flags. The current NES package recognizes SMB and Nova. It supports search,
recorded-input execution, replay, branching, rooted search and continuation.
NES controller recordings use `--actions FILE`; console logs and virtual-time
rewinds are not available for these recordings.

## Execution and investigation

```sh
harmony run my-app:local -- /bin/program argument
harmony run --actions input.json --repeat 2
harmony branch baseline --finding 1 --rewind 10 --do verbose --name debug
harmony branch baseline --finding 1 --step 120 --do delayed --name delayed
harmony branch baseline --finding 1 --rewind-time 2s --stop --name earlier
harmony search --from earlier --executions 2000 --name neighborhood
harmony search --from baseline --finding 1 --rewind 10 --name neighborhood-2
harmony resume baseline --executions 5000 --name extended
harmony resume extended --for 10m --name longer
harmony diff confirmed debug
```

`--step N` selects the boundary after N recorded actions; step 0 is the prepared
initial state. `--rewind N` moves back N steps from the selected finding's first
observed failure, or the execution endpoint. Recovery actions are included when
the failure was observed during recovery. `--rewind-time` uses virtual time and
rounds down to an available boundary. Branch output states the resolved step and
time. The same selectors work with `timeline`, `logs` and `search --from`.

A branch requires an explicit point. It reconstructs the prefix, applies
interventions in order, and executes the recorded suffix. `--stop` omits the
suffix and automatic recovery, leaving a prefix for another search. Branching is
a new experiment: interventions can change timing and outcomes. Exact replay
uses the unchanged saved inputs and artifacts.

`--do NAME` selects a named intervention from the saved recipe. The faults package
accepts `kill`, `pause`, `restart`, `wait` and `hook` actions, with an explicit
`for` duration. Node operations take `node`; hooks take `name`. Hook durations are
execution windows, not promises of completion: timeline counters show observed
hook starts and completions. Windows must fit whole 10ms ticks. The same typed
plan is available inline:

```sh
harmony branch baseline --finding 1 --rewind 10 \
  --intervention-toml 'actions = [{kind = "hook", name = "debug", for = "1s"}]'
```

`--actions FILE` supplies package-specific recorded actions, including targeted
fault event parks and kills. Logging changes require an application control hook.
Rebuilding with another logging configuration creates a different execution
identity and is not exact replay.

`search --from` starts fresh search history at the selected prefix. `resume`
restores the existing corpus, snapshots, scheduler and evidence from the latest
retained whole-search checkpoint. Its budgets are additional: `--executions 5000`
permits 5,000 more executions from that checkpoint, while `--for 10m` grants a new
10-minute wall window. With neither, it adds 1,000 executions. A time-only resume
removes the previous execution ceiling. Checkpoints are written periodically and
at completion; an interrupted run can retain an earlier checkpoint. NES retains
its checkpoint journal and origin input under the run's `package` directory. A new seed
intentionally changes the draw sequence.

Run names resolve beneath `.harmony/runs`; saved-run commands also accept a run
directory. `--out DIRECTORY` creates a fresh output directory. Existing runs are
never overwritten. `runs`, `findings`, `inspect` and `timeline` support `--json`.
Console evidence retains a bounded 64 KiB tail; adjacent observations can overlap.

## Storage and boundaries

The v2 manifest contains the resolved workload and runner identities, artifact
hashes, parent run and a package-owned payload. Shared run storage never decodes
fault actions or controller inputs. Package adapters own payload schemas and
operation capabilities. Runtime identity checks belong to runners. Saved replay
never repeats automatic runner selection or resolves a mutable image tag.

Replay verifies the exact CLI executable, architecture and saved artifact hashes;
UML also pins host identity. Application replay compares final state, assertions,
actions and recovery evidence. Plain UML replay compares application output and
bridge events while retaining raw boot logs as diagnostics. Hardware command
replay also compares the full serial digest. NES replay compares its typed witness
and snapshot digest. Removing a run directory removes its saved evidence.

Exit 0 means successful operation, including verified reproduction of a finding.
Application search exits 1 for findings or unmet reachability assertions; a failed
application command also exits 1. Exit 2 denotes invalid input, infrastructure
failure or replay divergence. Search budgets and seeds must fit TOML's signed
64-bit integer range.

## Verification

Run the CLI tests, package tests, Clippy and registered CI checks. The test-only
counter package exercises preparation, search, replay and branching through the
same dispatch and storage interfaces without application or NES types.

- `bash cli/tests/investigation.sh DIRECTORY` exercises preparation, logging hooks,
  exact replay, prefix branching, additional-budget continuation and tamper refusal.
- `bash cli/tests/uml-command.sh IMAGE PROFILE INITRAMFS DIRECTORY` exercises plain
  command replay as an unprivileged UML user and rejects planted output divergence.
- `bash cli/tests/nes.sh ROM CORE DIRECTORY` exercises native NES search, replay,
  prefix branching, rooted search, continuation and tamper refusal.
