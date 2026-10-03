# Harmony CLI

`harmony` prepares applications, searches their behavior, and investigates saved
executions. An OCI image is an application input; a `.nes` file is a game input.
The CLI owns these conveniences and configuration. The execution engine and
searcher remain independent of OCI and TOML.

## Start an application

```sh
harmony init --language c
harmony prepare
harmony doctor
harmony search --name baseline --for 10m
harmony inspect baseline
harmony inspect baseline --bug 1
harmony timeline baseline --bug 1
harmony logs baseline --bug 1 --contains ERROR
harmony replay baseline --bug 1 --repeat 3 --name confirmed
```

`init` creates `harmony.toml` without overwriting a file. For an existing image
with a Harmony supervisor bundle, `harmony search my-app:local` is sufficient.
`prepare IMAGE` performs image admission without requiring guest artifacts or a
hypervisor. `run IMAGE -- /bin/program ARG` runs a plain command and saves its
serial log and exit record. `run` with configured nodes runs one supervised
scenario; `run --actions input.json --repeat 2` runs an explicit recorded action
sequence. Search discovers sequences itself.

Commands read `harmony.toml` when no positional input is supplied. Use `--config
PATH` to select another file, or `--config-toml 'image = "my-app:local"'` for inline
TOML. CLI flags override the selected configuration. Paths in a file are relative
to that file; inline paths and flag paths are relative to the working directory.
Unknown TOML fields are errors. Durations on `--for` accept `s`, `m`, or `h`.

## Application configuration

```toml
image = "my-app:local"
backend = "auto"
seed = 0
executions = 1000
wall_seconds = 600
ram_mib = 1024
knobs = ["app.scenario=contention"]
setup = ["/app/setup"]
ready = ["/app/ready"]
workload = ["/app/client"]
check = ["/app/check"]

[build]
language = "rust"
context = "."

[nodes.database]
command = ["/opt/harmony/application", "server"]

[nodes.worker]
command = ["/opt/harmony/application", "worker"]

[hooks]
debug = ["/app/control", "log-level", "debug"]
partition = ["/app/control", "partition"]
```

Commands are argument arrays. Use an explicit shell for shell syntax. Node and
hook names use letters, digits, underscores and hyphens. Names are sorted for
stable node indices and hook IDs. Nodes, setup, readiness, workload, checks and
hooks are compiled into a supervisor bundle and installed into the assembled
guest image. Omit these fields to use the image's own bundle. Services share the
image's filesystem; package all service executables in that image. Lifecycle
behavior follows the [supervisor contract](../consonance/harmony-linux/supervisor/README.md).

`check` and application SDK assertions define correctness. `Sometimes` assertions
track reachability. Admission checks executable files and instrumentation
attestations; successful admission does not prove application coverage or
correctness. The search enables event faults only when the image supplies the
required instrumentation and symbols.

## Language preparation

`prepare` builds a configured application, composes the current libvoidstar
runtime, retains symbols and attestations, and checks image admission. `search`
and `run` also prepare when a build is configured. Docker or Podman is required.
Builds may fetch pinned toolchains and dependencies. `--offline` prevents Harmony
release downloads; it does not change container-builder networking.

| Language | Default application input | Preparation |
|---|---|---|
| C | `main.c` | Clang coverage callbacks and the SDK forwarding object |
| Rust | one Cargo package with `src/main.rs` | pinned Rust, SDK dependency, coverage passes |
| Go | one main package with `go.mod` | SDK and patched compiler wrapper, selected standard library coverage |
| Python | `main.py` and its sources | instrumented CPython interpreter, source coverage catalog and precompiled bytecode |
| Java | prebuilt `app.jar` | instrumented HotSpot and automatic bytecode back-edge callbacks |

These are starter recipes. Rust workspaces, multiple binaries, native Python
extensions, and additional Java modules need a custom `build.dockerfile`.
C/Rust/Go can instead set `build.command` to an argument array that produces
`/out/application` inside the language builder. With no language or Dockerfile,
`build.command` runs on the host to produce the configured image. Application
source is copied into the builder; preparation does not edit the source tree.
Language recipes exclude `.git` and `.harmony` from their build context and
honor the project’s `.dockerignore`. Use it to exclude other build outputs.

The [language references](../workloads/languages/README.md) describe callback
coverage and runtime limits. Runtime callbacks on instrumented threads advance
virtual time; per-action coverage quanta are preserved in recordings. Python's
minimal interpreter omits some standard native modules and rejects ctypes
callbacks. Java's default image contains `java.base`. Preparation cannot infer
readiness, test traffic, application invariants, or logging controls: configure
those explicitly.

Release builds embed their tag as `HARMONY_RELEASE_VERSION` and display it in
`harmony --version`. An installed CLI fetches that release’s source SDK. Development checkouts use
local recipes, or `HARMONY_SDK_DIR` can select a checkout. The first build of a
language runtime can be substantial; the container builder caches its layers.

## Runtime and doctor

Applications select KVM, then HVF, then UML as available. UML runs on Linux
without hardware virtualization, including ordinary containers. `--backend`
overrides selection; an explicit UML profile selects UML. Every execution prints
its selected backend. NES defaults to native QuickNES:

```sh
harmony search game.nes --core quicknes_libretro.so --name game
harmony search game.nes --backend kvm --nes-image nes.oci --name guest-game
```

The shared NES dispatcher currently supports SMB and Nova. Other games and NES
continuation/replay use the [game campaign tools](../workloads/nes/README.md).

`doctor` checks backend availability, resolves runtime artifacts, and optionally
checks image admission. It does not boot the application. Missing kernel, base
initramfs, or UML profiles are downloaded from the CLI version's release assets,
verified against SHA-256 sidecars, and cached in the user data directory. Releases
also publish the language SDK. `doctor --offline --json` reports missing artifacts
without downloading them. A development build needs local artifacts until its
version is released.

Set `kernel`, `base_initramfs`, `uml_profile`, `core`, or `nes_image` in TOML (or
the corresponding flags) for explicit inputs. Guest artifacts are also discovered
in `consonance/harmony-linux/build/ARCH` and an installation's
`share/harmony/guest/ARCH`. The cache uses `HARMONY_DATA_DIR`, otherwise
`XDG_DATA_HOME/harmony`, macOS `~/Library/Application Support/Harmony`, or Linux
`~/.local/share/harmony`. `HARMONY_GUEST_DIR` is no longer used.

## Investigate and branch

Run names resolve beneath `.harmony/runs`; every command also accepts a run
directory. `--out DIRECTORY` selects a fresh output directory. Existing runs are
never overwritten. `inspect --json` and `timeline --json` expose structured data;
human inspection lists findings and their next commands.

```sh
harmony branch baseline --bug 1 --before 1steps --name before-failure
harmony branch baseline --bug 1 --at-step 3 \
  --inject 'pause database 100ms' --follow --name delayed
harmony branch baseline --bug 1 --before 1s \
  --inject 'hook debug 1s' --follow --name verbose
harmony logs verbose
harmony diff confirmed delayed
harmony search --from before-failure --executions 2000 --name neighborhood
harmony search --resume baseline --executions 10000 --name extended
```

A branch replays the recorded prefix from the saved prepared guest. Step 0 is
post-setup; action boundaries are the available rewind points. A time rewind
rounds down to a recorded boundary and prints its resolved offset. With no point,
a branch stops one action before the end of the selected input. Branching without
`--follow` stops after the prefix and interventions. `--follow` appends the saved
suffix and runs recovery checks. This is a new experiment: an intervention can
change the timing and the outcome.

Interventions are `kill NODE DURATION`, `pause NODE DURATION`, `restart NODE
DURATION`, `wait DURATION`, or `hook NAME DURATION`. The default duration is one
second. Durations must fit whole 10ms ticks. Hooks can implement partitions or
change a running application's logging level. `--actions FILE` appends the full
[recorded action format](../workloads/faults/README.md), including targeted event
parks and kills. The timeline shows operation, coverage quantum, virtual tick,
assertions and recovery evidence.

Logging changes require an application control hook. Rebuilding with different
logging creates a different execution identity; it cannot be passed off as an
exact replay. `logs --at-step N` displays console evidence captured at a boundary.
Console captures retain a bounded 64 KiB tail on both guest backends. Adjacent
steps can overlap and old console output can be truncated. Hooks and faults may
not fire within a short window; inspect the timeline’s observed hook and event
counters as well as the requested action.

`search --from` explores a new neighborhood from a recorded prefix, with fresh
search history. Findings include that full prefix. `search --resume` restores
the search corpus, snapshots, scheduler and evidence from its latest whole-search
checkpoint. Fresh searches stop at the first objective; resuming explores past existing
findings up to the requested budget. `--executions` is the total campaign budget
when resuming. Checkpoints
are saved periodically and at normal completion. Abrupt termination can retain
an earlier checkpoint. A changed seed intentionally starts a new draw sequence.

## Saved evidence and exit status

Each run keeps `manifest.json`, `resolved.toml`, immutable prepared artifacts and
a report. Searches also retain their campaign stream, summary, checkpoints and
finding action sequences. Replays and branches retain observations and console
evidence for each action and recovery check. Plain command runs retain
`serial.log` and `run.json`. Large guest images and checkpoints consume disk
space; removing a run directory removes its evidence.

Replay verifies the exact CLI executable, host architecture, recorded artifact
digests, and (for UML) host identity. It uses saved kernel and assembled guest
bytes without rebuilding or resolving the image tag. It compares the final
execution digest, terminal condition, assertions, applied actions and recovery
evidence with the original for supervised scenarios. Plain command replay
compares the saved serial digest, exit codes and terminal record. A branch's no-recovery semantics survive subsequent
replays. Search continuations inherit execution configuration; only seed and
search budgets may change.

Exit status 0 means success, 1 means a finding, unmet reachability assertion or
application command failure, and 2 means invalid configuration, infrastructure
failure or replay divergence. A verified replay exits 0 even when it reproduces
a bug. Watchdog cutoffs remain visible as execution failures in the report.

## Verification

Run `cargo test -p harmony-cli`, the faults package tests, and the searcher tests.
`bash cli/tests/investigation.sh EVIDENCE_DIRECTORY` exercises preparation,
TOML supervision, a logging intervention with observed completion and output,
repeated prefix replay, checkpoint continuation, and artifact-tampering refusal.
It needs a container builder and the local guest artifacts. The C language CI
lane runs this bounded integration check.
