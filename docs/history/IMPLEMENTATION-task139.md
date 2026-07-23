# Task 139 — mutants job: survive past the ~59-min hosted-runner preemption window

**Bead:** hm-y53x (P1). Related: hm-4w7q (the kill evidence), hm-jfw (stale-install / re-run
follow-up). Surface: `.github/workflows/quality.yml` (mutants job only) + this doc.

This write-up is the review record for a CI-only change (no crate directory). The foreman
should lift the **Diagnosis** and **Costed options** sections into the PR description; the
live GHA proof runs on that PR (see **First-run expectations**).

---

## 1. Diagnosis — the ~59-min kills are genuine hosted-runner preemption

**Method.** `gh run view` / `gh api .../actions/jobs/<id>/logs` over the mutants-job history on
`task/differential-vertical` (PR #134), starting from the run named in the spec
(`29810466534`). All runs below are the pre-shard *single* mutants job (274-mutant diff); the
4-way shard fix landed after them.

| Run | mutants job | Duration | Kill signature (log) | Reading |
|-----|-------------|----------|----------------------|---------|
| `29810466534` (seed) | job `88616159823` | 11:09:19→12:08:35 = **59m16s** | `The runner has received a shutdown signal` @12:08:32, then `The operation was canceled` | preemption |
| `29805115005` | job `88553999596` | 05:49:59→06:47:33 = **57m34s** | `The runner has received a shutdown signal` @06:47:30 | preemption |
| `29799489193` | job `88537546520` | 03:48:18→05:11:34 = **83m16s** | several `MISSED` mutants **and** `The runner has received a shutdown signal` @05:11:30 | real reds **and** preemption, at 83 min |
| `29745930209` | job `88363761291` | 13:21:49→15:27:59 = **126m10s** | log since GC'd (BlobNotFound) | ran 126 min uninterrupted |
| `29855903141` shard 0/4 | job `88720019521` | →18:28:18 | `The operation was canceled` **only** — no shutdown-signal line; every sibling shard stopped at the *same instant* (18:28:2x), coinciding with the next push (run `29857330385`, 18:28:04) | **concurrency cancel** (contrast case) |

**Hypotheses, and how each was settled:**

1. **A `timeout-minutes` someone already set — RULED OUT.** The mutants job carried *no*
   `timeout-minutes` and the workflow has no top-level/`defaults` timeout. Decisive evidence:
   the same 274-mutant diff ran **83 min** (`29799489193`) and **126 min** (`29745930209`)
   *uninterrupted past an hour*. A fixed timeout cuts every run at the *same* wall-clock mark;
   these did not. (GitHub's documented hosted-runner ceiling is **6 h/job**, far above any of
   these.)

2. **Workflow `concurrency` cancel-in-progress fired by a later push — RULED OUT for these
   kills.** The workflow does have `concurrency: { group: quality-${{ github.ref }},
   cancel-in-progress: true }` (unchanged by this task). But a concurrency cancel has a
   *different signature*, shown by the contrast run `29855903141`: status `cancelled`, log
   shows **only** `The operation was canceled` (no shutdown-signal line), and *all* jobs in the
   run stop at the *same instant* the superseding push starts. The preempted runs instead show
   status `failure`, the shutdown-signal line *first* (then "operation was canceled" as a
   consequence), a ~57–59-min mark, and **no** superseding push at that time (e.g. the 06:47
   kill on `29805115005` — the next run didn't start until 39 min later).

3. **Org/repo runner policy / non-standard runner — RULED OUT.** The killed jobs ran on the
   standard hosted pool: `runner_group_name: "GitHub Actions"`, `labels: ["ubuntu-latest"]`.
   No policy caps a standard job below the 6 h ceiling.

4. **Genuine host preemption — SURVIVING hypothesis.** `The runner has received a shutdown
   signal. This can happen when the runner service is stopped, or a manually started runner is
   canceled.` is GitHub's canonical message for the ephemeral runner VM being reclaimed
   mid-job. It is *stochastic*: it hit at 57 and 59 min on two runs, yet two other runs on the
   identical diff survived to 83 and 126 min. That variance is the fingerprint of preemption,
   not of any deterministic limit.

**Consequence for the fix:** host preemption tears down the entire runner VM, so **no in-job
retry can survive the specific ~59-min kill.** The layered defense is (a) sharding — already
merged, keeps each leg well under the window — plus (b) the two no-cost hardening measures
below, plus (c) a cost-bearing option (§4) for the residual.

---

## 2. The no-cost hardening (this PR)

Both changes are confined to the `mutants` job in `.github/workflows/quality.yml`.

**(a) Explicit `timeout-minutes: 90`.** Comfortably above the observed ~59-min preemption
window (so the timeout itself never lands *inside* it) and far below the 6 h hosted-runner job
ceiling, so a pathologically slow shard fails *cleanly* at 90 min instead of burning hours. It
is calibrated for the 4-way-sharded job (typical shard ~30–40 min for a PR-#134-sized diff;
90 min ≈ 2.5× headroom). It also doubles as a **tripwire**: a shard that legitimately needs
>90 min is the signal to raise the shard count (8-way), not to raise the timeout.

**(b) Infrastructure-only retry** around each of the three `cargo mutants` calls. A plain
retry action (e.g. `nick-fields/retry`) retries on *any* nonzero exit — which would re-run a
genuine surviving-mutant red until it flaked green. That is exactly what the spec forbids, so
the retry is a small inline bash loop that **discriminates by cargo-mutants exit code**
(verified empirically against v27.1.0):

| exit | meaning | action |
|------|---------|--------|
| `0` | all diffed mutants caught, or none in the diff | **pass** |
| `2` | surviving (missed) mutants | **REAL RED → fail now, never retry** |
| `3` | tests timed out | **real result → fail now, never retry** (the `mutants.toml` 3× timeout multiplier already absorbs infra slowness, so a residual timeout is a real signal) |
| other (`4` baseline-broken, `137` OOM, tool-install/network blip, interrupted run) | infrastructure/transient | **retry on this runner, bounded to 2 attempts, then fail with that code** |

The "never mask a red" property was exercised for every path, including the adversarial case
where the *retry attempt itself* returns a real red (infra `4` → `2`): the loop propagates
exit `2` and does not mask it. Bound is 2 attempts (one retry) — enough to shed a one-off
transient, small enough to stay cheap; infra failures (baseline build, tool install) surface
early (~3 min), so the retry cost is bounded regardless of a shard's length.

**What the retry does *not* do:** it cannot rescue the ~59-min host-preemption kill itself
(the runner VM is gone, taking the bash loop with it). It rescues the *other* infra classes —
transient baseline/toolchain/network failures and cargo-mutants' own interrupted runs — that
would otherwise turn a clean diff red. Preemption survivability rests on sharding + §4.

---

## 3. Finding: `ci/gha-migration` removes sharding (re-exposes the window)

The in-flight `ci/gha-migration` branch (`b3d7f4e`) **un-shards** this job (`mutants
(${{ matrix.shard }}/4)` → `mutants`, drops every `--shard` flag) and renames
`harmony-linux/{sdk,flow-agent}` → `guest/{sdk,flow-agent}`. Un-sharding removes the *primary*
preemption defense: the full 274-mutant diff took 83–126+ min single-job, i.e. well past the
~59-min window. After that migration lands, **`timeout-minutes` + retry + §4 are the only
survivability levers left** — and `timeout-minutes: 90` would then sit *below* the legitimate
single-job runtime and must be re-tuned (or sharding kept). This change and `ci/gha-migration`
both edit the mutants-job region, so a merge conflict there is *inherent*; the 3-step
structure here was kept (rather than collapsed) so the resolution is mechanical (re-apply the
per-step retry wrapper onto the renamed/un-sharded commands). **Flagging for Paul/foreman:**
un-sharding on `ci/gha-migration` is a survivability regression for this exact problem.

---

## 4. Costed options for the residual (PROPOSAL — do not flip without Paul's ruling)

The free sharded baseline already resolves the immediate problem; these address the "future
big-diff shard could still approach the window" residual. Per-minute rates are GitHub's
published larger-runner Linux pricing (~$0.004/min per vCPU) — **verify against current
pricing before adopting.**

| Option | Cost | Preemption exposure | Caveats |
|--------|------|---------------------|---------|
| **A. Status quo — standard `ubuntu-latest`, 4-way shard + `timeout-minutes` + infra-retry** (this PR) | **$0** (free, unlimited minutes for a public repo) | **Low** — each shard finishes well under the window; residual preemption is rare/stochastic | In-job retry can't rescue a mid-shard host preemption; a big future diff may need 8-way sharding. |
| **B. GitHub-hosted larger runner** (e.g. Linux 8-vCPU) | **~$0.032/min, billed** even on a public repo (larger runners are *not* covered by free public-repo minutes). A ~35-min shard × 4 ≈ 140 runner-min ≈ **~$4.5 per full run**, and every push re-runs. | **Lower** — larger runners are dedicated VMs, less subject to shared-pool preemption; more vCPUs let `cargo mutants --jobs` finish faster, shrinking the exposure window. | Real money per PR push; needs org billing. The free tier already suffices *with* sharding, so this mainly buys speed/headroom. |
| **C. Self-hosted dispatchable runner for mutants** (the existing determinism box, or a dedicated VM; aligns with the documented `runs-on: [self-hosted, kvm]` follow-up) | **$0 incremental GitHub billing**; reuses the box whose lease is already sunk for the ~23 box gates. | **None** — you own the machine: no preemption, no 6 h ceiling. | ⚠️ **Security:** self-hosted runners on a *public* repo let untrusted fork-PR code execute on the box (GitHub explicitly warns against this). Requires fork-PR approval gating + an ephemeral/isolated runner. ⚠️ **Reliability:** the box's availability *fluctuates* (docs box-access); making it a *required* PR gate couples PR throughput to box uptime. ⚠️ Adds ops/maintenance surface. |

**Recommendation (for Paul to rule):** ship A now (this PR). Prefer **8-way sharding** over B
if a future diff outgrows the window (still free). Reserve **C** for when mutants must be
guaranteed-uninterruptible, and only with fork-PR isolation + ephemeral runners — its public-
repo security and box-uptime caveats make it a poor fit for a *required* per-PR gate.

---

## 5. First-run expectations (foreman: the live proof runs on the PR)

I cannot push, so GHA never executed this edited workflow locally. On the foreman's PR:

- The `mutants (0..3/4)` legs run as today. The retry wrapper is a **no-op on the happy path**
  (exit 0 → pass first try) and on a **genuine red** (exit 2/3 → fail first try, no retry) —
  so a green PR stays green and a real surviving-mutant PR stays red, unchanged.
- `timeout-minutes: 90` appears in the job's timing UI; expect each shard to finish far under
  it (~9–40 min historically).
- To *see* the retry fire, watch for a `::warning::cargo-mutants infrastructure failure …
  retrying on this runner` annotation — it only appears on an infra exit code (`4`/OOM/etc.),
  which is rare.
- The change is confined to the mutants job; `actionlint` (with its shellcheck pass over the
  `run:` blocks) is clean. No crate code changed, so the Rust gates are unaffected.

---

## 6. Gates

CI-only diff (no Rust touched). Ran the portable gates anyway (spec asks for it — they're
fmt/deny no-ops here) and validated the workflow:

- `actionlint .github/workflows/quality.yml` → clean (includes shellcheck over `run:` blocks).
- Retry control flow exercised under GHA's exact shell (`bash --noprofile --norc -eo
  pipefail`) for exit codes `0 / 2 / 3 / 4-persistent / 4→0 / 4→2`; all behaved per the table.
- cargo-mutants exit-code contract (`0/2/4/1`, empty-diff `0`) verified empirically on
  v27.1.0.
