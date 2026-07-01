# Task 94 — rename the two loops: **Modulation** (inner) / **Progression** (outer)

> **DEFERRED CHORE · queue after the Wave-4 keystones (58–61).** The two exploration loops
> currently have **three** competing vocabularies: `docs/DISSONANCE.md` says **Variation/Theme**,
> `tasks/12-explorer.md` and all of `dissonance/explorer`'s code say **Timeline/Multiverse**
> (`engine.rs` `timeline()`/`multiverse_step()`), and readers bounce between them. The integrator
> has ruled the final names:
>
> - **Modulation** — the inner loop: one run under one environment (was Variation / Timeline).
> - **Progression** — the outer loop: the search across runs (was Theme / Multiverse).
>
> One sweep unifies docs, task specs, and code to these two names. Musical fit is intentional:
> a *modulation* is one in-flight change of key; a *progression* is the sequence that moves the
> piece forward.
>
> **Sequencing:** land when the `dissonance/explorer`-touching queue is clear (this renames the
> crate's public API, which task 58's adapter and task 60's campaign bin consume — coordinate,
> task-90/43 precedent). Pure rename; zero behavior change.

## Surface

1. **Code (`dissonance/explorer`, plus any 58/60 consumers):** rename public types/methods —
   `timeline()` → `modulation()`, `multiverse_step()` → `progression_step()`, and every
   `Timeline`/`Multiverse`-named type, module, error variant, and doc comment. Update the
   `public_api.txt` golden (quality-d) in the same PR. **Determinism-neutral by construction —
   verify, don't assume:** grep that no renamed identifier reaches wire bytes, hashes, or
   goldens (control-proto frames and `Bug` fingerprints carry no loop names today; assert it
   stays true).
2. **Docs:** `docs/DISSONANCE.md` (the "two loops" section + table + every use),
   `dissonance/README.md`, `docs/REVIEW-2026-07.md`, ROADMAP if it names the loops.
3. **Task specs:** live references in open/future specs (58/60/61/62 as merged). Historical
   specs (12, 24, 25, 45, 93) keep their original vocabulary **with a one-line historical note
   at the top of task 12** (task-90 precedent: history is a record, not a lie to maintain).
4. **Terminology table:** add a short "naming history" footnote to DISSONANCE.md
   (Variation/Timeline → Modulation; Theme/Multiverse → Progression) so old PR discussions stay
   decodable.

## Acceptance gates

1. `git grep -niE 'variation|theme|timeline|multiverse'` over `docs/ dissonance/ consonance/`
   returns only the DISSONANCE.md naming-history footnote, historical task specs, and
   incidental uses of the words in unrelated meaning (each verified by eye and listed in the PR
   description).
2. Standard suite green on every touched crate; `public_api.txt` updated; **no golden or hash
   changes** (gate: existing `live_*` and corpus goldens byte-identical).
3. Zero behavior change: `cargo nextest run` pass-list identical before/after (rename-only
   diff).

## Non-goals

Renaming anything else (Environment, Moment, the verb set are settled); reorganizing explorer
internals (the corpus redesign belongs to the SDK/coverage epoch, not a rename PR).
