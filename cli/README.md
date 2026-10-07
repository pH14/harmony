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

Consonance artifact overrides are `kernel`, `base_initramfs` and `uml_profile`
inside `runner.options`. `check` provisions checksummed versioned artifacts.
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
changes the draw sequence.

Names resolve beneath `.harmony/runs`; commands also accept a saved directory.
`--name NAME` chooses a name; `--out DIRECTORY` chooses an explicit fresh directory.
Existing results are never overwritten. `list` and `show` support `--json`.
Console evidence retains a bounded 64 KiB tail; adjacent observations can overlap.
Terminal output is saved separately and included by `show --logs` without a point
selector. Point-filtered logs show the console captured at that boundary.

The `nested` package preserves the nested VM search supported by the runtime.
Select it explicitly with an OCI driver input, `runner.kind = "consonance"` and
`runner.options.kernel` pointing to a kernel built with nested KVM support.
It requires Linux x86-64 with nested KVM enabled. Search and finding summaries
are supported; branching, shell, continuation and console timelines are not yet
implemented by this package.

## Developer qualification

Hidden `harmony debug run` executes a fixed action input or an OCI command;
`harmony debug replay NAME [--finding N] [--repeat N]` checks exact reproduction.
These are developer qualification tools. Ordinary investigation uses a branch,
and further exploration uses `search`.

## Storage and boundaries

The v2 manifest contains the resolved workload and runner identities, artifact
hashes, parent recording and a package-owned payload. Shared recording storage never decodes
fault actions or controller inputs. Package adapters own payload schemas and
operation capabilities. Runtime identity checks belong to runners. Saved replay
never repeats automatic runner selection or resolves a mutable image tag.

Replay verifies the exact CLI executable, architecture and saved artifact hashes;
UML also pins host identity. Application replay compares final state, assertions,
actions and recovery evidence. Plain UML replay compares application output and
bridge events while retaining raw boot logs as diagnostics. Hardware command
replay also compares the full serial digest. NES replay compares its typed witness
and snapshot digest. Removing a saved directory removes its saved evidence.

Exit 0 means successful operation, including verified reproduction of a finding.
Application search exits 1 for findings or unmet reachability assertions; a failed
application command also exits 1. Exit 2 denotes invalid input, infrastructure
failure or replay divergence. Search budgets and seeds must fit TOML's signed
64-bit integer range.

## Verification

Run the CLI tests, package tests, Clippy and registered CI checks. The test-only
counter package exercises preparation, search, replay and branching through the
same dispatch and storage interfaces without application or NES types.

- `bash cli/tests/investigation.sh DIRECTORY` exercises preparation, guest commands,
  local scripts, interactive shells, snapshot restoration, continuation and tamper refusal.
- `bash cli/tests/uml-command.sh IMAGE PROFILE INITRAMFS DIRECTORY` exercises plain
  command replay as an unprivileged UML user and rejects planted output divergence.
- `bash cli/tests/nes.sh ROM CORE DIRECTORY` exercises native NES search, replay,
  prefix branching, rooted search, continuation and tamper refusal.
