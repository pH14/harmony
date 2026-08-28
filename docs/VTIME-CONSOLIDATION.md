# V-time consolidation: one clock, one codebase, documented and fast

## 1. Problem

The VM-exit-count V-time work proved out on two architectures in two branches:
`claude/consonance-virtual-time-6kvrz6` (ARM: HVF on the M1 Max, KVM on msr1,
milestones M0–M6 in `docs/VM-EXIT-COUNT-VTIME-STATUS.md`) and
`claude/x86-prescriptive-vtime` (x86: stock KVM on GitHub Actions runners,
milestones X0 onward, PR #204). The repository is now carrying three costs:

1. **Two divergent branches** of the same design, forked at `a51fe015`, that
   must become one before either can land.
2. **Two clocks.** The retired-conditional-branch machinery (`WorkSource`,
   `PerfWorkCounter`, `InjectionPlanner`, the skid margin, MTF single-step
   injection, the old `run_until` contract) still exists alongside
   VM-exit-count advancement. The VM-exit-count design is the path forward;
   the branch-count machinery is retired. Git history keeps it.
3. **An undocumented determinism story.** What consonance trusts has shifted:
   it used to lean on a patched KVM; it now leans on the hypervisor layer
   plus a specific guest kernel carrying the patches that keep guest-visible
   behavior deterministic. The full story — why the system is deterministic,
   on which assumptions, with which evidence — exists only in scattered plan
   documents and status files.

It is also, today, too slow: the M3 postgres scenario spends roughly 9 ms of
wall time per VM exit (`docs/VM-EXIT-COUNT-VTIME-STATUS.md`, M3 phase table),
three orders of magnitude above what the exit itself costs. Consonance's goal
is to be the go-everywhere deterministic hypervisor, and "everywhere" includes
an old laptop and a free CI runner; that requires being fast and small.

This plan sequences the consolidation as milestones N0–N6, in strict order,
with the same evidence discipline as `docs/VM-EXIT-COUNT-VTIME.md` §3: every
milestone states what passing means and what does not count, and every
comparator used as evidence must be shown able to fail before it is trusted.

### Where consonance runs

The point of VM-exit-count V-time is that nothing in it reads the host: no
performance counters, no host clocks, no vendor-specific measurement. So the
question "where does consonance run?" reduces to "where can the guest enter
and exit?" — any hardware-virtualization surface, at any depth. The matrix
below is the go-everywhere goal made concrete. **Proven** means recorded
evidence exists (the milestone in parentheses); **expected** means the design says
it should work but no evidence exists yet — still a claim to be earned. N0 carries this
matrix into `docs/DETERMINISM.md`, which then owns it; a cell moves from
expected to proven only with the evidence cited in the cell.

| Host                                                                | x86-64 Intel    | x86-64 AMD      | arm64                     |
| ------------------------------------------------------------------- | --------------- | --------------- | ------------------------- |
| Linux KVM, bare metal                                               | expected        | expected        | **proven** (M4–M5, msr1)  |
| Linux KVM, inside a VM — nested virtualization: cloud VMs, GitHub Actions default runners | **proven** (X2) | **proven** (X2) | expected, where hosts offer it |
| Linux KVM, inside a container with `/dev/kvm`                       | expected        | expected        | expected                  |
| macOS HVF, bare metal — Apple silicon                               | —               | —               | **proven** (M0–M6, M1 Max) |
| macOS HVF, inside a macOS VM — M3+ hosts, macOS 15 nested virtualization | —               | —               | expected                  |

Within a column, sessions are portable across every row: the same seed and
image produce the same bytes on any host of that ISA. That is proven for
arm64 in both directions (M5, HVF↔KVM). Across the two x86 columns it is
the X-series' in-flight claim — the vendor-agreement work (AF, RFLAGS.RF,
MXCSR_MASK, XSAVE encodings) exists to make Intel and AMD one column in
practice, with one divergence still being hunted. Requirements are
deliberately small: one core, hardware virtualization at any nesting depth,
no performance counters, no special kernel. Not planned: Windows hosts and
Intel Macs.

## 2. Standing rules

These hold for every milestone.

- **Re-run, never inherit.** A milestone that claims a determinism property
  re-runs the check on the tree as it stands, on the machines it names.
  Evidence recorded on an ancestor commit does not transfer.
- **The reference runs are the regression check.** The same-seed boot and
  NES-campaign scenarios from M5 (normalized logs, `state_hash` sequences,
  placement checks) and the X-series runner boots are the fixed reference
  workloads. Any change — a merge, a deletion, a rename, an optimization —
  passes only if these reproduce byte-identical normalized logs and
  `state_hash` sequences on the machines the milestone names.
- **Machines.** "All three machines" means: HVF on the M1 Max, KVM on msr1
  (pinned, as in M4/M5), and stock KVM on a GitHub Actions x86 runner (both
  vendor pools, as in X2).
- **Repository checks** (`AGENTS.md`) stay green at every milestone close:
  build, nextest, clippy `-D warnings`, fmt, deny, all `--all-features`;
  Miri for unsafe crates. These are the locally run commands, and they are
  what "checks green" means in this plan. CI workflows are handled by
  cause, not by color: a workflow failure this work caused is fixed before
  the milestone closes; a failure that also reproduces on the tree as it
  stood before the milestone's changes is not this plan's blocker — record
  it in the status file, file an issue, and continue. No new CI jobs are
  required by this plan; locking more in comes later.
- **Bugs found along the way are filed** as GitHub issues (`gh`), not only
  fixed — each with the evidence that found it. Fixing in place is fine; the
  issue records that it happened.
- **Status file.** The executor maintains
  `docs/VTIME-CONSOLIDATION-STATUS.md` in the same decision/evidence format
  as `docs/VM-EXIT-COUNT-VTIME-STATUS.md`.
- **This plan is the source of truth, not conversation memory.** Re-read
  this plan and the status file at the start of every milestone, and again
  whenever earlier context has been summarized away. If working memory and
  this plan disagree, the plan wins.
- **Smallest change that passes.** Every milestone is satisfied by the
  least code and the least new structure that meets its clauses. Extra
  abstraction, extra configurability, and extra features are defects here,
  not diligence; §4 is binding and is re-read alongside this plan.

## 3. Milestones

### N0 — DETERMINISM.md, the document of record

Write `docs/DETERMINISM.md`. It absorbs and replaces
`docs/VM-EXIT-COUNT-VTIME-CLOSURE.md` and becomes the reference every later
milestone is checked against. Contents:

1. **The argument.** Determinism by induction over the event stream: a closed
   system; deterministic execution between exits (ISA semantics plus the
   defenses below); deterministic behavior at each exit (every exit's effect
   a pure function of the seed and the events so far); one total order. The
   host is never referenced, so portability across hosts of the same ISA is
   the same theorem, not a second property.
2. **The assumptions, split.** (2a) Each CPU is deterministic within the
   subset of the architecture the guest is confined to — needed for
   same-host replay. (2b) Different implementations of the ISA agree
   bit-for-bit on that subset — needed for portability. For each assumption:
   what confines the guest to the subset, and the recorded evidence (the
   M4/M5 cross-host runs; the X2 both-vendor evidence, including the
   AF-flag, RFLAGS.RF, MXCSR_MASK, and XSAVE init-state-encoding findings
   and their pinning patches).
3. **The defenses, per architecture.** The four layers from
   `docs/VM-EXIT-COUNT-VTIME.md` §2.4 — protocol, image audit, guest-kernel
   userspace confinement, hypervisor tripwires — written out separately for
   arm64 and x86. Each architecture gets a frozen table of the untrusted
   instructions, and for each one: which layer handles it and which test
   demonstrates that. The table is committed in a machine-readable form,
   because N6 generates its instruction-sweep payload from it. The x86 table is the one that needs the most new
   writing; PR #204's decisions (RDTSC through the clock page, RDRAND/RDSEED
   CPUID hiding and opcode allowlists, the flags/FPU pinning) are its raw
   material. The instructions no layer can reach (unprivileged entropy
   instructions with no user-mode disable) are stated plainly for both
   architectures.
4. **The support matrix.** The "Where consonance runs" table from this
   plan's §1 moves here and this copy becomes the one that is maintained:
   every proven cell cites its evidence, every expected cell stays
   expected until evidence exists.
5. **What is trusted, stated honestly.** What must be true of the hypervisor
   layer (HVF, stock KVM) and what must be true of the guest kernel; that
   the guest kernel's patches are essential to determinism, so the guest
   image is part of consonance, not an accessory (N4 acts on this). What is
   verified by comparing bytes (MANIFEST hashes) versus what is trusted by
   argument.
6. **Decisions.** The open questions earlier documents deferred, decided
   here and recorded: what happens to each KVM patch (the patches that
   deliver interrupts retire with the branch-count machinery in N2; the
   patches that intercept instructions either stay as tripwires or are
   retired explicitly), where the LL/SC relaxation stops, and which layer
   each future workload class must be audited at.

**Passes when** the document exists, replaces the closure document, and every
factual claim in it either cites recorded evidence (a status-file entry, a
committed test, a command transcript) or is explicitly marked *untested*.
**Does not count unless** both architectures' instruction tables cover every
instruction class the existing plan documents enumerate, and at least one
claim is honestly marked *untested* rather than asserted — a document with no
untested claims at this stage has been written aspirationally.

### N1 — one branch

Integrate `claude/x86-prescriptive-vtime` (PR #204) and
`claude/consonance-virtual-time-6kvrz6` into a single branch. The x86
branch's in-flight divergence hunt (the one remaining divergent mid-boot
checkpoint) must be resolved — its X-milestone checks green — before or as
part of this milestone; merging over a known-red check forfeits the
milestone. Since the fork point, only four files were modified on both sides
(`consonance/vmm-core/src/vmm.rs`, `snapshot.rs`, `vendor/arm64/dispatch.rs`,
`vendor/x86/mod.rs`); those merges are validated by the reference runs, not
by compilation.

One known trap: commit `57b16ce` on the x86 branch deleted the dissonance
machine/searcher code (and its deny.toml license allowances) to keep PR
#204's diff scoped to x86 work. That deletion is PR scoping, not a design
decision, and the ARM reference runs (M2/M5, the NES campaign) need that
code. In the merge, the ARM branch's dissonance tree wins everywhere:
resolve every modify/delete conflict by keeping the file, and restore any
dissonance file or deny.toml allowance the merge would silently drop — a
silent drop will not conflict, so check the merged dissonance tree against
the ARM branch's explicitly rather than trusting conflict markers to
surface it.

**Passes when** the merged tree re-runs, green, on all three machines: the M5
same-seed boot and NES-campaign runs (HVF and msr1-KVM, byte-identical
normalized logs, `state_hash` sequences, placement checks), the X-series
runner boots (both vendor pools), and the repository checks.
**Does not count unless** the evidence is produced by the merged tree itself,
and one planted one-V-ns mismatch is caught and located by the comparator on
the merged tree before the real equalities are accepted.

### N2 — one clock: delete the branch-count machinery, retire "prescriptive"

Delete the retired-conditional-branch clock wholesale: `WorkSource` and its
implementations (`ScriptedWork`, `PerfWorkCounter`), `InjectionPlanner` with
its skid/margin/single-step state machine and simulator, the old `run_until`
delivery path, the force-exit and MTF single-step dependencies, and every
configuration, test, and document that exists only to serve them. Where a
type survives with a narrower job (e.g. `CpuBackend`, `Backend::run_until`),
its contract is rewritten for what it now does; where nothing survives, the
type goes. The N0 decision on each KVM patch is applied here. Git history is
the archive; no `#[deprecated]` half-state, no dead code behind feature
flags.

With one clock left, the qualifier goes too: the codebase says **virtual
time**, advanced by **VM exit counts**. `prescriptive` disappears from
identifiers, file names (`prescriptive_vtime.rs` included), workflow names,
and prose. Historical records (status files, recorded decisions, closed
PRs/issues) stay verbatim.

Issue tracker sweep: every open issue that exists only because of the
branch-count clock (the clearest example is #170, exact-arrival arming across
a pvclock re-anchor) is closed as not-planned with a one-line comment naming
this milestone. Issues that survive the deletion in reworded form (none are
currently known) are re-scoped instead.

**Passes when** the tree builds and all repository checks pass with the
machinery gone; the N1 reference runs pass again on all three machines; a
case-insensitive search for `prescriptive` over the tree returns only
historical records; and the issue sweep is recorded in the status file with
each closed issue number and its one-line rationale.
**Does not count unless** the deletion removes the code outright (search
finds no orphaned modules, unused feature flags, or commented-out remains)
and the frozen public-API snapshots are regenerated to the new surface in
the same change.

### N3 — fast: ≥10× on the reference workload

Make the M3 postgres scenario at least ten times faster in wall time, with
byte-identical determinism output. All performance numbers in this milestone
— the baseline, the improvement, the benchmark harness — are measured on the
M1 Max only: GitHub Actions runners span several hardware generations, so
their wall times are not comparable run to run. Runners still run the
determinism re-checks; they are never the benchmark machine. Order of work:
**measure first** — profile the run loop on the M1 Max and record the
baseline phase table and the top costs in the status file before changing
anything. (Likely
suspects, to be confirmed or ruled out by the profile, not assumed:
rendering and flushing the normalized text log on every event; synchronous
I/O on the event path; per-exit serialization or hashing beyond the sparse
checkpoints; the build profile of the measured binary.) Then optimize,
re-running the reference runs after each change. Close with a small
benchmark harness (a pinned scenario and an events-per-second measurement)
committed to the tree with its baseline number recorded in the status file,
so future regressions are measurable — as a tool, not a required CI job.

**Passes when** the M3 postgres scenario's wall time improves ≥10× against
the recorded baseline on the M1 Max, with normalized logs and `state_hash`
sequences byte-identical to the pre-optimization runs of the same tree (on
all three machines, per the standing rules), and the before/after profiles
are in the status file.
**Does not count unless** the baseline profile was recorded before the first
optimization landed, and every optimization commit's reference re-run is
listed (a single end-of-milestone run does not establish which change
preserved determinism).

### N4 — the guest is part of consonance

Move `harmony-linux/` under `consonance/` (final path recorded in the status
file), reflecting N0's decision that the guest kernel is essential: the
hypervisor does not meet its determinism contract with an arbitrary guest.
Build scripts, workflows, and documents follow the move. The AGPL/GPL
boundary stays exactly as crisp as today: kernel patches and kernel-adjacent
code keep their licenses and their own workspace separation; the move is of
location, not of license.

This milestone also fixes **#172**: the `/dev/harmony` driver serializes
against agents ringing the same doorbell pages from userspace, with a test
that drives a concurrent ringer against an in-flight transaction and fails
on the unfixed driver. M6 made this latent race real — the threshold
protocol is live traffic now — so it must be fixed before any composition
co-runs an instrumented process with an agent.

**Passes when** the tree builds and all checks pass at the new layout on both
macOS and Linux; the N1 reference runs pass again (guest images rebuilt from
the moved tree, hashes recorded); and the #172 test demonstrably fails
against the pre-fix driver and passes against the fixed one.
**Does not count unless** the guest images used in the re-run were built
from the moved tree (not cached artifacts of the old layout), shown by their
recorded hashes checked against a fresh manifest.

### N5 — reproducible guest builds

Pin the guest build with Nix: one flake (or equivalent pinned nixpkgs
revision plus lockfile) that produces the guest kernel(s), initramfs
image(s), and payload binaries from source — kernel.org source plus the
in-tree patch series, musl/busybox userland, the postgres and NES payloads —
with no network access at build time beyond the pinned, hash-checked inputs.
The guest remains what it is today: consonance's own patched kernel and tiny
init; nixpkgs is where the packages come from, not the operating system. The
committed manifest (per-artifact SHA-256) becomes an output of the build,
and the M5-style same-bytes check becomes "rebuilt from the lock" rather
than "copied and checksummed".

**Passes when** two independent builds on different hosts (msr1 and a
GitHub Actions runner; a macOS-hosted Linux builder may substitute for
either) produce byte-identical artifacts for every image in the manifest,
and the N1 boot runs pass again using the Nix-built images on all three
machines.
**Does not count unless** the two builds are on distinct machines from a
clean store (no shared binary cache between them for the artifacts under
test), and a deliberate one-byte patch change is shown to change the output
hash — the reproducibility check must be able to fail.

### N6 — the defenses, tested by attacking them

Implement the verification work `docs/DETERMINISM.md` §3 prescribes (the
successor to the closure document's T0–T4), for **both** architectures,
skipping what prior milestones already proved and saying so per item:

- **The instruction sweep.** A guest payload generated from the frozen
  tables that executes every instruction in them — every row, not a sample —
  twice from the same seed on each machine. A row a layer handles must
  produce byte-identical results across the two runs. A row no layer can
  reach (the unprivileged entropy instructions) can never be made
  deterministic by executing it, so its assertion is the mask instead: the
  feature bit is hidden from the guest, and the image audit rejects the
  opcode. Every row carries exactly one of those two claims; there is no
  third category. Because the payload is generated from the table, a table
  row without a probe fails the build, and the sweep report states the row
  count it exercised against the table's row count.
- Trap verification with a JIT-emitted in-guest probe per architecture (a
  kernel configuration with the traps off must fail the check).
- Completing both frozen instruction tables against their generated
  listings.
- The LL/SC decision with a demonstration that the accumulating variant
  diverges.
- The tripwire audit for whichever intercept patches N0 decided to keep.

Each item lands with its meaningful positive and planted negative, per the
closure document's original discipline.

**Passes when** every item in the doc's verification section is either
recorded as passed with evidence in the status file or explicitly recorded
as out of scope with a reason, and `docs/DETERMINISM.md` is updated so its
*untested* markings from N0 reflect what N6 actually tested.
**Does not count unless** the traps-off configurations demonstrably fail
their checks before the traps-on configurations are credited, and the
instruction sweep's exercised-row count equals the table's row count on both
architectures — a sweep that silently skips rows does not count.

## 4. Out of scope

- New CI jobs or required checks beyond keeping existing workflows green.
- New workloads, new SDK capabilities, or search/campaign features.
- The dissonance searcher beyond keeping its existing tests green.
- macOS-on-KVM, M3+/EL2 features, or any new hypervisor layer.
