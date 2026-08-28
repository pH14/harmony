# V-time consolidation: one clock, one codebase, documented and fast

## 1. Problem

The VM-exit-count V-time work proved out on two architectures in two branches:
`claude/consonance-virtual-time-6kvrz6` (ARM: HVF on the M1 Max, KVM on msr1,
milestones M0–M6 in `docs/VM-EXIT-COUNT-VTIME-STATUS.md`) and
`claude/x86-prescriptive-vtime` (x86: stock KVM on GitHub Actions runners,
milestones X0 onward, PR #204). The repository is now carrying three costs:

1. **Two divergent branches** of the same design, forked at `a51fe015`, that
   must become one lineage before either can land.
2. **Two time bases.** The retired-conditional-branch machinery (`WorkSource`,
   `PerfWorkCounter`, `InjectionPlanner`, the skid margin, MTF single-step
   injection, the descriptive `run_until` contract) still exists alongside
   VM-exit-count advancement. The VM-exit-count design is the path forward;
   the branch-count machinery is retired. Git history keeps it.
3. **An undocumented determinism story.** The trusted base shifted: consonance
   used to lean on a patched KVM; it now leans on the substrate plus a
   specific, attested guest kernel carrying the closure patches. That story —
   why the system is deterministic, on which premises, with which evidence —
   exists only in scattered plan documents and status ledgers.

It is also, today, too slow: the M3 postgres scenario spends roughly 9 ms of
wall time per VM exit (`docs/VM-EXIT-COUNT-VTIME-STATUS.md`, M3 phase table),
three orders of magnitude above the substrate's exit cost. Consonance's goal
is to be the go-everywhere deterministic hypervisor, and "everywhere" includes
an old laptop and a free CI runner; that requires being fast and small.

This plan sequences that consolidation as milestones N0–N6, in strict order,
with the same evidence discipline as `docs/VM-EXIT-COUNT-VTIME.md` §3: every
milestone states what passing means and what does not count, and every
comparator used as evidence must be shown able to fail before it is trusted.

## 2. Standing rules

These hold for every milestone.

- **Re-run, never inherit.** A milestone that claims a determinism property
  re-runs the oracle on the tree as it stands, on the substrates it names.
  Evidence recorded on an ancestor commit does not transfer.
- **The determinism corpus is the regression oracle.** The same-seed boot and
  NES-campaign scenarios from M5 (normalized logs, `state_hash` sequences,
  placement checks) and the X-series runner boots are the fixed reference
  workloads. Any change — a merge, a deletion, a rename, an optimization —
  passes only if these reproduce byte-identical normalized logs and
  `state_hash` sequences on the substrates the milestone names.
- **Substrates.** "All three substrates" means: HVF on the M1 Max, KVM on
  msr1 (pinned, as in M4/M5), and stock KVM on a GitHub Actions x86 runner
  (both vendor pools, as in X2).
- **Repository gates** (`AGENTS.md`) stay green at every milestone close:
  build, nextest, clippy `-D warnings`, fmt, deny, all `--all-features`;
  Miri for unsafe crates. No new CI jobs are required by this plan beyond
  keeping the existing workflows passing; locking more in comes later.
- **Issues found en route are filed** (`gh`), not only fixed — each with the
  evidence that found it. Fixing in place is fine; the issue records that it
  happened.
- **Status ledger.** The executor maintains
  `docs/VTIME-CONSOLIDATION-STATUS.md` in the same decision/evidence format
  as `docs/VM-EXIT-COUNT-VTIME-STATUS.md`.

## 3. Milestones

### N0 — DETERMINISM.md, the document of record

Write `docs/DETERMINISM.md`. It subsumes `docs/VM-EXIT-COUNT-VTIME-CLOSURE.md`
(which it replaces) and becomes the north star every later milestone is
checked against. Contents:

1. **The argument.** Determinism by induction over the event stream: a closed
   system, deterministic interiors (ISA semantics plus the closure below),
   deterministic boundaries (every exit's effect a pure function of seed and
   event history), one total order. The host is never referenced, so
   portability across hosts of the same ISA is the same theorem, not a second
   property.
2. **The premises, split.** (2a) Each CPU implementation is deterministic in
   the architecturally-determined subset the guest is confined to — needed
   for same-host replay. (2b) Distinct implementations of the ISA agree
   bit-for-bit on that subset — needed for portability. For each premise:
   what confines the guest to the subset, and the recorded evidence (the
   M4/M5 cross-host oracles; the X2 both-vendor evidence, including the
   AF-flag, RFLAGS.RF, MXCSR_MASK, and XSAVE init-state-encoding findings
   and their pinning patches).
3. **The closure, per architecture.** The four layers from
   `docs/VM-EXIT-COUNT-VTIME.md` §2.4 — protocol, image audit, guest-kernel
   userspace confinement, substrate tripwires — written out separately for
   arm64 and x86, each with its frozen disposition table of untrusted
   instructions and, per entry, which layer closes it and which oracle
   demonstrates that. The x86 table is the one that needs the most new
   writing; PR #204's decisions (RDTSC through the clock page, RDRAND/RDSEED
   CPUID hiding and opcode allowlists, the flags/FPU canonicalizations) are
   its raw material. The residual class (unprivileged entropy instructions
   with no user-mode disable) is stated plainly for both architectures.
4. **The trusted base, stated honestly.** What must be true of the substrate
   (HVF, stock KVM) and what must be true of the guest kernel; that the
   guest kernel's patches are load-bearing for determinism, so the guest
   image is part of consonance, not an accessory (N4 acts on this). What is
   attested by bytes (MANIFEST hashes) versus what is trusted by argument.
5. **Rulings.** The open decisions earlier documents deferred, decided here
   and recorded: the fate of each KVM patch (delivery-path patches retire
   with the branch-count machinery in N2; intercept-path patches remain as
   substrate tripwires or are retired explicitly), the LL/SC relaxation
   boundary, and the layer at which each future workload class must be
   audited.

**Passes when** the document exists, replaces the closure document, and every
factual claim in it either cites recorded evidence (a status-ledger entry, a
committed test, a command transcript) or is explicitly marked *untested*.
**Does not count unless** both architectures' disposition tables are complete
over the instruction classes the plan documents already enumerate, and at
least one claim is honestly marked *untested* rather than asserted — a
document with no untested claims at this stage has been written
aspirationally.

### N1 — one lineage

Integrate `claude/x86-prescriptive-vtime` (PR #204) and
`claude/consonance-virtual-time-6kvrz6` into a single branch. The x86
branch's in-flight divergence hunt (the one remaining divergent mid-boot
checkpoint) must be closed — its X-milestone oracle green — before or as part
of this milestone; merging a known-red oracle forfeits the milestone. Since
the fork point, only four files were modified on both sides
(`consonance/vmm-core/src/vmm.rs`, `snapshot.rs`, `vendor/arm64/dispatch.rs`,
`vendor/x86/mod.rs`); those merges are validated by oracle, not by
compilation. PR #204's apparent dissonance diff is the shared pre-fork
lineage and dissolves in the merge; nothing is stripped.

**Passes when** the merged tree re-runs, green, on all three substrates: the
M5 same-seed boot and NES-campaign oracles (HVF and msr1-KVM, byte-identical
normalized logs, `state_hash` sequences, placement checks), the X-series
runner boot oracle (both vendor pools), and the repository gates.
**Does not count unless** the evidence is produced by the merged tree itself,
and one planted one-V-ns negative is localized by the comparator on the
merged tree before the real equalities are accepted.

### N2 — one clock: delete the branch-count machinery, retire "prescriptive"

Delete the retired-conditional-branch time base wholesale: `WorkSource` and
its implementations (`ScriptedWork`, `PerfWorkCounter`), `InjectionPlanner`
and the skid/margin/single-step state machine and its simulator, the
descriptive `run_until` delivery path, the force-exit and MTF single-step
substrate dependencies, and every configuration, test, and document that
exists only to serve them. Where a type survives with a narrower job (e.g.
`CpuBackend`, `Backend::run_until`), its contract is rewritten for what it
now does; where nothing survives, the type goes. The N0 ruling on each KVM
patch is applied here. Git history is the archive; no `#[deprecated]`
half-state, no dead code behind features.

With one clock left, the qualifier goes too: the codebase says **virtual
time**, advanced by **VM exit counts**. `prescriptive` disappears from
identifiers, file names (`prescriptive_vtime.rs` included), workflow names,
and prose. Historical records (status ledgers, recorded decisions, closed
PRs/issues) stay verbatim.

Issue tracker sweep: every open issue that exists only because of the
branch-count path (the anchor case is #170, exact-arrival arming across a
pvclock re-anchor) is closed as not-planned with a one-line comment naming
this milestone. Issues that survive the deletion in reworded form (none are
currently known) are re-scoped instead.

**Passes when** the tree builds and all repository gates pass with the
machinery gone; the N1 oracle set re-runs green on all three substrates; a
case-insensitive search for `prescriptive` over the tree returns only
historical records; and the issue sweep is recorded in the ledger with each
closed issue number and its one-line rationale.
**Does not count unless** the deletion removes the code outright (search
finds no orphaned modules, feature-graveyards, or commented-out remains) and
the frozen public-API snapshots are regenerated to the new surface in the
same change.

### N3 — fast: ≥10× on the reference workload

Make the M3 postgres scenario at least ten times faster in wall time, with
byte-identical determinism output. Order of work: **measure first** — profile
the run loop on the M1 Max and on a runner, record the baseline phase table
and the top cost centers in the ledger before changing anything. (Prime
suspects, to be confirmed or retired by the profile, not assumed: per-event
normalized-log text rendering and flushing; synchronous I/O on the event
path; per-exit serialization or hashing beyond the sparse checkpoints; build
profile of the measured binary.) Then optimize, re-running the corpus after
each change. Close with a small benchmark harness (a pinned scenario and an
events-per-second measurement) committed to the tree with its baseline
number recorded in the ledger, so future regressions are measurable — as a
tool, not a required CI job.

**Passes when** the M3 postgres scenario's wall time improves ≥10× against
the recorded baseline on the M1 Max **and** on a GitHub Actions runner, with
normalized logs and `state_hash` sequences byte-identical to the
pre-optimization runs of the same tree, and the profile-before/profile-after
tables are in the ledger.
**Does not count unless** the baseline profile was recorded before the first
optimization landed, and every optimization commit's corpus re-run is listed
(a single end-of-milestone run does not establish which change preserved
determinism).

### N4 — the guest is part of consonance

Move `harmony-linux/` under `consonance/` (final path recorded in the
ledger), reflecting N0's ruling that the guest kernel is load-bearing: the
hypervisor does not meet its determinism contract with an arbitrary guest.
Build scripts, workflows, and documents follow the move. The AGPL/GPL
boundary stays exactly as crisp as today: kernel patches and kernel-adjacent
code keep their licenses and their own workspace separation; the move is of
location, not of license.

This milestone also fixes **#172**: the `/dev/harmony` driver serializes
against agents ringing the same doorbell pages from userspace, with a test
that drives a concurrent ringer against an in-flight transaction and fails
on the unfixed driver. M6 made this latent race load-bearing — the threshold
protocol is real traffic now — so it must close before any composition
co-runs an instrumented process with an agent.

**Passes when** the tree builds and all gates pass at the new layout on both
macOS and Linux; the N1 oracle set re-runs green (guest images rebuilt from
the moved tree, hashes recorded); and the #172 test demonstrably fails
against the pre-fix driver and passes against the fixed one.
**Does not count unless** the guest images used in the oracle re-run were
built from the moved tree (not cached artifacts of the old layout), shown by
their recorded hashes differing-or-matching against a fresh manifest.

### N5 — reproducible guest builds

Pin the guest build with Nix: one flake (or equivalent pinned nixpkgs
revision plus lockfile) that produces the guest kernel(s), initramfs
image(s), and payload binaries from source — kernel.org source plus the
in-tree patch series, musl/busybox userland, the postgres and NES payloads —
with no network access at build time beyond the pinned, hash-checked inputs.
The guest remains what it is today: consonance's own patched kernel and tiny
init; nixpkgs is the package universe, not the operating system. The
committed manifest (per-artifact SHA-256) becomes an output of the build,
and the M5-style same-bytes attestation becomes "rebuilt from the lock"
rather than "copied and checksummed".

**Passes when** two independent builds on different hosts (msr1 and a
GitHub Actions runner; a macOS-hosted Linux builder may substitute for
either) produce byte-identical artifacts for every image in the manifest,
and the N1 boot oracles re-run green using the Nix-built images on all three
substrates.
**Does not count unless** the two builds are on distinct machines from a
clean store (no shared binary cache between them for the artifacts under
test), and a deliberate one-byte patch perturbation is shown to change the
output hash — the reproducibility comparator must be able to fail.

### N6 — the closure, verified adversarially

Implement the verification workstream `docs/DETERMINISM.md` §3 prescribes
(the successor to the closure document's T0–T4), for **both** architectures,
skipping what prior milestones already sealed and saying so per item:
adversarial trap verification with a JIT-emitted in-guest probe per
architecture (a fail-open kernel configuration must fail the oracle);
completion of both frozen disposition tables against their generated
listings; the LL/SC ruling with its accumulating-variant divergence
demonstration; and the substrate-tripwire audit for whichever intercept
patches N0 ruled to keep. Each item lands with its meaningful positive and
planted negative, per the closure document's original discipline.

**Passes when** every item in the doc's verification section is either
sealed with evidence in the ledger or explicitly recorded as out of scope
with a reason, and `docs/DETERMINISM.md` is updated so its *untested*
markings from N0 reflect what N6 actually tested.
**Does not count unless** the fail-open configurations demonstrably fail
their oracles before the fail-closed configurations are credited.

## 4. Out of scope

- New CI jobs or required checks beyond keeping existing workflows green.
- New workloads, new SDK capabilities, or search/campaign features.
- The dissonance searcher beyond keeping its existing tests green.
- macOS-on-KVM, M3+/EL2 features, or any new substrate.
