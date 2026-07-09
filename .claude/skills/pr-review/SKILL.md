---
name: pr-review
description: >
  Review a pull request for the harmony project and post inline comments on it.
  Use this whenever the user asks to review a PR, look over a task branch, check a
  delegated agent's work, or give feedback on a change — even if they just say
  "take a look at PR 3" or name a task branch like task-02-snapshot-store. Covers
  finding the right task spec, running the gates, a cross-model second pass (codex /
  GPT-5.6 Sol), and posting the review via gh.
---

# PR review

PRs in this repo are produced by delegated agents, each implementing one crate against a
written spec. The review's job is to check the work against **that spec and the project
conventions** — not generic code review taste. Most real problems here are contract
violations (wrong public API), determinism leaks, or gates that don't actually pass, so
spend your effort there.

## 1. Gather context before reading any code

Read these in order; they tell you what "correct" means for this PR:

1. **PR metadata**: `gh pr view <n> --json title,body,headRefName,files,url`. Note any
   dependency-whitelist exceptions requested in the description (conventions rule 5 allows
   ask-by-comment) and any linked issues (`Closes #N` → `gh issue view N`).
2. **The task spec**: branch/title names the task (e.g. `task-01-hypercall-proto` →
   `tasks/01-hypercall-proto.md`). Read it fully — especially the Public API section
   (a contract: exact names, types, semantics) and any task-specific gates.
3. **`tasks/00-CONVENTIONS.md`**: the hard rules every PR must satisfy. Re-read it each
   review rather than working from memory; it changes.
4. **Prior feedback**: check `feedback/` for earlier reviews touching this task or its
   plan. Don't re-litigate points already resolved there, and do check that accepted
   feedback was actually applied.
5. **`docs/INTEGRATION.md`** whenever the PR touches anything cross-component: traits
   other crates will implement, wire formats, magic constants, ABI registers. Cross-check
   every shared constant against both the task spec and INTEGRATION.md — specs written in
   parallel contradict each other, and a reviewer comparing documents is the first place
   such a contradiction can be caught. A spec self-contradiction is a `[question]` for
   the integrator, not a flaw to pin on the implementer.

## 2. Check out the code locally

Review from a local checkout, not from `gh pr diff` alone. The diff shows you what
changed; the checkout lets you run the gates and read whole files — and since these PRs
add entire new crates, the "diff" is the whole crate anyway. Use a worktree (the main
checkout must stay untouched, same as for implementers):

```sh
git -C ~/workspace/harmony fetch origin
git -C ~/workspace/harmony worktree add --detach ../harmony-review-pr<N> origin/<head-branch>
```

Detach matters: the implementer's worktree often still has the task branch checked out,
and git refuses to check out one branch in two worktrees. Detaching at the fetched head
commit sidesteps that and pins exactly what you're reviewing.

Use `gh pr diff <n>` only as an orientation pass and to find the diff line numbers you'll
need for inline comments.

## 3. Run the gates

Findings from running the code outrank findings from reading it. Run the standard gates
plus any task-specific ones from the spec:

```sh
cargo build -p <crate> --all-features
cargo test  -p <crate> --all-features          # note runtime; budget is ~3 min
cargo clippy -p <crate> --all-features --all-targets -- -D warnings
cargo fmt -p <crate> -- --check
```

**If the crate contains `unsafe`, also run Miri** (the unsafe⇒Miri review-bar rule, AGENTS.md):

```sh
# pinned nightly + MIRIFLAGS match .github/workflows/quality.yml's `miri` job
MIRIFLAGS=-Zmiri-permissive-provenance \
  cargo +nightly-2026-06-16 miri test -p <crate>
```

A Miri error is a blocking finding even when every behavioral test passes — Miri sees UB
(out-of-bounds reads returning plausible bytes, provenance violations, aliasing) that
value/panic assertions cannot. `grep -rl 'unsafe' consonance/<crate>/src` tells you whether it
applies. If the crate adds `unsafe` but has no Miri-exercisable test path (the asm/privileged
bits must sit behind a seam so the unsafe logic runs under the interpreter), that gap is
itself a finding.

A red gate is automatically a blocking finding — quote the failing output in the comment.
You're on macOS; if a failure looks platform-specific, that's itself a finding
(portability is rule 6, both platforms must pass).

## 4. What to review, in priority order

1. **Contract conformance** — diff the implemented public API against the spec's Public
   API section item by item. Renames, changed signatures, or semantic drift are blocking
   even when the new shape is arguably better; other workers are building against the
   spec, not this crate.
2. **Determinism discipline** — the reason this project exists. Grep is effective here:
   `HashMap`/`HashSet` iteration that can reach output/hashes/encoded bytes, floating
   point in state-affecting code, wall-clock time, unseeded randomness, `unwrap`/`expect`
   or panics reachable from untrusted input.
3. **Scope and isolation** — touched only its own directory (`gh pr view --json files`),
   no edits to root `Cargo.toml`, no dependencies on sibling crates, no invented shared
   crates. Dependencies outside the whitelist need an explicit ask in the PR description.
4. **`unsafe`** — only if the task file grants it, only for the named purpose, every
   block carrying a `// SAFETY:` comment that actually justifies it. **Any crate with
   `unsafe` must run clean under Miri** — run `cargo +nightly miri test -p <crate>` (§3) and
   treat a Miri error as blocking. Reading a `// SAFETY:` comment is not a substitute for
   running Miri: the comment asserts soundness, Miri checks it. Confirm the unsafe logic is
   actually *reachable* under the interpreter (asm/privileged paths behind a seam, exercised
   by loopback/in-process tests) — an `unsafe` crate whose pointer code Miri never executes
   has a vacuous Miri gate, which is a finding.
5. **Quality-tooling sufficiency — is quality slipping?** The repo has an excellent quality
   toolchain (`docs/CODE-QUALITY.md`); **a green gate is the floor, not the bar.** For the
   code this PR adds, check it actually *uses* the right tools — and that the suite isn't
   quietly degrading:
   - **Coverage** (`cargo-llvm-cov`, region floor): new logic is genuinely *exercised*, not
     merely executed; the region floor holds and **ratchets up** where the PR earns it —
     never slips. New uncovered branches, or a lowered floor, are findings.
   - **Mutation** (`cargo-mutants --in-diff`): would a mutation in the new logic be *killed*?
     The gate checks the diff, but read the new tests — do they pin **exact** values/behavior
     (what mutation testing rewards) or just loose properties a mutant survives? Missing
     exact-count/exact-value assertions on new arithmetic or counters is a finding.
   - **Property / stateful tests** (`proptest` ≥256 cases; `proptest-state-machine`): any new
     state machine, codec, or invariant-bearing logic wants a property or **stateful** test
     against an **independent** reference model (a re-derivation, not a mirror of the impl),
     not happy-path unit tests.
   - **Proofs** (`Kani`): new saturating/overflowing/bit-twiddling arithmetic, or a safety
     invariant over bounded inputs, is a proof candidate — flag if it's only sampled.
   - **Fuzzing** (`cargo-fuzz` + `arbitrary`, the Tier-1 backlog): a new parser/decoder over
     untrusted bytes (anything shaped like `hypercall-proto::decode`) should have fuzz or
     adversarial-property coverage — call it out if absent.
   - **Public-API snapshot** (`cargo-public-api`): an *intended* API change must update the
     committed snapshot; an *unintended* surface change is a finding.
   Does the suite, taken together, catch the bugs you looked for in (1) and (2)?
6. **Docs and handoff** — public items documented, crate-level doc comment,
   `IMPLEMENTATION.md` present and honest about deviations/limitations.

Read tests with the same suspicion as the code: a delegated agent under gate pressure is
tempted to weaken a test, relax a gate, or lower a floor instead of fixing a bug. Quality
should **ratchet up** across PRs, never drift down — a PR that loosens a lint, drops a floor,
or skips a tool the code plainly calls for is a blocking/question finding, not a nit.

**Verify behavioral findings before reporting them.** For anything you'd mark blocking
based on reading the code, write a quick repro test in the review worktree and run it;
quote the observed behavior in the comment ("dispatching `&req[..len-5]` returns
`(0,0,0)`, spec requires `(1,1,77)`"). A confirmed repro turns an argument into a fact —
and sometimes the code is right and your reading was wrong. Delete repro tests before
removing the worktree.

**Then make a second, targeted pass** over the two places review experience shows
first-pass reading misses real bugs:

- *State save/restore and snapshot paths*: does restore reject every state that save
  could never have produced (degenerate values that brick a stream or violate
  invariants)? Is a failed restore atomic, or does it leave the object half-mutated?
  Does round-trip equality actually hold?
- *Trust boundaries*: every length, index, or enum that arrives from the transport, the
  host, or a decoded frame — follow it to where it's used. Unchecked slicing or
  arithmetic on such a value is a panic reachable from untrusted input (rule 4), even
  when the happy-path tests all pass.

## 5. Cross-model second pass (MANDATORY)

Single-reviewer variance is real and large — your own read misses things a different model
catches. The cross-model pass is **`codex review` (GPT-5.6 Sol)**: a model different from the one
doing the primary review, run over the same diff. In a live calibration on the CPU/MSR
contract it independently caught real blocking findings the primary read missed (a
backend-mechanism gap — instructions the contract says it traps that stock KVM has no
userspace exit for — and a TOML-grammar violation). **This pass is mandatory. Never skip it,
and never merge a PR without a clean codex pass.** Launch it **blind** —
don't seed it with your own findings; an anchored reviewer re-treads your path, an
independent one covers what you missed.

### Primary: `codex review` (reliable, agentic, near-zero setup)

`codex review` is OpenAI's native non-interactive reviewer hitting GPT-5.6 Sol directly
(model pinned in `~/.codex/config.toml`; integrator switched 5.5 → 5.6 Sol 2026-07-09) — it
reads the repo and runs tools itself, so there's no worktree-inlining dance and no headless
stall. Run it from a detached PR-head worktree against `main`:

```sh
git -C ~/workspace/harmony worktree add --detach ../harmony-review-pr<N> origin/<head-branch>
cd ../harmony-review-pr<N>
gtimeout 1200 codex review --base main \
  -c approval_policy='"never"' -c sandbox_mode='"workspace-write"' \
  > /tmp/codex-review-pr<N>.md 2>&1
```

- `--base main` reviews the branch-vs-main diff; codex pulls and reads it agentically (it
  also opens whole files and runs `git`/`rg`/`cargo`), so whole-artifact completeness checks
  work even though it's diff-anchored.
- `codex review` will **not** accept a custom positional prompt together with `--base`
  (mutually exclusive) — project review focus lives in the repo's `AGENTS.md`, which codex
  auto-reads. Keep `AGENTS.md` current; it's what makes the review determinism-aware rather
  than generic.
- `approval_policy=never` + `sandbox_mode=workspace-write` (workdir + /tmp only) — it can
  build/test but can't escape or hang on a prompt. The worktree is disposable.
- xhigh is codex's default effort here and completes reliably.
- Output is verbose (an execution trace); the findings are the final `codex` block. Extract
  with `grep -nE '\[P[0-9]\]' /tmp/codex-review-pr<N>.md` and read the tail. Map codex
  severities P1→blocking, P2→suggestion (or blocking by your judgment), P3→nit.

### High-stakes artifacts

For the determinism contract, security-critical crates, or anything where a missed leak is
expensive, give the codex pass extra scrutiny: read its full reasoning (not just the `[P]`
findings), run it from a clean PR-head worktree so it can build/test itself, and verify each
behavioral finding with a repro in the worktree before it enters the review — treat its
output as leads, not verdicts, and drop what doesn't hold up.

### Re-review after significant findings (mandatory)

If the codex pass produced **any blocking finding**, then
after the author fixes them you MUST run the cross-model pass **again** on the updated PR
head, and keep iterating until a cross-model pass returns with **no blocking findings**.
Fixes introduce new code and can introduce new bugs; a spec or contract with many findings
is rarely correct after one round. A PR is mergeable only once a *clean* cross-model pass
confirms the fixed state — never merge on the strength of the pre-fix review alone.

## 6. Post the review

Post one review with all inline comments batched, not comment-by-comment. Prefix each
comment with a severity so the author can triage:

- `**[blocking]**` — must fix before merge (contract, determinism, red gate, scope)
- `**[suggestion]**` — worth doing, author's call
- `**[question]**` — you need an answer to finish judging something
- `**[nit]**` — style; mention only if not already caught by clippy/fmt

Build the review as a JSON file first, then submit it — this lets you proofread the whole
review and recover if the API call fails:

```sh
cat > /tmp/review-pr<N>.json <<'EOF'
{
  "body": "<summary: what you checked, gate results, verdict rationale>",
  "event": "REQUEST_CHANGES",
  "comments": [
    {"path": "consonance/foo/src/lib.rs", "line": 42, "side": "RIGHT",
     "body": "**[blocking]** ..."},
    {"path": "consonance/foo/src/wire.rs", "start_line": 10, "line": 18, "side": "RIGHT",
     "body": "**[suggestion]** ..."}
  ]
}
EOF
gh api repos/{owner}/{repo}/pulls/<N>/reviews --input /tmp/review-pr<N>.json
```

Notes on the mechanics:
- `line` is a line number in the **head** version of the file, and it must appear in the
  PR diff. For new crates that's every line; for edits to existing files, comment only on
  changed/context lines or the API rejects the review.
- `event` is `APPROVE`, `REQUEST_CHANGES`, or `COMMENT`. Any `[blocking]` finding ⇒
  `REQUEST_CHANGES`. `APPROVE` requires **all three**: clean gates, no blocking findings,
  and a **clean cross-model pass** (§5) on the current head — if blocking findings were
  fixed, that means a *fresh* GPT-5.6 Sol pass after the fixes, not the original one
  (suggestions alone shouldn't hold a delegated-task PR hostage; the integrator merges,
  the author iterates).
- Omitting `event` creates a *pending* (draft) review only you can see — useful if the
  user wants to look before it goes out.

The summary body should state plainly: which gates you ran and their results, whether the
public API matches the spec, and the count of blocking findings. That's what the
integrator reads first.

## 7. Clean up

```sh
git -C ~/workspace/harmony worktree remove ../harmony-review-pr<N>
```

Report back to the user with the verdict, the blocking findings, and a link to the
posted review.
