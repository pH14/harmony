# Task 165 — W4: ARM spike harness input + stage-gating hardening (hm-kdih)

**Work order:** `hm-kdih` (P1 epic, label `work-order`). Read it first — `bd show hm-kdih`
— then each child with `bd show <id>`. All five findings come from the PR #132 review.

**Surface:** `spikes/arm-altra/harness/src/arm_spike.rs`, `el0.rs`, `run.rs`, plus
`arm_el0_count.rs` and the run-set schema for the two items that reach them. Nothing else.

## Calibrate before you start

This is an **operator-controlled tool**. None of these five is reachable by an untrusted
guest — every trigger requires someone running the spike binary with hostile or wrong
arguments. So do not build a threat model that isn't there, and do not over-engineer.

The reason it is still worth a P1 work order: this harness is the least-defended code in a
project whose entire output is **evidence integrity**. Two of these five let a run produce a
manifest that misdescribes what actually happened, which is the same species the last three
merges have been about. The other three are robustness.

Do them as one pass — they live in three files and would be one sitting for one person.

## The five

### Stage-gating — a manifest that lies about its own run

- **`hm-ej5` (J15)** — `arm_spike.rs:1054` selects `run_sample_exact` on `Preempt` +
  `skid_margin` alone. So `--stage aa1 --mechanism patched --skid-margin N` records
  `skid=0` **under an aa1 manifest label**. Require `Aa3`, or reject the combination
  outright. This is the one that most directly produces false evidence: read it first and
  decide whether any retained AA-1 manifest could have been produced this way. If one could
  have been, **stop and report** before changing anything — that would be an evidence
  question, not a code fix.
- **`hm-usj` (J17)** — `gen-run-inputs.py:48` can emit `kvm_mode: protected` (nVHE) but the
  run-set schema permits only `vhe`/`nvhe`, so a **supported** mode yields a schema-invalid
  manifest. Add the enum value, or reject before writing — pick one and say why. Silently
  writing an invalid manifest is the worst of the three options.

### Robustness

- **`hm-8z7` (J14)** — `el0.rs:294` has no preflight bound on
  `classes × scales × cases × reps`; `--cases u64::MAX` OOMs. Add product, file, and record
  ceilings, with an error that names which ceiling was hit and what the value was.
- **`hm-bec` (J16)** — `run.rs:1017` increments `deliveries` and continues unbounded after
  landing; a fast duplicate `Preempt`-exit storm resets the per-`KVM_RUN` watchdog and hangs
  the sample. Cap advisory exits per sample. A hang is worse than a failure here because it
  burns a box window silently.

### Miri reach

- **`hm-fou` (J13)** — `arm_el0_count.rs`'s raw `rt_sigaction` / `mmap` / `ucontext` /
  `global_asm` is cfg-disabled on the Miri host, so the project's standing
  `unsafe ⇒ Miri` discipline **never runs it**. Extract a portable, Miri-exercisable seam for
  the record-assembly and bookkeeping logic.

  Be honest about what this buys: the genuinely unsafe syscall and asm surface stays outside
  Miri's reach — that is inherent, not something to paper over. What you can bring under Miri
  is the logic *around* it. Say plainly in the implementation record which parts remain
  un-exercised, rather than letting "Miri now covers `arm_el0_count`" stand as the summary.
  A vacuous Miri gate is itself a finding — the Gate Auditor seat hunts exactly that, and it
  would be reviewing your work.

## Note on overlap

`hm-7np`'s scan-bound sub-item overlaps `hm-8z7` (work order W2 owns the rest of `hm-7np`).
If you find yourself fixing the same bound twice, do it once here and note it on `hm-7np` so
W2 does not redo it.

## Gates

`arm-harness` nextest, clippy `-D warnings`, fmt, and Miri for the new seam on the pinned
nightly with the `MIRIFLAGS` from `.github/workflows/quality.yml`. Every behavior change
ships its regression in the same commit. For the two input-bound items, the regression is a
hostile-argument test that must fail before your fix and pass after — show both.

Do not weaken any existing check to make a bound fit.

## Deliverable

PR from `task/arm-spike-harness-hardening` closing `hm-ej5`, `hm-usj`, `hm-8z7`, `hm-bec`,
`hm-fou` with the merge. Lead the PR body with `hm-ej5` — whether a mislabelled manifest was
ever actually producible is the question a reviewer will want answered first.
