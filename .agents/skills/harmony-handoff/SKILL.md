---
name: harmony-handoff
description: Draft the next provider-neutral Harmony handoff workflow. Use when explicitly evaluating or exercising the proposed grounded draft-PR handoff, implementation-vendor provenance marker, and cross-vendor coordinator takeover.
---

# Harmony handoff

**Status: draft.** Do not alter a live PR under this workflow unless the user explicitly
invokes or adopts it.

Prepare work performed outside the normal task queue for `$harmony-coordinator`.

## Preserve ownership

Use a non-`task/` branch: `docs/<topic>`, `spike/<topic>`, or `oob/<topic>`. Open the PR as a
draft so the coordinator does not take over while the author is still changing it.

Associate the work with either:

- a Bead or future task spec, including real dependency edges; or
- a documented decision point with evidence, constraints, and the work it unblocks.

Do not create a second task list in the PR description.

## Record provenance

Include exactly one machine-readable marker in the PR body:

```html
<!-- harmony-provenance: implementation_vendors=anthropic models=claude-opus-4-8 -->
```

Allowed vendor values are `anthropic`, `openai`, `human`, or a comma-separated set. Record
the models that materially authored implementation code. Update the marker when a fixer from
a new vendor materially changes the implementation. Documentation-only edits do not change
implementation provenance unless the PR itself is a documentation deliverable.

Never guess provenance. Use `human` when no model authored the work and disclose `unknown`
in prose when the history cannot be established.

## Ground the review

The PR body must state:

- what the change is;
- the governing spec, Bead, or decision;
- evidence and measurement method where relevant;
- what it unblocks or refutes;
- cross-PR dependencies or conflicts;
- determinism implications;
- gates already run and their results.

The description is coordinator and judge input. Blind review seats must not receive author
framing.

## Transfer control

When the work and local gates are complete, mark the PR ready. That transition is the
takeover signal. From then on, the coordinator owns review routing, fixes, verification, and
merge. Coordinate through durable PR comments and answer explicit questions.
