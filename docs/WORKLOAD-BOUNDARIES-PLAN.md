# Workload packages and a fault-free execution core

Status: architecture and delivery specification. The restructuring establishes
the package boundaries, existing NES execution paths, and extracted fault policy
and SDK libraries. A separate delivery stage adds the systems fault package, its
guest runtime, fixture, CLI registration, and acceptance gates. Component READMEs
describe implemented capabilities and their validation coverage.

## Purpose

Harmony combines deterministic execution with search over program behavior.
Consonance supplies the deterministic machine and controlled Linux environment.
Dissonance explores executions and retains useful states. Workload packages
connect these services to concrete programs by supplying execution setup,
actions, observations, and evaluation policies.

This change establishes independently usable execution and exploration cores,
with NES demonstrating the boundaries. The subsequent distributed-system package
uses the same interfaces. Consonance
builds and runs ordinary workloads without a fault package. Dissonance builds as
an independent search library. Packages depend on their interfaces and supply
workload-specific behavior.

## User interface and execution requirements

The search command selects a package and passes it a workload input. The `faults`
command belongs to the systems-package delivery stage:

```sh
harmony search --package nes smb.nes
harmony search --package faults foo.oci
```

The NES package supports two execution backends as first-class user-facing
capabilities:

```sh
harmony search --package nes --backend native smb.nes
harmony search --package nes --backend consonance smb.nes
```

`native` runs QuickNES directly in the host worker process. `consonance` runs
QuickNES inside the controlled Linux guest and uses whole-VM snapshots. Both
backends use the same game adapter, controller actions, evaluation policies, and
campaign engine. Each supported game adapter, initially SMB and Nova, must
support both backends.

The NES default is `native`. The faults package defaults to
`consonance`, its initial supported backend. Each package declares its supported
backends and default. The CLI records and reports the resolved backend and gives
an actionable error when required artifacts or host capabilities are unavailable.
An explicitly selected backend is never silently substituted.

The NES package identifies supported games by ROM content hash and resolves the
appropriate adapter and runtime. Additional ROM revisions have explicit mappings;
an unknown hash produces an unsupported-adapter error. The faults package accepts
an OCI image that supplies the system under test and its startup/checking contract.

Distributed systems execute entirely inside one Consonance VM on one virtual
CPU. Replicas, guest networking, supervisors, and fault-runtime state share one
execution and snapshot boundary.

## Architecture and dependency direction

| Component | Responsibility | Interface supplied to consumers |
|---|---|---|
| Consonance | Deterministic execution, controlled Linux, platform inputs, snapshots, replay, and extension state | Machine control and raw observations |
| Consonance client | Sessions, control operations, event access, and snapshot resource handling | Workload-neutral host API |
| Dissonance | Campaign scheduling, rollouts, mutation machinery, archive retention, recording, and search replay | Typed search interfaces |
| Workload packages | Input interpretation, runtime preparation, actions, observation decoding, evaluation, and policy composition | Runnable search configuration |
| Fault toolkit | Fault definitions, injection, sampling policies, guest tooling, and host adapters | Reusable fault actions and execution support |
| Harmony CLI | Package/backend selection, common campaign options, invocation, and reporting | Search command |

Packages depend on core interfaces. The CLI composes packages and cores through
an explicit Rust registration boundary. Dispatch occurs at campaign startup so
targets, actions, and snapshots retain concrete types internally.

Use a top-level `workloads/` area for packages, guest payloads, and reusable
workload support. Initially group NES support and game adapters in one package;
create separate crates where host/guest builds or dependency isolation require
it. Keep the Consonance client beside its platform contracts. Preserve the two
existing core workspaces and define explicit membership for the extracted crates.

### Cargo-enforced dependency boundaries

Represent architectural ownership in a checked-in dependency policy and enforce
it against Cargo metadata. The policy classifies every first-party crate and
declares allowed dependency edges between groups. Classify new crates as part of
adding their manifests; an unclassified crate fails the check.

Use distinct groups for search core, execution core, platform contracts, search
contracts, Consonance client, workload support, workload packages, and composition
applications/tests. The allowed edges encode these responsibilities:

- Search core consumes search contracts and other search-core crates.
- Execution core consumes platform contracts and execution-core crates.
- Contract crates have explicitly enumerated dependencies appropriate to their
  interface, keeping their transitive dependencies within the core boundary.
- The Consonance client consumes platform contracts and the execution implementation
  needed for an in-process client.
- Workload support and packages consume the contracts and implementations they adapt.
- CLI and integration-test crates compose the components under test.

Implement `scripts/check-dependency-boundaries.py` using `cargo metadata
--format-version 1` across both core workspaces and standalone manifests. Inspect
declared normal, build, and development dependencies, including inactive optional
and target-specific declarations. Resolve workspace inheritance and dependency
renames through Cargo's metadata, identifying local crates by package identity and
canonical manifest path. Discover repository-owned manifests within declared source
roots and require each to be covered; explicitly distinguish vendored sources and
generated build output. Checking every local edge also catches forbidden paths
through intermediary crates.

Failures name the source crate, destination crate, dependency kind, and violated
group rule. Integration tests that compose cores with packages live in dedicated
test crates and follow the same classification rules. Keep third-party advisory,
license, source, and targeted crate bans in the existing `cargo-deny` gate; use
its wrapper allowances where access to a particular dependency belongs to a
specific adapter.

Run the boundary checker as a required portable CI step. Test the checker with
fixtures for cross-workspace edges, aliases, optional and target-specific
dependencies, build/dev dependencies, and unclassified crates. Independent build
fixtures additionally demonstrate that the permitted graph produces usable cores.
Add a review lens to `REVIEWING.md` for semantic ownership: core-interface changes
identify the mechanism, its consumer, and its snapshot/replay contract. Cargo
checks enforce dependency structure; this review covers behavior placed inside
an otherwise correctly classified crate.

## Package and runtime contracts

### Preparation

A package accepts its input, selected backend, and options, then prepares a
runnable system and typed search adapter. Its preparation result binds:

- Workload and execution artifact identities, configuration, and capabilities.
- A deterministic readiness/genesis boundary and worker construction procedure.
- Action, observation, evaluation, and input-policy versions.
- Snapshot compatibility and requirements for rebuilding a campaign origin.

NES preparation resolves the ROM, game adapter, and pinned QuickNES runtime.
Native preparation supplies the host emulator library. Consonance preparation
assembles the guest agent, emulator, and ROM on the controlled Linux platform.
Emulator settings and initialization have equivalent game-visible meaning across
both paths; backend-specific artifacts retain their own identities.

The faults package defines an image-side contract for startup, logical node
identity/control, readiness, and assertions. One image can contain a supervisor
and several replicas. Application checks establish correctness properties;
crash and runtime observations provide a separate baseline signal. A searchable
database package supplies its invariants and progress interpretation explicitly.

### Search and evaluation

Decompose the existing `Game` responsibilities into target execution, workload
evaluation, input policy, and reporting:

- Target execution constructs and restores targets, applies actions, captures
  observations, and produces snapshots.
- Evaluation classifies outcomes, derives archive keys, merges progress and
  milestones, and checks candidate viability.
- Input policies generate suffixes and optionally maintain history-derived state.
- Reporting formats retained evidence.

Dissonance owns restore, parent-path replay, suffix iteration, action limits,
candidate assembly, and stop handling. Preserve observations at action boundaries
and within actions. Workloads define their logical action cost, such as emulator
frames for NES.

Represent normal continuation, workload failure, successful completion, and
execution/infrastructure errors distinctly. Preserve existing recorded semantics
during extraction and version subsequent changes.

A retention probe restores the complete candidate state before rollout continues,
including adapter caches, observation buffers, and pending input. Restoration
failure aborts the job. The workload implements the probe; the engine determines
when to invoke it.

Stateless and history-derived input policies implement their respective contracts.
The engine controls deterministic update order, admission order, and checkpoints.

### Execution adapters and guest observations

Split `ConsonanceMachine` into generic VM control/snapshot support and an NES guest
driver. NES support owns controller encoding, 2 KiB RAM frames, save RAM, and
billboard interpretation. The generic client exposes raw memory and events;
each workload adapter defines typed evidence.

The native QuickNES driver and NES guest driver implement the same NES execution
contract. Their snapshots remain backend-specific. Runtime selection happens
once at startup and uses shared argument parsing and campaign invocation.

Put the NES publication codec in shared host/guest code. Specify protocol
versions, publisher-scoped discovery, region bounds, action completion, and
publication lifetime. Resolve names within publisher scope and reject ambiguous
or incomplete declarations. Check existing catalog capabilities before extending
the transport.

Read publications at coherent stopped boundaries. Validate lengths, arithmetic,
required regions, versions, and descriptor validity across restore/restart. Share
codec implementations and use independent golden fixtures. Support layout changes
through explicit versions and region descriptions as required by the guest agents.

### Platform and payload assembly

Consonance supplies reproducible kernel/base Linux artifacts, SDK transport, and
reusable image assembly. Packages own workload executables, data, setup commands,
resource requirements, and readiness conditions.

Move NES guest-agent code, ROM installation, emulator options, and hugepage
requirements into the NES package. Bind platform and payload hashes together with
launch settings in preparation metadata. The systems-package stage applies the same assembly contract to its workload.
Platform acceptance fixtures stay with the platform components they exercise.

### Fault policy extraction and subsequent guest execution

Separate `environment` into platform mechanisms and optional fault behavior.
Consonance retains deterministic entropy, input delivery, recording, and replay.
The fault toolkit owns fault catalogs, eligibility, probabilities, fault-aware
mutation, decoding, and enforcement.

A versioned request/response channel carries opaque workload payloads, request
identities, deterministic stops, and recorded answers. Consonance validates
transport, ordering, and bounds; packages interpret the content. Support both
installed guest-local schedules and host-selected answers at decision points.
Scheduled actions use deterministic execution progress.

The systems-package delivery stage adds guest enforcement for:

- Directional network loss, partition, delay, and recovery.
- Process pause/resume and kill/restart with existing files retained.
- Application failpoints and custom actions.
- Recovery windows for convergence and invariant checks.

Define interception coverage, stable node identities, fault overlap, and restart
behavior. The supervisor survives the logical node failures it injects. Guest
fault state participates in whole-VM snapshots.

Optional external device or machine-control implementations provide an extension
path for faults requiring control below guest userspace. Their execution-affecting
state participates in deterministic scheduling, snapshot/restore, identity, and
replay. Introduce extension interfaces around concrete implementations. Inventory
existing host perturbations and migrate supported enforcement through these
interfaces with explicit compatibility handling.

### Identity, replay, and backend equivalence

Record workload semantics, execution compatibility, and search semantics
separately. Exact replay and snapshot import validate execution identity. New
formats receive explicit versions and supported historical formats retain defined
compatibility paths.

Backend comparison executes the same actions from compatible genesis and compares
workload evidence. Native emulator and whole-VM snapshots are backend-specific;
comparison reconstructs state through recorded actions. Snapshot-root campaigns
require a reconstructible origin for cross-backend comparison.

Extend the Nova observation oracle to cover setup, per-action observations,
terminal outcomes, restored continuations, and retention probes. Apply the same
contract tests to SMB. Native execution is both a supported campaign backend
and an independent reference for VM-backed workload behavior.

Campaign comparison uses a defined semantic projection of recorded results and
fixed logical work. Use non-binding memory limits and an unbounded host-time
allowance for these bounded-work comparisons: different snapshot sizes can
otherwise change eviction and later search decisions. Production campaigns retain
backend-specific resource accounting and strict replay of their recorded choices.

## Implementation sequence

Land each phase through small coherent pull requests with component README updates
and relevant checks. Separate mechanical relocation from semantic changes. Track
follow-ups in GitHub issues and preserve implementation history in Git.

### 1. Establish migration evidence

Inventory consumers of `Game`, `Machine`, `environment`, guest catalogs, image
recipes, and host perturbations. Include `dissonance/fuzzer`, the CLI, acceptance
suite, release scripts, and nightly jobs. Distinguish implemented fault enforcement
from declared but unsupported variants.

Capture SMB/Nova fixtures covering terminal actions, probes, history-derived
input policies, resume origins, and memory pressure. Run the existing Nova oracle
on supported Linux/KVM infrastructure and establish restore/branch baselines.
Record artifact versions and platform requirements.

Inventory first-party manifests, assign dependency groups, and implement the Cargo
boundary checker. Record existing violating edges as exact temporary exceptions
with an owning migration issue; CI rejects additional violations. Remove each
exception in the phase that relocates its behavior.

Completion: consumers and compatibility obligations are documented, with executable
replay and observation baselines and an active dependency regression gate.

### 2. Extract workload dependencies

Move game modules, binaries, and NES dependencies into the NES package. Retain
`Game` temporarily while keeping archive/campaign logic in an independently
buildable search crate. Add an external-consumer fixture implementing a small
deterministic target using only the search interfaces.

Update crate classifications and remove search-to-workload dependency exceptions.
Put package/core composition tests in their own integration-test crate.

Completion: the Dissonance library builds independently of NES and hypervisor
implementations, and existing replay fixtures retain their meaning.

### 3. Centralize rollouts and compose policies

Compare the three rollout loops branch by branch, including probe/key ordering,
milestones, initial terminal checks, and failure paths. Introduce the composed
interfaces and move rollout mechanics into the engine while preserving seeded
draws and update boundaries. Preserve snapshot pinning, conservative charges,
and admission-order resource release.

Completion: all workloads use the engine rollout loop and appropriate input-policy
interfaces, with matching migration replay results.

### 4. Build both NES execution paths

Extract the generic Consonance client and share the NES host/guest codec. Implement
scoped discovery and strict publication validation. Consolidate native and VM
construction behind one typed campaign entry point while preserving snapshot
resource lifetimes and trace-buffer retirement.

Implement guest initialization for SMB and Nova and run the shared execution
contract against both backends. Test restore, branch, terminal outcomes, and probes.

Completion: each supported NES game runs campaigns directly in QuickNES and inside
Consonance through the same adapter. Both paths satisfy the observation contract
and enforce backend-specific snapshot compatibility.

### 5. Extract fault behavior

Introduce the generic decision channel and recorded-input support. Move fault
policy, codecs, mutation, and guest enforcement into the toolkit. Adapt existing
fault-aware VMM handlers and host perturbation consumers to explicit mechanisms
and optional implementations. Version affected protocols and reproducers; provide
compatibility handling for supported historical artifacts.

Build and execute Consonance with the fault toolkit absent from its dependency
graph and guest installation.

Completion: the execution core owns platform mechanisms; optional packages own
fault semantics and enforcement. Supported existing behavior has a migration path.

### 6. Establish package-owned preparation

Factor platform image assembly from workload recipes. Move NES payloads and
workload-specific probes into their package, retaining platform acceptance tests.
Implement preparation for the native runtime and guest runtime, recording their
resolved identities and preserve reproducible-image and release-discovery checks.

Completion: NES owns its setup and payloads, using reusable Consonance platform
artifacts where selected. OCI preparation is available as workload support.

### 7. Expose package and backend selection

Register `nes` and implement its search commands. Resolve
package defaults, validate backend support, and expose `--backend native` and
`--backend consonance` for NES. Report resolved execution configuration and precise
errors for missing artifacts, unknown ROMs, incompatible images, and unavailable
host capabilities. Integrate separated identities into recording and existing
replay APIs. Transitional binaries may delegate to the shared entry point.

Completion: CLI checks cover both explicit NES backends, package defaults, and
unsupported selections. Hardware acceptance executes a campaign through each
NES backend. The systems-package stage adds the `faults` registration and its
Consonance acceptance.

### 8. Implement and validate the systems package separately

Add the guest runtime and `faults` package registration. Package a small replicated
system with a known failure, nominal control, assertions,
and deterministic progress signal using the bug corpus conventions. Run every
replica in one VM/vCPU. Reproduce a fault schedule through the runtime, then discover
the failure through Dissonance.

Restore with messages, delays, and restarts pending. Verify identical continuations,
alternative branches, and recovery-window checks. Add another guest fault entirely
within the toolkit/package to exercise the extension boundary.

Completion: the workload demonstrates useful search and complete fault-state
restoration. Adding the fault changes package code while preserving the same
Consonance binary and base Linux artifacts.

Phases 5 and 6 can proceed independently once their interfaces are established.
Phase 7 integrates the execution and preparation paths; phase 8 verifies the full
composition. TetaNES cleanup can land separately after checking image consumers,
with its build wiring, documentation, and license exception updated together.

## Validation and acceptance

Use `CONTRIBUTING.md`, component scripts, and CI for validation commands. Run
formatting, clippy, tests, and dependency/license checks for affected workspaces.
Update CI path filters and standalone-workspace checks as code moves. Guest and
client changes run reproducibility and hardware acceptance gates. Document safety
invariants beside unsafe blocks and exercise changed unsafe logic under Miri.
Report hardware gate results with their execution environment.

Run the Cargo boundary checker on every change in the portable quality gate,
alongside `cargo-deny`. Its manifest discovery covers newly added crates and
standalone workspaces. Complete the migration by removing temporary architectural
dependency exceptions.

Restructuring acceptance requires:

1. Independent Dissonance and fault-free Consonance builds.
2. User-selectable native and Consonance campaigns for SMB and Nova.
3. Shared NES execution semantics verified across both backends, including restored
   branches and terminal boundaries.
4. Preserved search replay, snapshot compatibility, and resource accounting.
5. Strict shared guest codecs with malformed-input and discovery tests.
6. Existing fault policy and SDK behavior supplied by optional package libraries.
7. End-to-end execution of both NES package/backend CLI combinations.
8. A required Cargo dependency gate covering all first-party manifests and dependency
   kinds, with complete crate classification and all migration exceptions retired.

The subsequent systems-package stage adds its guest supervisor, process/network
actions, replicated fixture, `faults` CLI registration, and hardware gates. Its
acceptance requires fault discovery, whole-VM restoration with pending work, and
recovery checks with every replica inside one VM on one virtual CPU.
