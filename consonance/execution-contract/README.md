# Execution assurance

This crate separates ordinary execution from a request for a qualified finite
composition. It belongs to the client layer: the generic VMM does not decide
which workload or evidence is qualified. Both the client and workload-owned
replay certificates depend on this small contract crate, without introducing a
client dependency into ordinary fault-history serialization.

`Requirement::Ordinary` admits execution with an explicit **unqualified**
assurance identity. It admits standard libraries and native arithmetic; existing
CPU provenance warnings remain advisory. A matching state hash or replay
certificate records an observed agreement, not a claim about CPU portability or
all possible continuations.

`Requirement::Qualified` selects a closed candidate composition. It cannot accept
caller-provided evidence, a `passed` flag, or an arbitrary contract identifier.
There is currently **no production qualified whole-composition contract** and no
public constructor or deserializer for `Qualified`. Every such request refuses
before guest boot or restore mutation. Adding a supported contract requires a
reviewed catalog and admission implementation, rather than merely adding a
manifest at runtime.

A future supported contract must bind the exact loaded host executable, guest
kernel and initramfs, engine/backend, normalization rules, admitted instruction
surface, measured source and target CPU/substrate classes, permitted restore
modes, and retained evidence identities. It must declare a finite workload and
input scope. Successful finite experiments do not establish universal future
equivalence for arbitrary programs or host implementations.

Candidate scope identities bind their declared engine, normalization, instruction
surface, substrate, restore limits and required transformation matrix. The
retained Linux candidate additionally
hashes typed executable, kernel, initramfs, instruction-contract, registry and
external evidence SHA-256 identities, plus the relevant contradictory state
digests. Raw manifests and diagnostic output stay outside the repository.
Changing any bound digest changes the candidate identity. These contract
constants are negative admission data, not a runtime artifact allowlist.

The current candidate assessments preserve two concrete limits:

- `RetainedLinuxContinuation9673` refers to the exact finite Linux continuation
  experiment at source `9673f33363395f0850cd2cb5e704432d33434180`, not arbitrary
  Session artifacts. Its raw whole-state comparison on the retained AMD cut
  contradicts the verified nested Intel continuation. Guest readbacks match, but
  x87 FIP differs in CPU state and guest RAM. The failed cross-host cell stays
  failed ([evidence](../../scripts/qualification/README.md),
  [issue #733](https://github.com/pH14/harmony/issues/733)). It is not normalized
  away. The other cells are missing **complete matrix evidence**; this does not
  relabel the separately documented bounded local passes as failures.
- `UmlWholeState` is unavailable because UML does not supply whole-state
  hashing ([issue #554](https://github.com/pH14/harmony/issues/554)). Observation
  histories do not substitute for that missing state capability.

The required matrix contains capture-and-continue, fresh restore, reused restore
following a detour, repeated restore, equivalent command segmentation, deferred
completion across restore, cold/warm cache, and cross-host restore in both
claimed directions. Missing, failed and unavailable cells refuse qualification.
The declared scope and artifact identities must match every cell; separate
experiments cannot be assembled into a broader green result by name alone.

The ordinary assurance identity is domain separated and travels through the
client's image identity and both portable snapshot forms, UML root identity,
cache namespace hashing, and workload replay certificates/history receipts.
It does not add CPU isolation to ordinary cache keys or change native backend
restore admission. Snapshot imports reject missing or changed assurance metadata;
receipt validation rejects a relabelled assurance even if the receipt's digest
was recomputed. Wire/image/certificate identities change where that new binding
changes the persisted representation. Evidence catalog edits alone do not change
the ordinary assurance identity.

Run `cargo test --locked -p execution-contract` for the closed catalog controls.
Client tests exercise real Session, worker INIT and UML boot admission before
artifact access, ordinary snapshot round trips and refusal before mutation,
sparse wire metadata, and cache namespace binding. Fault history tests verify
that observed KVM certificates and unavailable UML records remain unqualified.
