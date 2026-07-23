---
name: harmony-pr-review
description: Draft the next opposing-vendor Harmony PR-review workflow. Use when explicitly evaluating or exercising proposed provenance-based Anthropic/OpenAI reviewer routing for substantive code, determinism, contract, wire-format, unsafe, gate, CI, or fix-verification work.
---

# Harmony PR review

**Status: draft.** The current tribunal ruling remains binding for live reviews until the user
explicitly adopts this replacement.

Preserve the binding tribunal's event shape and severity bar while proposing that reviewer
selection depend on implementation provenance rather than fixed model names. Read
`docs/REVIEW-TRIBUNAL.md` before exercising the draft.

## Route by provenance

Read the `harmony-provenance` marker from the PR body and confirm it against known dispatch
history. Select the discovery reviewer vendor:

| Implementation | Discovery reviewer |
|---|---|
| Anthropic only | OpenAI, defaulting to the configured Sol-class model |
| OpenAI only | Anthropic, defaulting to the configured Fable-class model |
| Human only | Configured default; use both vendors for unusually risky work |
| Anthropic + OpenAI | Fresh panels from both; disclose that strict vendor opposition is unavailable |

Do not silently fall back to the implementation vendor. If the opposing vendor is
unavailable, stop or obtain an explicit user ruling.

Pin the chosen model and reasoning effort on every invocation. Never inherit a user-level
default for a load-bearing review property.

## Tier and bound

- Light docs/spec/roadmap/handoff work: coordinator sanity read plus applicable gates.
- Substantive code, determinism, contract/wire, unsafe, gate, or CI work: tribunal.
- Discovery: 5 seats under ~1k meaningful LOC, 6 seats around 1k-5k, 7 seats above 5k or
  for a new crate/artifact. Split oversized PRs by spec milestone when possible.
- After one P1-scoped fix batch: Closer plus 1-2 fresh sweep seats and a fresh judge.
- Permit at most one Closer-only re-check, then escalate.

## Prepare context and gates

1. Fetch PR metadata, task spec, `tasks/00-CONVENTIONS.md`, prior feedback, and
   `docs/INTEGRATION.md` when cross-component.
2. Create detached worktrees from the remote head and remote base.
3. Run applicable build, test, clippy, fmt, platform, box, and Miri gates before reading.
4. Treat a red required gate as P1. Record exact commands and output.
5. Give seats the diff, tree, spec, conventions, and their charter—but not the PR body.

Use the existing seat charters in `references/seats/`. They are procedures, not personas.
Every seat must report exact `file:line`, mechanism, concrete trigger, wrong outcome,
confidence, and at most eight findings. A clean report is valid.

## Run vendor-specific seats

The orchestration interface is artifact-based: each seat receives a charter and isolated
worktree, then produces a report file. The transport may differ:

- OpenAI seats may use `codex review` or a native Codex subagent. Pass the charter
  explicitly and pin model plus effort.
- Anthropic seats may use interactive Claude Code subscription sessions in isolated tmux
  panes. Instruct each session to write its report artifact and stop; do not require
  print/headless API mode.

Preserve blindness and fresh context regardless of transport. Do not scrape terminal prose
as the authoritative result when the runner can write an artifact.

## Judge and disposition

Use a fresh judge context. Prefer a model family independent of the finder when that does
not defeat the required worker-review vendor opposition. The judge must cite-or-reject,
deduplicate by mechanism, verify load-bearing claims from code or execution, classify each
as CONFIRMED/PLAUSIBLE/REFUTED, and emit `DISPOSITION.md` plus `ADJUDICATION.md`.

P1 means the PR must not merge: red gate, determinism leak, contract/wire violation,
green-on-failure evidence hole, data loss/corruption, or unsound unsafe. File P2 findings as
Beads rather than extending the fix loop.

Post one batched review and the adjudication record. Dispatch one P1 fix batch. At verify,
only P1-bar findings may block. Merge with zero open P1s and green required gates; otherwise
follow the bounded escalation rule.
