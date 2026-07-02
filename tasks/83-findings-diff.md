# Task 83 — findings: the run-over-run behavioral diff (the push surface)

> **DELEGABLE (with named sibling deps) · queued after tasks 60 + 64.** The push surface of
> `docs/RESOLUTION.md`: not a crash list, a **diff of the system's behavior from one campaign to
> the next** — New / Resolved / Ongoing / Rare findings framed as calls to action, every one
> stamped with a copyable `MomentRef`. This is the front page; the task-29 telemetry console
> becomes the per-run drill-down beneath it (re-homed, not rebuilt — no console work in this
> task). Pure logic over serialized campaign artifacts; no VM, no socket.
>
> Depends on **task 64** (the `RunTrace`/`Bug` vocabulary), **task 66** (the declared-signal
> catalog + never-fired detection — the property/rarity finding kinds need the matcher stack;
> the crash-bucket diff degrades gracefully without it), and **task 60** (a real campaign
> producing artifacts worth diffing). Sequence after all three.

Read first: `tasks/00-CONVENTIONS.md`, `docs/RESOLUTION.md` ("The human layer", the sequencing
table), `docs/EXPLORATION.md` ("RunTrace", the matcher DSL's declared-catalog =
never-fired-detection rule, §Triage's stable-coordinates dedup ruling),
`tasks/64-explorer-spine-refactor.md` (the vocabulary), `tasks/82-resolution-crate.md`
(`MomentRef`), `tasks/29-telemetry-console.md` (the per-run pane this report links into).

**Dependency grant (hard rule 2 exception, explicit):** `dissonance/explorer` (the
`RunTrace`/`Bug`/catalog vocabulary — defined there by task 64), `dissonance/resolution`
(`MomentRef`), `dissonance/environment`. This crate is a pure consumer of those types.

## Environment

Pure-logic, macOS + Linux, laptop-gated. Inputs are on-disk campaign artifacts (the
artifact-shaped supervisory seam of `docs/RESOLUTION.md`); tests run over committed synthetic
fixtures. No box gate.

## What to build

One new crate `dissonance/findings`: a library + `findings` bin (`required-features = ["cli"]`)
that takes N ≥ 1 campaign artifact directories (ordered) and emits a deterministic report.

### 1. The finding model

A finding's **identity is stable coordinates, never learned cells or stack hashes**
(`docs/EXPLORATION.md` §Triage, Klees et al.): v1 identity = `Bug.fingerprint` for bugs and the
declared signal id for property findings. Finding kinds, diffed campaign-over-campaign:

- **bug** — an `Oracle` verdict (`Bug`): present/absent per campaign.
- **property** — a declared signal (the config's declared set *is* the catalog): an
  `assert_sometimes`/`sometimes`-role signal that **never fired** is a finding; one that fired
  is coverage. A `never`/always-role violation is a bug.
- **rarity** — a declared signal whose fire-count sits below a configured threshold across the
  campaign (the "Rare" bucket: things that happen, but barely — checkpoint candidates).

### 2. The diff

Between consecutive campaigns: **New** (present now, absent before — regression call-to-action),
**Resolved** (absent now, present before — "an outstanding issue may have been fixed; verify"),
**Ongoing**, **Rare**. First campaign: everything is New. Ordering and bucketing are total and
deterministic (BTree everything; no HashMap iteration reaches output — hard rule 4).

### 3. The artifacts

- `findings.json` — versioned, machine-first (the agent's input for epoch-loop triage).
- `findings.md` — the human rendering: buckets in severity order, each finding with its
  identity, history sparkline (fired-count per campaign), and — for anything with a
  reproducing run — the **`MomentRef`** of the terminal moment, ready to paste into a
  task-82 `resolution` session (the copy-a-moment workflow).

Both byte-deterministic given the same inputs.

## Acceptance gates

1. **Standard suite** green (build / nextest / clippy `-D warnings` / fmt / deny),
   all-features, macOS + Linux.
2. **Golden reports** over committed synthetic fixtures covering: first campaign (all New), a
   fix (Resolved), a regression (New), a flapping finding (Resolved → New), never-fired
   detection, and the Rare threshold boundary.
3. **Proptests (≥256):** diff algebra — every finding lands in exactly one bucket;
   New/Resolved are inverses under campaign-order swap; report bytes identical across repeated
   runs and input-directory listing orders.
4. **Vocabulary lockstep:** consumes task 64's types as-is; no parallel definitions (a
   compile-time dependency, not a copy).

## Non-goals

- Rendering the per-run detail view (task 29's console owns it; this report *links* by
  `MomentRef`); triage automation (ddmin/bisection/LDFI — resolution-side, later);
  full stable-coordinates dedup (necessary-fault set + divergence bracket — arrives with the
  triage suite; v1 identity is fingerprint + signal id, and the identity field is versioned so
  the upgrade is additive); any web UI or serving; campaign scheduling.
