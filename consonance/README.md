# Consonance

Consonance is the run-everywhere deterministic hypervisor. It provides a
single-vCPU machine that can be captured, forked, and replayed reproducibly.

## What the evidence establishes

Determinism depends on the selected machine contract, controlled inputs, and
admitted guest instruction surface. Keep the following claims distinct when
interpreting a snapshot, replay report, or qualification result:

| Evidence | What it establishes |
|---|---|
| Artifact digest | Equality of the serialized bytes covered by that digest. |
| Logical state digest | Equality under a specified state projection and machine contract; any excluded representation differences must preserve admitted continuations. |
| Observation digest | Equality of the selected outputs covered by that digest. |
| Hardware qualification | The named tests passed for the recorded CPU, backend, runtime, and execution paths. |

Matching observations can conceal different latent machine state. Matching
endpoint state can also conceal different intervening events followed by
reconvergence. An ordered-event claim needs a comparison of the defined event
stream. A replay report establishes only the comparisons its verifier actually
performs. See the [determinism contract](../docs/DETERMINISM.md),
[testing oracles](../docs/TESTING.md), and the
[client snapshot contract](client/README.md).

The current execution model includes cooperative guest assumptions. Feature
masking and executable-image audits cover their documented surfaces; they do
not prevent every hidden, untrappable instruction or establish that arbitrary
runtime-generated code is safe. An enforced admission check rejects the cases
it validates. A cooperative assumption still requires the guest to obey the
profile. Neither an artifact digest nor a fixed CPU identity upgrades that
assumption into enforcement.

Qualification applies to the tested composition and restore path. Continuing
after capture, restoring into a fresh VM, and restoring into a reused VM after
a divergent detour exercise different obligations. Cross-host replay needs
evidence for the named host pair. A passing path does not qualify another CPU,
backend, or restore path. The [VMM core](vmm-core/README.md) and
[backend documentation](vmm-backend/README.md) describe the admission rules,
platform limits, and available qualification checks.

## Design tenets

These tenets define the direction for new work and the criteria for evaluating
changes.

- Minimize CPU- and operating-system-dependent architectural decisions. When
  they are unavoidable, encapsulate them behind narrow platform boundaries.
- Treat KVM and Hypervisor.framework (HVF) as equal targets. Architectural and
  implementation decisions must work within the constraints of both.
- Run both on bare-metal hosts and under nested virtualization.
- Remain single-vCPU. Deterministic multicore execution is an unsolved problem
  and is outside Consonance's model.
- Keep forking and replay fast.
- Keep snapshots and forks memory-efficient.
- Remain workload-agnostic. Consonance provides interfaces through which
  clients connect workloads; its internals never encode the concepts or
  vocabulary of a particular workload.

The architecture-neutral VMM engine lives in `vmm-core`, virtualization
substrates live behind `vmm-backend`, and copy-on-write guest-memory snapshots
live in `snapshot-store`. `uml` verifies and launches the User-mode Linux
profile, which runs a guest as an ordinary host process. The other READMEs in
this directory document the component boundaries.
