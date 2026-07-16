# Task 108 — ARCH-BOUNDARY restructure, steps 1–4: the compiler-enforced arch seam + engine/vendor split

**Bead:** `hm-b5n` (P1). **Binding spec:** `docs/ARCH-BOUNDARY.md` — the ruled seam design
(2026-07-03) and its §Sequencing steps 1–4. This file is the dispatch wrapper: surface list,
gates, and the constraints that post-date the ruling. **Dispatch authority:** the pre-build
ruling (Paul, 2026-07-13; `docs/ARCH-BOUNDARY.md` §Pre-build ruling) — this restructure is
queue item 1, the enabler for both vendor lanes.

Read first, in full: `docs/ARCH-BOUNDARY.md` (the design you are executing — the audit, the
two-level `Exit` ruling and its rejected alternatives, §B's engine/vendor table, §C's upstream
fixes, the risks list), `docs/R-BACKEND.md` (the substrate seam whose default-deny invariant
you are extending, not weakening), `docs/GLOSSARY.md` (naming authority), and
`tasks/00-CONVENTIONS.md`.

## The four steps (one branch, four clean step-commits, gates green at each boundary)

1. **C-list neutralizations** (`docs/ARCH-BOUNDARY.md` §C + the two §A neutralizations):
   `HypercallRegs{rax..rdx}` → `HypercallFrame{args: [u64; 4]}` (fixes
   `complete_hypercall(rax)` too); `Exit::Hlt` → `Exit::Idle`; widen
   `HostFault::InjectInterrupt { vector: u8 }` (environment codec + the explorer demo
   constant — the only dissonance code change in the whole task);
   `CrashKind::TripleFault` → a portable crash-taxonomy name; `VClockConfig::{tsc_hz,
   tsc_base}` → guest-clock naming (naming-only; you are "next touching" vtime, so the §C
   proviso fires); `campaign-runner/main.rs` box-mode defaults (`bzImage`, x86 cmdline,
   `BackendKind`) become per-arch config data.
2. **Mechanical extraction** of the x86 value types into an arch module inside
   `vmm-backend` — no semantics change, all gates green.
3. **The keystone**: the `Arch` trait (associated types per §A) + generic `Backend` +
   the engine/vendor module split in `vmm-core` per §B's table, x86 as the sole vendor,
   every existing portable gate passing unchanged through it.
4. **`vm-state` arch-tagged records + `VM_STATE_VERSION` bump** — versioned wire evolution,
   never silent reinterpretation; goldens updated deliberately.

## Surface list (frontier-class boundary; touch nothing outside it)

`consonance/vmm-backend`, `consonance/vmm-core`, `consonance/vm-state`,
`consonance/environment` (vector widening + codec), `consonance/control-proto` (CrashKind
rename only), `consonance/vtime` (the §C naming rename only), `dissonance/explorer`
(adapter demo constant only), `dissonance/campaign-runner` (main.rs config data only),
plus goldens/snapshots those crates own. `consonance/lapic` is **not** restructured (its
seam shape is already ruled correct); only mechanical fallout (renamed types/imports) may
touch it.

## Constraints (binding)

- **Naming**: "**vendor**" replaces "personality" in every new identifier/module/doc-string
  (2026-07-12 ruling; ARCH-BOUNDARY's prose predates it — follow the ruling). GLOSSARY
  vocabulary throughout (Subject, Moment/Span, Reproducer). **No rename residue**: never
  write "(formerly X)" / "renamed from" comments — code reads as if the new names were
  always the names (Paul, 2026-07-13); why-comments stay.
- **Module split, not crate split.** The reserved engine/vendor *crate* names stay reserved
  (they activate with the ARM window). No new crates; the boundary is the trait + module
  lines.
- **Default-deny stays structural**: two-level `Exit` exactly as ruled (a superset enum or
  opaque exit is a rejected alternative, not a simplification you may re-litigate); each
  arch's exit enum exhaustively matched by that arch's own dispatch — no wildcard arms over
  arch exits.
- **Generics stop at vmm-core**: `A` is an associated-types trait; vendor structs are ZSTs;
  a `<A: Arch>` parameter (or any arch generic) appearing in a dissonance crate or
  control-proto is a review-blocking defect. Nothing above vmm-core goes generic.
- **The trait is designed, NOT frozen**: `run_until_overflow`'s late-only-stop contract
  stays exactly as-is; the AA-3 trait-freeze memo (ARM spike) owns the freeze decision.
  Leave the freeze question as a doc note where the trait is defined; do not pre-solve it.
- **State-hash canonical form must survive the record-set refactor** — this is the named
  risk the determinism gates exist to catch. Same seed ⇒ same `state_hash` before and after
  every step (except where step 4's version bump *deliberately* changes encoded bytes —
  call that out explicitly in the PR, with the golden updates isolated in the step-4
  commit).
- On completion, update `docs/ARCH-BOUNDARY.md`'s status line (steps 1–4 landed; D-list and
  freeze state unchanged) in the same PR.

## Gates

- Full portable suite for **every touched crate**: `cargo build`, `cargo nextest run`,
  `cargo clippy --all-features --all-targets -- -D warnings`, `cargo fmt -- --check`,
  `cargo deny check`.
- **Cross-target clippy** for the Linux side: `cargo clippy --target
  x86_64-unknown-linux-gnu --all-features -- -D warnings` for vmm-backend/vmm-core —
  Mac-only gates cannot see `cfg(linux)` breakage and that exact gap broke main for three
  PRs once; treat it as mandatory.
- **Miri** for the crates with Miri jobs (vmm-core per quality.yml; reduced-case discipline
  per conventions). Moved `unsafe` keeps its `// SAFETY:` comments; no new `unsafe`
  expected.
- **public-api snapshots** regenerated for touched crates that have them — the API *will*
  change; the snapshot diff is the reviewable record of exactly how.
- det-corpus / determinism suites for the touched crates (the state-hash stability check).

## Environment

Everything above is Mac-portable. **Do not touch the determinism box**: it is under the
nested-x86 re-certification lock. The keystone's "every box gate passing unchanged" is
verified by the foreman in the post-re-cert window before merge — state box-gate readiness
(what to run, expected result) in your IMPLEMENTATION.md instead of running it.

The open PR #98 (spike/nested-x86) lives in `spikes/` + docs and does not overlap this
surface; do not rebase-chase it.

Done = four step-commits on `task/arch-boundary-restructure`, all gates above green, x86 the
sole vendor behind a compiler-enforced seam, IMPLEMENTATION.md with the box-gate handoff
note, and the ARCH-BOUNDARY status line updated.
