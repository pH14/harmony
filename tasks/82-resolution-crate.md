# Task 82 — `dissonance/resolution`: the moment-addressed session client, REPL, and transcript

> **DELEGABLE (with named sibling deps) · the agent-facing surface.** `docs/RESOLUTION.md` rules
> that resolution's first user is an agent and its surface is API-first: a session client over
> the task-58 socket, a human/agent REPL over that client, and a moment-stamped transcript that
> makes every investigation a replayable artifact. This task builds all three as one pure-logic
> crate against an in-crate mock server; the live proof is one thin box gate handed to the
> foreman.
>
> Depends on **tasks 58/80/81** for the *live* gate; the crate itself builds and fully gates
> against the mock (the verb set, including `read`/`regs`/`exec`, is fixed by those specs — code
> against the wire contract, not the live server).

Read first: `tasks/00-CONVENTIONS.md`, `docs/RESOLUTION.md` (all of it — this crate is its v1),
`docs/DISSONANCE.md` ("The control transport (verbs)", the reproducer ruling),
`tasks/80-inspection-verbs.md`, `tasks/81-improvisations.md`, `dissonance/control-proto/src/`
(the codec this speaks), `dissonance/environment/src/` (`Environment`, `Moment`),
`consonance/telemetry/src/main.rs` (the `--source`/cli conventions and one-renderer principle
this crate's transcript mode mirrors).

**Dependency grant (hard rule 2 exception, explicit):** `dissonance/control-proto` and
`dissonance/environment` as normal workspace deps — this crate *is* a client of that wire
contract and *is* addressed by those env types. No dependency on `explorer` (resolution speaks
the socket directly; it is not a `Machine`).

## Environment

Pure-logic, macOS + Linux, laptop-gated: all tests run against an **in-crate mock server**
(in-process loopback speaking the real codec, scripted guest behavior — the task-58 loopback
pattern, owned here). **Box gate:** one live scenario, handed to the foreman (this is the
delegable/box split of `docs/harmony-box-only-gates` practice).

## What to build

### 1. `MomentRef` — the copyable coordinate

```rust
pub struct MomentRef { pub env: Environment, pub moment: Moment }
```

A versioned, self-contained **textual** encoding (display + parse; implementer picks the format,
documents it, round-trips it) — the artifact users copy out of findings/logs and paste into a
session. Parsing never panics (untrusted input).

### 2. The session client

`Session::connect(socket)` then:

- `materialize(mref) → MaterializedSession` — `branch(genesis, mref.env)` + `run(until =
  mref.moment)`; v1 always roots at genesis (nearest-retained-ancestor arrives with the Archive,
  task 64+ — design the signature so a snapshot hint can be added without breaking).
- Observation: `read`, `regs`, `hash` passthroughs.
- Navigation: `run(until)`, re-materialize (wind back = materialize again — cheap by ruling).
- Improvisation: `exec(cmd)` — surfaces the taint state; the client refuses nothing (the server
  guard is authoritative) but *displays* taint prominently.
- **Counterfactual (replay-with-one-change):** `vary(mref, edit) → MomentRef` — apply one
  override edit (add/remove/change an entry in `overrides`) to a copy of the env; materializing
  the result is the counterfactual run. Pure function; the native data model does the work.

Fail-loud: `StopReason` vs `ControlError` are never conflated (the two-result-categories rule);
`Tainted` errors surface verbatim.

### 3. The REPL (`resolution` bin, `required-features = ["cli"]`)

Commands mapping 1:1 onto the client: `open <momentref>`, `regs`, `read <gpa> <len>`, `hash`,
`run <until>`, `exec <cmd>`, `vary <edit>`, `transcript`. No cleverness; the REPL is a thin,
scriptable shell (agent-first: every command reads as a line, emits deterministic
machine-parseable output plus a human rendering).

### 4. The transcript

Every command + result summary appended as one JSONL record, `MomentRef`-stamped, with a
monotonic sequence number. `resolution --transcript <file>` re-renders a recorded investigation
identically (task 29's one-renderer principle: live and replay share the rendering path).
Transcripts are deterministic given the same session inputs (no wall-clock in records; V-time
and sequence numbers only).

## Acceptance gates

1. **Standard suite** green (build / nextest / clippy `-D warnings` / fmt / deny), all-features,
   macOS + Linux.
2. **Proptests (≥256):** `MomentRef` display/parse round-trip (including adversarial inputs —
   parse errors, never panics); `vary` is pure and minimal (edits exactly one key; env otherwise
   byte-identical); transcript replay renders byte-identically to the live rendering for
   arbitrary scripted sessions.
3. **Mock integration:** a scripted end-to-end investigation against the mock server —
   materialize → inspect → exec (mock taints) → vary → materialize the counterfactual —
   exercising every REPL command and both error categories.
4. **Box gate (foreman):** open a real `MomentRef` from a mid-workload Postgres run; `regs` +
   `read` twice from genesis → identical; one `exec` improvisation on a fork with the original
   timeline's hash unperturbed; one `vary` counterfactual that visibly diverges (different
   hash). Record the session transcript in `IMPLEMENTATION.md`.

## Non-goals

- The MCP/agent harness and rehearsal-mark inbox (later handoff; the REPL's line protocol is
  designed to be wrapped, not replaced); `donate` (task 64+); triage drivers
  (ddmin/bisection/LDFI — later, as REPL commands over this client); the findings report
  (task 83); any web/visual UI; nearest-ancestor materialization (Archive-era optimization).
