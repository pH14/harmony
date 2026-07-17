# REVIEW-TRIBUNAL — the bounded multi-seat review pipeline (process ruling)

**Status: RULED** (Paul, 2026-07-16; codified by the PR that lands this document).
Governs how substantive PRs are reviewed. Operational procedure lives in
`.claude/skills/pr-review/SKILL.md`; seat charters in `.claude/skills/pr-review/seats/`.
This document is the why, the evidence, and the boundaries — binding on process changes.

## 1. The problem this replaces (audit of 2026-07-02 → 07-16)

Forensics over the 12 heaviest PR threads (~500 classified findings), the foreman/codex
transcripts, and post-merge outcomes:

- **Volume**: 620 `codex review` invocations in two weeks; 86% of completed passes returned
  ≥1 blocking finding. Review shepherding consumed 70–85% of foreman actions in heavy
  stretches.
- **Precision was never the problem — termination was.** Only ~3–5% of findings were
  refuted. But of 17 review loops, all 8 that ended on a genuinely clean pass were ≤5
  rounds; every deeper loop (9–25 rounds) ended only by imposed authority (owner ruling,
  foreman stop-rule, or posture override). The heaviest (25 passes over one PR) never
  produced a single zero-P1 pass — on a 139-file diff, blind-pass coverage is stochastic
  and the finding supply is effectively inexhaustible.
- **Value front-loads.** Every verdict-changing catch of the fortnight landed in rounds
  1–4. Late rounds drifted to checker-hardening, speculative corner cases, and reviewing
  machinery earlier rounds had demanded. ~30 late-round bugs were introduced by earlier
  rounds' fixes — which justifies exactly one verification pass after fixes, not unbounded
  iteration.
- **Running and reading catch disjoint bug sets.** 35–65% of confirmed findings were only
  findable by reading, dominated by evidence-integrity holes — gates that lie green. Live
  runs produced two consecutive false ALL-GO certifications on the nested-x86 spike; review
  caught both (including recomputing 588,923 actual reps against a claimed 1,062,000).
  Conversely, post-merge escapes clustered where nothing ran (cfg(linux), guest kernel
  config, stale goldens, CI wiring) — and escape rate did not fall with review depth.
  Reviews and live gates are complements; neither substitutes for the other.
- **Parking works.** Deliberately parked follow-ups outnumbered escapes 1.5:1 across the
  window, and no parked item became a fire.

## 2. The design

```
Discovery tribunal            Judge                    Foreman
5–7 parallel seats     →   pool → verify →     →   post one batched review
(GPT-5.6 Sol, xhigh,       synthesize              + adjudication record
 blind, capped)            (Fable 5, xhigh)        + file P2 beads
                                                   + dispatch ONE P1 fix batch
                                                     (+ ride-along reductions)
                                                            ↓
Verify event: Closer + 1–2 sweep seats → judge → merge with parks
Contingency: one Closer-only re-check → else ESCALATE (never loop)
```

**Model ruling (Paul, 2026-07-16):** every panel seat runs **GPT-5.6 Sol at xhigh**; the
judge runs **Fable 5 at xhigh**. Claude writes (workers), Sol finds (seats), Fable judges
(verification + triage) — the cross-model check sits at both interfaces. Fable is the
stronger reviewer in isolation, but the cross-model property is worth more than the delta.

**The severity bar:** P1 = *this PR must not merge with this defect* — red gate,
determinism/replay leak, contract/wire violation, evidence-integrity hole (a gate that can
pass while its property fails), data loss/corruption, unsound `unsafe`. Everything else is
a P2 (filed as a bead at review time) or a note. Zero open P1s + green gates ⇒ merge; the
parked P2 queue is the pressure-relief valve that makes the bound safe.

**Simplicity is a standing seat at every panel size** (Paul, 2026-07-16): reduction
pressure — fewer lines, abstractions, knobs, public items — applied structurally, never as
idiom golf. Its teeth: **unspecced irreversible surface sits at the P1 bar** (public API,
wire fields, config knobs, new crates ossify within days once other agents build against
them — removing them later costs what contract drift costs), and interior reductions may
**ride along** with a P1 fix batch at the judge's discretion, when reduction is cheapest
because the worker still holds context. Parked reductions stay in the bead queue like any
P2.

## 3. Design principles and their evidence

1. **A separate verifying judge; evidence beats agreement.** The best-replicated result in
   the field: a discrimination stage downstream of high-recall finders cuts false positives
   25–95% (CORE arXiv:2309.12938; GPTLens arXiv:2310.01152; agentic SAST verification
   arXiv:2601.22952; CriticGPT arXiv:2407.00215). Verification must be evidence —
   recomputation, execution, quoted code — never agreement: ten dedicated reviewers
   unanimously endorsed a nonexistent vulnerability that a single empirical test killed
   (arXiv:2604.19049), and judges shown a majority opinion measurably degrade
   (arXiv:2410.02736). Anthropic's own cloud review runs a dedicated per-candidate
   verification step (code.claude.com/docs/en/code-review); their security action runs a
   per-finding filter model call (github.com/anthropics/claude-code-security-review).
2. **Seats aggressive, judge holds the bar; a clean report is legitimate.** Cursor's Bugbot
   learned this by inversion: once a real validator exists, restraining the finder
   double-filters and starves recall (cursor.com/blog/building-bugbot). The
   "must-find-something" effect is measured, not folklore: LLM reviewers falsely reject
   correct code at 26–92% rates, and prompts demanding explanation-plus-fix roughly double
   false flags (arXiv:2603.00539). Google shipped both production systems on explicit
   precision targets and found correct-but-low-value comments are net negative
   (research.google/blog/resolving-code-review-comments-with-ml).
3. **Seats are blind to the author's framing.** Persuasive PR metadata drops a reviewer's
   vulnerability detection from 97.2% to 3.6%; redacting the description restores it
   (arXiv:2603.18740). Seats get spec + diff + tree; only the judge and foreman read the
   PR description.
4. **Convergence across seats is priority, never confirmation.** An all-Sol panel shares
   one model's blind spots; agreement among correlated finders is weaker evidence than it
   looks (arXiv:2410.02736, 2604.19049). Randomized diff-order decorrelation (Bugbot
   v1–v10) is not implementable through `codex review --base`; lens differentiation plus
   evidence-based judging carry decorrelation instead.
5. **Bounded events and caps replace open-ended rounds.** The audit's termination data
   (§1) is the local evidence; industry practice agrees — Anthropic budgets finders
   (min 2 / max 8, scaled to diff size), caps reports, and suppresses nits on re-review;
   Codex ships only P0/P1 by default (developers.openai.com/codex). The stop is the
   default; extending a loop requires an explicit user ruling.
6. **Asymmetric verdicts; the drain is beads, not the void.** REFUTED must be constructible
   from the code (quote it); uncertain-but-real survives as PLAUSIBLE and parks. Even the
   best verification filters kill ~22% of true positives (arXiv:2601.22952) — parked beads
   make that loss recoverable, and this repo's own parked-vs-escaped record (1.5:1, zero
   fires) says the queue is trustworthy.
7. **Split before review.** Convergence broke above ~5k meaningful LOC per unit in the
   audit (4.3k converged clean in hours; 8.2k took 21 rounds; 30k never converged).
   Spec milestones are the natural unit. Vendored payloads, goldens, and generated files
   get provenance checks, never line review.
8. **Charters are procedures, not personas.** No ablation shows persona prompts beat
   independent samples plus a discriminator, and deliberating agent teams underperform
   their own best member (arXiv:2602.01011) — so seats never deliberate, and each charter
   is a scope, a mandated search procedure, and hard rules. The procedures encode this
   repo's observed catch classes (gate vacuity, determinism leaks, contract drift, dead
   production paths, hostile inputs, doomed premises, unearned complexity).

## 4. Deliberately rejected

- **Automatic disposition-feedback into seat prompts** (learned suppression, Greptile-style).
  Wrong regime for this repo: the signal would be thin and agent-generated rather than
  dense and human-voted; the invariant landscape is non-stationary (yesterday's correctly
  parked class becomes tomorrow's real bug — the SIGSTOP wedge shipped as a review-accepted
  "documented limitation," then froze a live spike three weeks later); and it is redundant
  with an evidence-checking judge. The judge emits rejected-pattern tallies as **telemetry
  only**. Promotion into a seat charter is a deliberate, human-ratified edit. Revisit the
  mechanism if telemetry shows the same pattern rejected across ≥3 PRs with the underlying
  ruling stable.
- **Reviewer memory across events** (beyond the Closer). Fresh seats demonstrably open
  theme classes that seeded reviewers miss; re-raise suppression belongs to the judge's
  adjudication record, not to the seats.

## 5. Migration

- Applies to review events that **start after this merges**; in-flight loops finish under
  the prior rules.
- Ops (user-level, not repo): `~/.codex/config.toml` keeps GPT-5.6 Sol pinned; xhigh is
  codex's default effort — pin it explicitly if it ever drifts (worker-effort ruling
  precedent, 913849f).
- The 2026-07-09 frontier posture stands and composes: **a green box gate plus a completed
  tribunal ⇒ merge**; a green box gate certifies only the paths it drives, which is why the
  Consonance and Gate Auditor seats exist even when the box is green.
- Follow-ups live in beads: the adoption bead (this PR) and a post-adoption retro after ~5
  tribunal reviews (seat-charter calibration + judge telemetry review).
