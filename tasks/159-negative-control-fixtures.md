# Task 159 — W1: every grader must be able to fail (hm-537)

**Work order:** `hm-537` (P0 epic, label `work-order`). Read it first —
`bd show hm-537`, including its `notes` field, which carries Paul's 2026-07-18
greenlight and the reasoning — then every child with `bd show <id>`.

## The doctrine

**A grader without a negative-control fixture proving it can go red is not a gate.**

Ten independently-confirmed findings across the ARM (PR #132/#135) and AMD (PR #128)
reviews are the same defect species in six files: *a check that returns green
without having actually checked anything.* None of them falsified a shipped result —
every one was re-verified by hand against retained evidence — but collectively the
gates are weaker than their names promise.

This is harmony's own core doctrine turned on its own evidence pipeline. We prove a
campaign works by having it find a planted bug. A gate that has never been seen to
fail is unproven by exactly that standard. Two green-on-fail escapes in one week
(PR #132's headline AA-3 co-tenant MATCH shipped on a comparator that was never
invoked — `hm-gzp`; and nested-x86 PR #98) motivated the ruling. A negative control
catches both.

## The single most important deliverable

**Write the planted-failure fixture harness ONCE.** That is the reason these ten
findings are one work order instead of ten. Ten separate fixes would produce ten
bespoke fixture styles and no reusable discipline. What you build should make "add a
negative control for this grader" a small, obvious, repeatable act for every future
spike gate — mutate one retained-transcript byte, drop one sample point, corrupt one
digest, and the gate **must** go red in CI.

Design it before you fix anything. The fixtures live next to the graders they arm;
the harness that runs them is shared. Say in the PR body what a future spike author
has to write to arm a new gate — if that answer is longer than a few lines, the
harness is not done.

Each child ships **its fix plus the fixture that would have caught it.**

## Scope: Mac-side only

This task does not touch the determinism box — another worker owns the box lane and
co-tenancy there is itself a determinism variable. Two children have owed box-side
halves; do the Mac half, and leave the box half on the bead with a precise note of
what the box run must show.

The `spikes/amd-epyc/harness/*.c` files are Linux/KVM C that will not build on the
Mac. Make the edits, keep them small and obviously correct, and state in the PR body
that they are un-compiled — the foreman runs the syntax check at review. (Wiring
those into a Linux CI runner is `hm-l82`, a separate bead; do not do it here.)

## The children

### `spikes/arm-altra/host/aa1c-determinism-check.py`

- **`hm-cte` (PR132 verify C2)** — `:143-147` uses `.get()` for `state_digest`,
  `measured_taken`, `overflow.deliveries`, so records omitting all three **on both
  lanes** compare `None == None` and report MATCH having compared nothing. Only
  triggers under symmetric schema drift; current retained records carry all three
  (verified against `aa1c-armed-smoke-001/records.jsonl`). Require and type-check the
  three fields. Note `aa3-determinism-compare.py` is immune here (KeyError →
  EvidenceError) — that asymmetry is the tell, and the fix should make both files
  agree.

### Both ARM comparators (`aa1c-determinism-check.py`, `aa3-determinism-compare.py`)

- **`hm-6sj` (PR132 verify C1)** — neither comparator attests **lane provenance**:
  not the recorded condition (pinned-solo vs co-tenant), not distinct `run_set_id`,
  not environment/mechanism/image compatibility. Pass the same run-set dir twice, or
  a copied/mislabeled lane, and you get a full-join MATCH. Not reachable on the
  automated path (the scripts feed distinct solo-ref vs co-tenant dirs) and the
  retained evidence is hash-proven — but "a determinism comparator that MATCHes a
  directory against itself" is the single most embarrassing sentence in this backlog.
  Fix it, and make the negative control be exactly that: same dir twice ⇒ red.

### `spikes/arm-altra/schemas/floor-check/src/check.rs`

- **`hm-7q0` (PR135 F5)** — (a) `:1498` `if r.step.is_some() { continue }` precedes
  the trips-vs-oracle check at `:1520`, so step records' trips are never graded.
  First verify whether step records legitimately carry no graded trips; if they do
  carry them, grade them. (b) `results/aa-6/live-20260720/MANIFEST.txt:12` records
  `VERDICT DEMONSTRATED` while floor-check FAILs two sub-checks
  (weights / aa6-matrix) that are transparently listed out of scope — no false green,
  but provide a **scoped invocation that exits zero** for the demonstrated scope so
  the manifest's verdict is machine-checkable rather than prose-reconciled.
- **`hm-gmt` (PR132 J11)** — `:1397` exempts every `step.is_some()` record from
  `CountExactness` regardless of stage, so a non-AA-2 step run bypasses count checks
  entirely. Restrict the exemption to AA-2, or reject non-AA-2 step records outright.
  Note `count-exactness` is the payloads' *semantic* gate and is independent of the
  byte pins — weakening it accidentally is expensive.

### `spikes/arm-altra/harness/src/arm_spike.rs`

- **`hm-9zy` (PR132 J5)** — `:318` permits excluding **any** payload via a generic
  `--exclude-payload`, the selection is not retained in the manifest, and AA-3 has no
  required-class check. The retained AA-3 evidence excluded only the ruled `wfi-idle`
  (7 classes present), so the shipped result stands. Fix: record exclusions in the
  manifest **and** add an AA-3 required-payload-matrix check — or restrict the flag
  to the ruled names. Prefer recording + checking over restricting: the manifest
  should be able to prove what ran.

### `spikes/amd-epyc/schemas/check-floors.py`

- **`hm-5a6` (PR128 P2-4)** — `check_overflow` (`:89-92`) reads `hits_1_ok` / `lost` /
  `dup` from the harness's own `overflow_summary`, while the docstring (`:5-8`)
  claims it never uses a summary line the harness asserted. The exactness half is
  genuinely independent. **Mac-side now: fix the docstring so it stops claiming
  something false.** The real fix — retaining richer per-arm overflow records so the
  checker can recompute — cannot be applied retroactively to the committed run;
  specify it on the bead as the forward-looking half.

### `spikes/amd-epyc/harness/ae0-probe.c`

- **`hm-e1n` (PR128 P2-7)** — `:182` returns 0 unconditionally, so it cannot serve as
  an automated stage-stop. It is honestly a capability *reporter*, judged from its
  emitted JSON rows, and no committed disposition rests on its exit code — so this is
  a capability gap, not a falsified result. Make it exit non-zero when a load-bearing
  capability is absent.

### `spikes/amd-epyc/harness/singlestep-driver.c`

- **`hm-pex` (PR128 P2-2)** — `:84` reads RFLAGS inside `vm_set_start`, i.e. **before**
  `KVM_SET_GUEST_DEBUG` at `:189-190`, so in mode `tf` both `guest_tf` and `tf_kept`
  are 0 trivially, and `IMPLEMENTATION.md:23` cites "guest-transparent (tf_kept=0)" on
  that basis. Mac-side: **relabel the claim** to what the code actually establishes.
  The genuine guest-PUSHF-observes-TF test needs the box and is an owed residual —
  record precisely what that box run must do. AE-2's GO itself rests on the sound
  #DB-count-vs-oracle exactness, not on this, and your relabel must not read as though
  AE-2 is in doubt.

## Gates

`floor-check` nextest + clippy `-D warnings` + fmt for the Rust; the Python graders'
own test path plus your new fixtures; **and the negative controls themselves running
in CI and observed to go red on the planted failure.** A fixture that is not wired
into CI is a script, not a control — demonstrate the red in the PR (paste the failing
output) and show the CI wiring.

Do not weaken any existing gate to make a fixture pass. If a fix makes a currently
green gate go red, that is a finding: stop, report it, and do not adjust the gate.

## Deliverable

PR from `task/negative-control-fixtures` closing `hm-7q0`, `hm-cte`, `hm-6sj`,
`hm-gmt`, `hm-9zy`, `hm-e1n`, `hm-5a6` (Mac half), `hm-pex` (Mac half) with the
merge, with the owed box-side halves recorded on their beads. Lead the PR body with
the fixture harness — how to arm a new gate with it — because that is the part of
this work that outlives the ten fixes.
