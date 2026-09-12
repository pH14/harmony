# Consonance

Consonance is the run-everywhere deterministic hypervisor. It provides a
single-vCPU machine that can be captured, forked, and replayed reproducibly.

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
live in `snapshot-store`. The other READMEs in this directory document the
component boundaries.
