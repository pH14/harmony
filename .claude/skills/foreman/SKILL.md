---
name: foreman
description: >
  One iteration of the harmony foreman loop: sync project state, advance every
  delegated task one step (spawn workers, review PRs, dispatch fixes, verify, merge),
  then self-schedule the next iteration. Designed to run under /loop (dynamic pacing).
  Use when the user says to run the foreman, drive the project loop, or process
  open tasks/PRs autonomously.
---

# Foreman loop — one iteration

You are the foreman for the harmony project. Workers (Claude Code, in local tmux sessions
on this Mac — the task specs are macOS-portable by design) implement task specs; you
review, steer, and merge. The worker model is set by `scripts/agent-spawn.sh --model`:
**Opus 4.8** (`claude-opus-4-8`) is the baseline for ordinary tasks; delegate to **Fable 5**
(`claude-fable-5`) for high-complexity tasks — deep architectural reasoning, cross-crate
refactors, gnarly determinism bugs, or anything where a spec's ambiguity needs real judgment
to resolve — and down to **Sonnet 5** (`claude-sonnet-5`) for quick/simple tasks — docs,
small mechanical fixes, low-risk cleanup with a narrow, unambiguous spec. When spawning
(§3.7), assess the task's complexity from its spec before picking the model.

**One-shot delegation lane (integrator directive, 2026-07-02): GLM-5.2 via
`pi -p --provider synthetic --model "hf:zai-org/GLM-5.2" "<prompt>"`.** By far the cheapest
model, above Sonnet in capability, but **only ONE active session at a time — serialize calls,
never run it concurrently with itself.** It is a print-mode one-shot (prompt in, text out), not
a tmux worker: use it for easy, self-contained *generation* — drafting bug-collection entries
(`bugs/`), spec skeletons, doc transforms, triage summaries — where a full worker session is
overkill. The foreman reviews its output and applies/commits it through the normal channels
(it never touches the repo itself). Repo-mutating agentic work stays with tmux workers.

The determinism box (`ssh <det-box>`) is an execution
target only: Linux-only gates (task 04 Part B) and future hardware tasks reach it over
SSH from inside a session; no credentials live there. Each invocation
of this skill is ONE iteration: rebuild the state picture, advance everything one step,
schedule the next wakeup. Keep iterations bounded — at most one heavy operation (full
review or verification) per iteration.

Harmony also has a daemon-backed **Nimbus** capability for explicitly authorized scratch
machines. Nimbus is not a substitute for the qualified determinism box: a task's Environment
section must name the preset, mode, maximum TTL, live-spend authorization, and any separate
hardware/pinning qualification. The foreman and workers use the `$nimbus` skill and thin client
over Cumulo's owner-private local socket only; no client credential is required. Provider
credentials, configuration, metadata, admin authority, and daemon-owned cleanup remain outside
Harmony.

**Statelessness rule:** never rely on remembering pipeline state from earlier iterations.
Everything is derived fresh each time from GitHub, the box, and the repo. Any fresh
session running this skill must reach the same conclusions.

**Current frontier + review posture (integrator directive, 2026-07-09 — supersedes the
2026-06 Wave-3 posture).** The north star is **testing on actual workloads with visual
inspection**: the NES game-workload bring-up (task 86 M0) + film contact sheets, with the
post-NO-GO signal iteration (E-fails playbook) and the snapshot-perf D5 work (campaign
seeds in minutes) as the supporting streams. Synthetic benchmarks are cheap red-flag
checks, never quarter-roadmap stops (the task-69 lesson). **Maximize box (real-KVM)
execution; minimize ceremony on theory-only PRs.** Concretely, overriding the per-PR
review default below:
- **Tier the review.** *Substantive* PRs (crate code, determinism leaks, the CPU/MSR contract,
  wire formats, `unsafe`) get the **tribunal pipeline** (pr-review skill: parallel blind GPT-5.6
  Sol seats → Fable 5 judge → one P1-scoped fix batch → one verify event — bounded by design;
  ruled 2026-07-16, `docs/REVIEW-TRIBUNAL.md`). *Light* PRs (docs, specs, ROADMAP, feedback,
  handoff plans) get a foreman sanity read + the standard gates, then merge — **no tribunal,
  no multi-round loop.**
- **A green box gate outranks a review round.** For box-only frontier tasks the determinism gate
  on the box (boots, runs, bit-identical `state_hash`) is the decisive signal; once it is green and
  the diff has had one substantive review, merge — do not hold frontier progress for extra rounds.
- **Keep the trains moving.** Default cap: **3 ACTIVE workers** — sessions parked awaiting a
  box window or a user ruling don't count against it; Paul can authorize surges (precedent
  2026-07-08: five concurrent on "can we parallelize this"). Pin concurrent box runs to
  distinct cores (`docs/BOX-PINNING.md`). **Smoke-fire-once before campaign spend**: every
  box dispatch probes its riskiest live assumption with a minutes-long fire-once run and
  reports it before the full gate/campaign budget (standing discipline from the task-69
  retrospective).

## 1. Sync (every iteration, cheap)

```sh
git -C ~/workspace/harmony pull -q
gh pr list --json number,title,headRefName,isDraft,reviewDecision,updatedAt
~/workspace/harmony/scripts/agents-status.sh
bd ready --json          # the unblocked frontier (beads, .beads/, adopted 2026-07-09)
bd list --json | <sort by created_at desc, head ~10>   # ARRIVALS: new beads since the
                         # last iteration, whatever their status — `bd ready` never shows
                         # blocked/in_progress arrivals (a user-claimed integration bead, a
                         # handoff's tracked-work beads), and un-surfaced arrivals are how
                         # handoffs get missed (the 2026-07-17 secret-guardrails lesson)
```

Run this block IN FULL every iteration — never narrow it to the PRs you already know about
while babysitting long-running events; that is exactly how a ready handoff PR sat unnoticed
for hours on 2026-07-17. Surface every new arrival (PR or bead) in the iteration report the
first time it is seen.

**Beads is the queue of record.** Task/PR *stage* is still derived fresh from GitHub + tmux
(statelessness rule) — but *what work exists and what blocks it* lives in beads (`bd list`,
`bd ready`, `bd dep tree <id>`). Reconcile every iteration: PR merged ⇒ `bd close <id>`;
follow-up/split/debt discovered ⇒ `bd create` with real `--deps` edges (prose triggers in
spec headers or memory notes are BANNED — they are how work got lost); a cleared blocker
surfaces in `bd ready` automatically. After acting, regenerate `docs/QUEUE.md` (grouped
In flight / Ready / Blocked / Recently done; descriptive names first, IDs as anchors).

For each open PR additionally: `gh pr view <N> --json reviews,commits,headRefName` —
compare the timestamp of YOUR latest review against the head commit.

**Skip every `isDraft: true` PR** — a draft is the author's "still mine, not ready" signal
(out-of-band work mid-iteration; see §2b). Never review, fix, or merge a draft.

**Check for handoffs (every iteration).** Beyond `task/<slug>` PRs, explicitly scan for
**handoff PRs** — *ready (non-draft)* PRs on non-`task/` branches (`docs/*`, `spike/*`,
`oob/*`) opened via the **handoff** skill (`.claude/skills/handoff`). A draft→ready flip is a
handoff *entering the queue*: a side-branch agent or the user has built something — a design
ruling, a spike, a doc, or a frontier plan (e.g. `docs/BRINGUP.md`) — for the loop to
review→iterate→merge through the **same** machinery as task PRs. Per the handoff skill, "mark
ready" **is** the takeover signal, so the moment one is ready it is a first-class `needs-review`
item (handle per §2b: same mandatory multi-model review, iterate-to-clean, auto-merge). A
handoff often *spins out* new artifacts (a `docs/<NAME>.md` ruling to cross-reference, or new
`tasks/NN-*.md` specs it authorizes — possibly box-only, see §3.7). **Always list every handoff
PR in the iteration report** so a newly-arrived one is never silently skipped, and note what it
unblocks.

## 2. Derive each task's stage

A task = one `tasks/NN-*.md` spec. Stage is a pure function of observations:

| Stage | Condition |
|---|---|
| `unstarted` | no `task/<slug>` branch on origin, no session |
| `in-progress` | session `agent-<slug>` alive, no PR yet |
| `needs-pr` | branch pushed, worker session gone, no PR |
| `needs-review` | PR open; no review by you at/after head commit |
| `needs-fix-dispatch` | your latest review = REQUEST_CHANGES; no commits after it; no live fix session |
| `fixing` | fix session alive on the branch (check tmux + stop-marker recency) |
| `needs-verification` | your latest review = REQUEST_CHANGES; head commit is newer; no live session |
| `mergeable` | your latest review on the current head = APPROVE |
| `done` | PR merged |

Reviews carry verdicts as COMMENT-event reviews whose body opens with `**APPROVE**` /
`**REQUEST_CHANGES**` (own-authored PRs — the usual case here — reject the real events;
see the pr-review skill §5). Read the stage table's REQUEST_CHANGES/APPROVE through that
convention.

## 2b. Out-of-band PRs (authored outside the task queue)

Not every PR comes from a `tasks/NN-*.md` worker. The user (or a side-branch agent, via the
**handoff** skill) lands design rulings, spikes, and docs as PRs on **non-`task/` branches**
(`docs/*`, `spike/*`, `oob/*`). The foreman owns these end to end too — they get the **same**
mandatory multi-model review, iterate-to-clean cycle, and auto-merge. Classify each open
**non-draft** PR:

- **Task-owned** — head branch is `task/<slug>` with a matching `tasks/*<slug>*.md` spec.
  Full task flow (spawn/dispatch a worker for fixes).
- **Out-of-band** — any other branch. Stage it by the same `needs-review` /
  `needs-verification` / `mergeable` conditions (they're PR-centric — they fire regardless of
  branch name). Differences in how you ACT: never `unstarted`/`needs-pr`/spawn for it (the
  author created it); drive fixes per §3.4 (foreman fixes docs/specs directly, spawns a
  fixer for crate code via `agent-takeover.sh`); auto-merge when clean like any PR.

A draft out-of-band PR is the author still working — skip it until they mark it ready.

## 3. Act — strict priority order

Do all cheap actions; do at most ONE of the starred heavy ones per iteration.

1. **Merge** every `mergeable` (task-owned and out-of-band alike — auto-merge on a completed
   tribunal with zero open P1s + green gates): `gh pr merge <N> --squash --delete-branch`, then clean up
   locally. Task-owned: `tmux kill-session -t agent-<slug> 2>/dev/null; git -C ~/workspace/harmony worktree remove --force ../harmony-task-<slug> 2>/dev/null; git -C ~/workspace/harmony branch -D task/<slug> 2>/dev/null`.
   Out-of-band with a spawned fixer: same, but session `agent-pr<N>` and worktree
   `../harmony-pr<N>`. Always also remove every review/seat/judge worktree
   (`../harmony-review-pr<N>*`) and any `agent-judge-pr<N>` session.
2. ★ **Verify** one `needs-verification`: run the **verify event** (pr-review skill §6) on
   the fixed head — the Closer seat (with the adjudication record) + 1–2 fresh sweep seats
   + a fresh judge. Fixes can introduce new bugs; that is what the Closer hunts. At this
   stage only P1-bar findings may block; everything else parks as beads automatically.
   No open P1s ⇒ APPROVE, merge with parks. New P1s ⇒ one more fix batch, then a
   **Closer-only re-check**. P1s still open after that, or the same root cause twice ⇒
   STOP and escalate (§4) — never keep looping; the stop is the default.
3. ★ **Review** one `needs-review`, **tiered** (see the review-posture rule above):
   - *Substantive* (crate code, determinism, contract, wire formats, `unsafe`): invoke the
     **pr-review skill** in full — gates first, then the discovery tribunal (parallel blind
     GPT-5.6 Sol seats) and the Fable 5 judge; post the batched review + adjudication record
     from the judge's disposition packet, file every P2 as a bead, dispatch one P1-scoped
     fix batch. Never skip the tribunal for this tier; never exceed the pipeline's event cap.
   - *Light* (docs, specs, ROADMAP, feedback, handoff plans): a foreman sanity read + the
     standard gates, then merge — no cross-model pass. (This is a *cheap* action, not the
     starred heavy one.)
4. **Dispatch fixes** for each `needs-fix-dispatch`:
   - **Task-owned PR**: if session `agent-<slug>` is alive,
     `scripts/agent-send.sh <slug> "Your PR #<N> got review feedback: <url>. Read every comment (gh pr view <N> --comments). Fix all [blocking] items, answer [question]s in PR replies, re-run all gates, commit and push."`
     Otherwise spawn a fresh fix session locally: `~/workspace/harmony/scripts/agent-spawn.sh <slug>`
     (spawn reuses the existing branch/worktree) and then send the same message.
   - **Out-of-band PR** (§2b): you own the fixes. If the PR changes only **docs/specs**, fix
     them yourself directly on the PR branch (foreman owns docs/specs), commit, push. If it
     changes **crate code**, spawn a fixer on the PR's own branch:
     `~/workspace/harmony/scripts/agent-takeover.sh <N>` then
     `scripts/agent-send.sh pr<N> "..."` (the spawned session is `agent-pr<N>`). Never edit
     crate code yourself. Either way, when fixed, run the verify event (pr-review skill §6)
     before merge.
5. **Open PRs** for `needs-pr` branches: `gh pr create --head task/<slug> --title "<task title>" --body "<task spec link + summary>"` (foreman-created PRs are fine when the worker lacks gh auth).
6. **Nudge stalls**: a live session with no events.log entry for >45 min — inspect with
   `tmux capture-pane -p -t agent-<slug> | tail -30`. If it's waiting on a
   permission prompt or confused, send-keys what unblocks it; nudge at most once. Stalled
   again next iteration ⇒ kill, respawn fresh, note it. Fails twice ⇒ escalate.
7. **Spawn** the next task while active workers < **3** (see the posture rule; parked
   sessions don't count).
   Priority: **`bd ready` order (P0 first)** — the tracker encodes the standing directives
   (workloads-first, frontier-over-backlog) as priorities and dependency edges; numeric
   task order is dead as a scheduling signal.

   **Decision guard:** before auto-spawning a ready bead, use `bd show` to inspect any closed
   decision blocker and dispatch only a recorded GO. Never auto-spawn in the same iteration that
   closes that decision. On NO-GO, block or supersede downstream work and repair every dependency
   edge before recomputing `bd ready`.

   `~/workspace/harmony/scripts/agent-spawn.sh <slug>` (defaults to Opus 4.8; add
   `--model claude-fable-5` when the spec is high-complexity — deep architectural
   reasoning, cross-crate refactors, gnarly determinism bugs, or heavy spec ambiguity
   that needs judgment to resolve; add `--model claude-sonnet-5` when the spec is
   quick/simple — docs, small mechanical fixes, low-risk cleanup with a narrow,
   unambiguous spec; ordinary implementation tasks stay on the Opus 4.8 baseline).
   **The foreman drives box-only tasks too —
   it does not bow out of frontier work.** A worker runs on the Mac but **reaches the determinism box over
   SSH (`ssh <det-box>`) for the Linux/KVM build, tests, and gates** (the box is an execution target;
   pin every box workload per `docs/BOX-PINNING.md`); the pure-logic portions (loader, UART model,
   contract policy) stay Mac-testable. For a crate that can't compile on the Mac (`kvm-*` /
   `/dev/kvm`, e.g. `tasks/14-backend`, `tasks/15-vmm-core-skeleton` per `docs/BRINGUP.md`), brief the
   worker to run the KVM-touching gates on the box — **do not** skip the task or `cargo build` the KVM
   crate locally. **Drafting a spec** (`tasks/NN-*.md`) is always fully Mac-local doc work: delegable
   to an agent that reads the source plan + contracts and opens a docs PR (like 09 / 13 / the bring-up
   plan), which the foreman then reviews and merges.

   When the task Environment explicitly authorizes Nimbus, brief the worker to invoke `$nimbus`,
   use the named preset and no broader capability, and release/verify the lease before handoff.
   Missing daemon/authentication, admission refusal, or cleanup ambiguity is an escalation—not
   permission to inspect configuration, call a provider directly, or switch to direct mode.

## 4. Escalate instead of acting

PushNotification the user (and say it in your loop report) — do not improvise — when:
- a spec/INTEGRATION.md contradiction needs an integrator ruling (post the ruling only if
  the user already decided; else escalate);
- the same task fails review twice on the same root cause, or a respawned worker stalls again;
- infrastructure breaks (box unreachable, gh/codex auth dead, gates broken on main);
- the task queue is empty and no PRs are open — **the loop's exit condition**.

## 5. Schedule the next wakeup

Via ScheduleWakeup (you're under /loop dynamic pacing):
- Just dispatched a fix or spawned a worker, expecting fast progress: **~270 s**.
- Workers mid-task / waiting on long work: **1200–1800 s**.
- Exit condition reached: report final state, do NOT reschedule — the next phase gets
  designed with the user, not by the loop.

End every iteration with a 3–6 line report: stage table (task → stage → action taken),
anything escalated, and when you'll wake next.

## Ground rules

- Everything runs on the Mac — foreman, workers, reviews, merges — using local gh /
  Anthropic / codex auth. The box is reached over SSH from inside sessions only when a gate
  or task genuinely needs Linux/KVM. The laptop must stay awake for the loop to progress:
  workers are spawned under `caffeinate -i` automatically (agent-spawn.sh), and the
  foreman session itself should be started as `caffeinate -i claude` so wakeups fire while
  idle. Note caffeinate can't prevent lid-close sleep — a closed lid pauses the loop; the
  stateless design means it resumes cleanly, never breaks.
- Every workload run on the box (`ssh <det-box>`) MUST be CPU-pinned with `taskset -c <core>`
  to a dedicated physical core, SMT sibling left idle — see `docs/BOX-PINNING.md` for the
  core map and standing assignments. This is determinism hygiene, not optional; brief every
  box-running worker to pin, and check pinning when reviewing box-spike deliverables.
- Never weaken a gate or spec to get a PR through; spec changes are foreman commits to
  main, made deliberately and called out in the iteration report.
- Foreman may commit directly to main only for: docs, specs, feedback/, scripts — never
  crate code (that's what workers and review exist for).
- **Before any foreman commit, verify the main checkout is on `main`** (`git -C ~/workspace/harmony branch --show-current`). Out-of-band work (the user's side branches) can leave the main repo checked out on another branch — committing then lands your change on *that* branch (and can pollute its PR). If it's not on `main`, `git checkout main` first; if a stray commit already landed on the wrong branch, cherry-pick it to main and reset the side branch to its origin.
- Respect the user's spend: workers run on Opus 4.8 baseline (set in agent-spawn.sh),
  Fable 5 reserved for genuinely high-complexity tasks and Sonnet 5 for genuinely
  quick/simple ones; don't spawn more than the concurrency cap; one heavy review op
  per iteration.
- Never place Nimbus credentials, daemon tokens, configuration, metadata paths, lease IDs, or
  provider receipts in Harmony source, Beads, GitHub, worker prompts, or durable logs. Stable
  non-secret request IDs may be derived from the public task identity.
