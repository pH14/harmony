---
name: pr-review
description: >
  Review a pull request for the harmony project via the tribunal pipeline and post the
  batched review on it. Use this whenever the user asks to review a PR, look over a task
  branch, check a delegated agent's work, or give feedback on a change — even if they just
  say "take a look at PR 3" or name a task branch like task-02-snapshot-store. Covers
  finding the right task spec, running the gates, the parallel review seats (codex /
  GPT-5.6 Sol), the judge (Fable 5), and posting the review via gh.
---

# PR review — the tribunal pipeline

PRs in this repo are produced by delegated agents, each implementing against a written
spec. The review's job is to check the work against **that spec and the project
conventions** — not generic code review taste. Most real problems here are contract
violations, determinism leaks, or gates that don't prove what they claim, so the pipeline
concentrates effort there.

The pipeline is **bounded by design** (ruled 2026-07-16, `docs/REVIEW-TRIBUNAL.md`): one
discovery tribunal → one judge-triaged fix batch → one verify event → at most one
Closer-only re-check → merge-with-parks or escalate. Never loop past the cap; extending a
loop requires an explicit user ruling.

## 0. Tier and size the review

- **Light tier** (docs, specs, ROADMAP, feedback, handoff plans): foreman sanity read +
  the standard gates, then merge. No tribunal, no codex.
- **Substantive tier** (crate code, determinism surfaces, contracts/wire formats,
  `unsafe`, gates/CI): the pipeline below.

Panel size scales with **meaningful LOC** — vendored payloads, goldens, and generated
files are excluded (they get a provenance/manifest check, never line review):

| Meaningful LOC | Seats |
|---|---|
| < ~1k | 5 — Gate Auditor, Consonance, Contract+Adversary (merged), Wiring, Simplicity |
| ~1k–5k | 6 — Gate Auditor, Consonance, Contract, Adversary, Wiring, Simplicity |
| > ~5k, or a new crate/artifact/approach | 7 — add Architect |

Simplicity sits at **every** size — reduction pressure is a standing directive, not a
big-PR luxury.

A single review unit over ~5k meaningful LOC should usually not exist: ask first whether
the PR splits along spec milestones (the Architect seat's question). Review per milestone
unit; always run Gate Auditor + Wiring on the assembled head as well.

## 1. Gather context before anything reads code

1. **PR metadata**: `gh pr view <n> --json title,body,headRefName,files,url`. Note
   dependency-whitelist exceptions requested in the description (conventions rule 5) and
   linked issues. **The PR description is judge/foreman input only — seats never see it**
   (author framing measurably suppresses detection; see the ruling doc).
2. **The task spec**: branch/title names the task (`task-01-hypercall-proto` →
   `tasks/01-hypercall-proto.md`). The Public API section is a contract.
3. **`tasks/00-CONVENTIONS.md`**: re-read each review; it changes.
4. **Prior feedback**: `feedback/` for earlier reviews touching this task. Don't re-litigate
   resolved points; do check accepted feedback was applied.
5. **`docs/INTEGRATION.md`** whenever the PR touches anything cross-component.

## 2. Check out and run the gates first

Findings from running the code outrank findings from reading it. From a detached worktree
(the main checkout stays untouched):

```sh
git -C ~/workspace/harmony fetch origin
git -C ~/workspace/harmony worktree add --detach ../harmony-review-pr<N> origin/<head-branch>
cargo build -p <crate> --all-features
cargo test  -p <crate> --all-features
cargo clippy -p <crate> --all-features --all-targets -- -D warnings
cargo fmt -p <crate> -- --check
# unsafe ⇒ Miri (pinned nightly + MIRIFLAGS match .github/workflows/quality.yml):
MIRIFLAGS=-Zmiri-permissive-provenance cargo +nightly-2026-06-16 miri test -p <crate>
```

A red gate is an automatic P1 — quote the failing output. A platform-specific failure is
itself a finding (rule 6, both platforms must pass). Note box-gate status for the judge:
green box + completed tribunal ⇒ merge (2026-07-09 posture), but a green box gate certifies
only the paths it drives — it never waives the tribunal.

## 3. The discovery tribunal (parallel seats)

Seats (charters in `.claude/skills/pr-review/seats/`): **gate-auditor** (does green mean
anything), **consonance** (record==replay bit-identity), **contract** (spec/ABI/wire
conformance), **adversary** (hostile inputs + `unsafe`), **wiring** (deliverable alive
end-to-end), **simplicity** (what is the least of this — structural reduction, unspecced
surface), **architect** (should this exist as designed — kill/split authority).

All seats run **GPT-5.6 Sol at xhigh** (model pinned in `~/.codex/config.toml`; xhigh is
codex's default), concurrently, each in its own detached worktree. The worktree `AGENTS.md`
is the lens-injection mechanism — `codex review --base` accepts no positional prompt but
auto-reads `AGENTS.md`:

```sh
PR=<N>; HEAD=origin/<head-branch>; cd ~/workspace/harmony
for SEAT in gate-auditor consonance contract adversary wiring simplicity architect; do  # trim per §0
  WT=../harmony-review-pr$PR-$SEAT
  git worktree add --detach "$WT" "$HEAD"
  cat AGENTS.md .claude/skills/pr-review/seats/COMMON.md \
      .claude/skills/pr-review/seats/$SEAT.md > "$WT/AGENTS.md"
  ( cd "$WT" && gtimeout 1200 codex review --base origin/main \
      -c approval_policy='"never"' -c sandbox_mode='"workspace-write"' \
      > /tmp/codex-review-pr$PR-$SEAT.md 2>&1 ) &
done; wait
```

(`--base origin/main`, never `main`: the fetch in §2 does not advance the local `main`
ref, and a stale base makes every seat review commits that are not this PR's. At the
5-seat size the merged Contract+Adversary seat is one worktree whose `AGENTS.md`
concatenates both charters after `COMMON.md`.)

A timed-out or truncated seat gets ONE re-run; a seat that dies twice is reported to the
judge as a named coverage gap — never silently skipped. Findings are the final `codex`
block of each output (`grep -nE '\[P[0-9]\]'` to orient, then read the tail).

## 4. The judge

A **fresh Fable 5 session at xhigh** with its own context — never the foreman in-session
(the judge's independence and the foreman's context budget are both the point). Spawn it
mirroring the worker pattern:

```sh
WT=../harmony-review-pr$PR-judge
git -C ~/workspace/harmony worktree add --detach "$WT" "$HEAD"
cat > "$WT/.judge-prompt.md" <<EOF
You are the review judge for harmony PR #$PR. Read your charter at
.claude/skills/pr-review/seats/judge.md in this worktree and follow it exactly.
Seat reports: /tmp/codex-review-pr$PR-*.md. This is a <discovery|verify> event.
Write DISPOSITION.md and ADJUDICATION.md at the worktree root, then stop.
EOF
tmux new-session -d -s agent-judge-pr$PR -c "$WT" \
  "CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION=0 caffeinate -i claude --permission-mode auto \
   --model claude-fable-5 --effort xhigh \"\$(cat .judge-prompt.md)\"; \
   echo '[judge exited]'; exec bash"
```

Wait on `DISPOSITION.md` appearing (state-based wait, never process-grep), sanity-check it,
then kill the session. The judge pools (dedupe + cite-or-reject), **verifies every
load-bearing claim by evidence** (CONFIRMED / PLAUSIBLE / REFUTED), and synthesizes
dispositions against the bar — **P1 = this PR must not merge with this defect**. Details
live in the charter; the foreman does not re-verify except by spot-check.

## 5. Post, file, dispatch (foreman)

From `DISPOSITION.md`, in one pass:

1. **Post one batched review** — build the JSON first, then submit (proofread + recover on
   API failure). Severity prefixes: `**[blocking]**` = P1, `**[suggestion]**` = P2,
   `**[question]**`, `**[nit]**`. `event` = `REQUEST_CHANGES` iff any P1, else `APPROVE` —
   **except on own-authored PRs** (worker and foreman PRs share the one gh identity, i.e.
   nearly every PR here): GitHub rejects both events on your own PR, so use
   `event: "COMMENT"` and state the verdict as the body's first line
   (`**REQUEST_CHANGES** (posted as comment — own-PR limitation)` / `**APPROVE** (…)`),
   the established convention the merge condition reads.

```sh
cat > /tmp/review-pr<N>.json <<'EOF'
{ "body": "<gates run + results; tribunal seats run; P1 count; beads filed>",
  "event": "REQUEST_CHANGES",
  "comments": [ {"path": "...", "line": 42, "side": "RIGHT", "body": "**[blocking]** ..."} ] }
EOF
gh api repos/{owner}/{repo}/pulls/<N>/reviews --input /tmp/review-pr<N>.json
```

   (`line` must be a head-version line present in the diff, or the API rejects the review.)
2. **Post/update the adjudication record** as a PR comment from `ADJUDICATION.md` — the
   durable per-PR log of every finding, verdict, and disposition, including refuted items
   with their quoted refutations. This record is what kills re-raises; it dies with the PR.
3. **File every P2 as a bead now** (`bd create`, real dependency edges), and list the bead
   IDs in the review body.
4. **Dispatch ONE fix batch** — P1s plus any judge-designated ride-alongs (chiefly
   simplicity reductions; see the judge charter). Never dispatch a batch for sub-P1 items
   alone. Existing mechanics: `agent-send.sh` to a live worker session,
   `agent-spawn.sh <slug>` to revive one, or `agent-takeover.sh <N>` for out-of-band crate
   code. Docs/spec fixes the foreman makes directly.

## 6. The verify event (after the fix batch) — bounded

Seats: **Closer** (always) + **1–2 fresh sweep seats** chosen for where the fixes
concentrated, + a fresh judge.

- The Closer is the one seat with memory. Fetch the adjudication record into its worktree
  before composing its `AGENTS.md`:

```sh
gh pr view $PR --json comments -q \
  '[.comments[] | select(.body | startswith("## Adjudication record"))] | last | .body' \
  > "$WT/ADJUDICATION.md"
cat AGENTS.md .claude/skills/pr-review/seats/COMMON.md \
    .claude/skills/pr-review/seats/closer.md > "$WT/AGENTS.md"
```

- Sweep seats run their normal charters, blind as ever, scoped by the fix-touched surface.
- **Convergence rule: at the verify event only P1-bar findings may block; everything else
  parks automatically — no exceptions, no "while we're here."**

Outcomes:
- **No open P1s** ⇒ APPROVE; merge with parks (§7).
- **New P1s** ⇒ one more fix batch, then a **Closer-only re-check** (no sweeps, no
  tribunal), judged the same way.
- **P1s still open after that, or the same root cause bounced twice** ⇒ STOP. Escalate to
  the user with `DISPOSITION.md`. The stop is the default.

## 7. Merge and clean up

Merge conditions: green portable gates + any spec-required box gate green + zero open P1s.
Do not hold a green-box, tribunal-reviewed PR for extra passes. After any merge-conflict
resolution or rebase, run ONE contract-seat pass on the merged head — wire regressions
introduced by conflict resolution are a real, observed class. Then:

```sh
git -C ~/workspace/harmony worktree list | grep review-pr$PR   # remove each:
git -C ~/workspace/harmony worktree remove ../harmony-review-pr$PR-<seat>
tmux kill-session -t agent-judge-pr$PR 2>/dev/null
```

Report to the user: verdict, the P1s and their fixes, beads filed, adjudication link.

## Standing rules (unchanged by the tribunal)

- Never weaken a gate, floor, or spec to get a PR through — the Gate Auditor hunts exactly
  this; quality ratchets up, never drifts down.
- `unsafe` ⇒ Miri, with an interpreter-reachable path; a vacuous Miri gate is a finding.
- The spec is the contract; spec self-contradictions are `[question]`s for the integrator,
  never pinned on the implementer.
- Dependency-whitelist exceptions: ask-by-comment in the PR description.
- Both platforms must pass; check what Mac-side gates cannot see (cfg(linux)) is covered by
  Linux CI before approving anything touching shared enums or box code.
