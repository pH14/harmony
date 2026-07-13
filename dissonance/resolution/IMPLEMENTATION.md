# `dissonance/resolution` — implementation notes (task 82)

The moment-addressed session client, REPL, and replayable transcript — dissonance's
epoch-loop agent surface. Three things, one pure-logic crate against an in-crate mock
server; the live proof is one box gate handed to the foreman.

## What's here

| Module | Role |
|---|---|
| `mref` | `MomentRef` (the copyable coordinate), its versioned textual codec, `OverrideEdit`, the pure `vary` counterfactual |
| `server` | the `Server` seam (the client's view of a control-transport server) + the task-80/81 views `RegsView` / `ExecResult` / `Snapshot` |
| `session` | `Session::connect` / `materialize` → `MaterializedSession`; the observation / navigation / improvisation verbs |
| `mock` | `MockServer` — the in-crate scripted, deterministic guest the whole laptop gate runs against |
| `transcript` | the `MomentRef`-stamped JSONL `Record` + `render_line`, the one renderer live and replay share |
| `repl` | the eight-command line protocol (`Command`) + the recording `Shell` |
| `main` (bin, `cli`) | the `resolution` REPL: `--transcript <file>` re-renders (spec's replay, read-only), else live from stdin (`--record <file>` logs JSONL) |

Gates: standard suite green (build / nextest / clippy `-D warnings` / fmt / deny), all-features,
macOS (portable — see below); proptests at 256 cases; the scripted mock investigation; the CLI
end-to-end live==replay test. **49 tests.**

**Every command is recorded, `transcript` included (round-9 fix).** The `transcript` command is a
recorded [`Record`] like every other command — its outcome captures the view *deterministically* as
a `(count, digest)` (the number of preceding records and an FNV fingerprint of their rendering),
**not** the full rendered text, which would be self-referential (a later `transcript` would
re-render this one, unbounded). So `--record` captures it and `--transcript` replay reproduces the
live stdout byte-for-byte even for scripts that include `transcript`
(`transcript_command_is_recorded_and_replays_byte_identically`, the CLI test's script, and the
byte-identity proptest all now include it). The full re-render of an investigation is
`resolution --transcript <file>`; the in-REPL `transcript` command prints a one-line checkpoint.

**`open` is transactional.** `materialize` invalidates `current` *before* touching the server and
installs the new timeline *only on full success*; if `branch` succeeds but the follow-up `run`
fails, `current` is left `None` (stamps show `-`, `materialized()` errors `NothingOpen`) rather
than a stale coordinate that names the *old* timeline while the server already sits on the new
branch (`open_is_transactional_when_the_run_fails`, a mock that fails the run after a successful
branch). Wind-back is `Session::materialize` again (a fresh handle) — there is no
`current`-mutating method on a live `MaterializedSession`, so its "an open timeline exists"
invariant holds after this change.

`materialize`/`open` **surface the landing `StopReason`** (`MaterializedSession::stop`, and the
`Opened` transcript record's `stop`/`detail`): a guest that crashes or quiesces *before* the
requested moment lands at that earlier moment and reports the crash/quiescence, never a swallowed
clean open (test `open_surfaces_an_early_crash_stop`).

## Spec contract audit (`tasks/82-resolution-crate.md`, line-by-line)

Every contract statement in the task spec, checked against the implementation. **✓** = met as
written; **✓ (deviation)** = met with a documented, deliberate deviation (all forced or additive,
none reducing the contract).

| # | Spec statement | Status | Where / note |
|---|---|---|---|
| **MomentRef** |
| 1 | `struct MomentRef { pub env, pub moment }` | ✓ (deviation) | `mref.rs`. `env: EnvSpec`, not the `Environment` *trait* the doc names — the reproducer type. Fields `pub`, names/`moment` type exact. |
| 2 | Versioned, self-contained **textual** encoding (display + parse, implementer picks/documents/round-trips) | ✓ | `mref1:<moment>:<lower-hex(EnvSpec::encode())>`; `Display` + `parse`; round-trip proptest `mref_round_trips`. |
| 3 | Parsing never panics (untrusted input) | ✓ | Total `parse`; `mref_parse_never_panics` over arbitrary + structured-garbage strings. |
| **Session client** |
| 4 | `Session::connect(socket)` | ✓ (deviation) | `Session::connect(server: S)` over the local `Server` seam (control-proto lacks the 80/81 verbs on this branch; rule 2 — see "Server seam" below). |
| 5 | `materialize(mref) → MaterializedSession` = `branch(genesis, env)` + `run(until = moment)`; v1 roots at genesis; signature ready for a snapshot hint | ✓ | `Session::materialize`; transactional; private `materialize_from(mref, root)` is the snapshot-hint seam. |
| 6 | Observation: `read`, `regs`, `hash` passthroughs | ✓ | `MaterializedSession::{read,regs,hash}`. |
| 7 | Navigation: `run(until)`, re-materialize (wind back = materialize again) | ✓ | `run`; wind-back is `Session::materialize` again (no separate method, per the literal ruling). |
| 8 | Improvisation: `exec(cmd)` — surfaces taint; refuses nothing; displays taint prominently | ✓ | `exec` returns `ExecResult::tainted`; refuses nothing; REPL shows `[TAINTED]` + `!`. |
| 9 | Counterfactual: `vary(mref, edit) → MomentRef`; pure function, one override edit | ✓ | `MomentRef::vary(&self, &edit)`; `vary_is_pure_and_minimal`. |
| 10 | Fail-loud: `StopReason` vs `ControlError` never conflated; `Tainted` surfaces verbatim | ✓ | `run` → `Ok(StopReason)`; failures → `SessionError`; `Tainted` verbatim (the taint rule). |
| **REPL** |
| 11 | `resolution` bin, `required-features = ["cli"]` | ✓ | `Cargo.toml` `[[bin]]`. |
| 12 | Commands 1:1: `open`, `regs`, `read <gpa> <len>`, `hash`, `run <until>`, `exec <cmd>`, `vary <edit>`, `transcript` | ✓ | Exactly these 8 (`repl.rs` `Command`); `every_repl_verb_parses`. |
| 13 | No cleverness; thin scriptable shell; line in, deterministic machine-parseable + human rendering | ✓ | One `Record` (JSONL) + `render_line` per command. |
| **Transcript** |
| 14 | One JSONL record per command, `MomentRef`-stamped, monotonic seq | ✓ | `Record { seq, mref, cmd, outcome }`. |
| 15 | `resolution --transcript <file>` re-renders a recorded investigation identically | ✓ (round-5 fix) | `--transcript` is **replay/re-render, read-only**; live-log output is `--record`. `cli.rs` asserts the replay input is unmodified. |
| 16 | Deterministic (no wall-clock; V-time + seq only) | ✓ | No `Instant`/`SystemTime`; seq + `Moment` only. |
| **Acceptance gates** |
| 17 | Gate 1: standard suite green (build/nextest/clippy `-D`/fmt/deny), all-features, macOS **+ Linux** | ✓ | Green on macOS; portable (no `unsafe`/`cfg(target_os)`/OS-only APIs/`HashMap`/float/wall-clock) ⇒ Linux. |
| 18 | Gate 2: proptests ≥256 — mref round-trip (adversarial); `vary` pure+minimal; transcript replay byte-identical | ✓ | All three at 256 cases (`tests/proptests.rs`). |
| 19 | Gate 3: scripted end-to-end (materialize→inspect→exec→vary→materialize counterfactual), every REPL command, both categories | ✓ | `repl_drives_the_whole_investigation` + client-level tests. |
| 20 | Gate 4: box gate → foreman; record transcript in IMPLEMENTATION.md | ✓ (handed off) | Procedure + laptop analogues below; transcript pending the box + merged 80/81. |
| **Boundaries** |
| 21 | Deps: `control-proto` + `environment` only; **no** `explorer` dep | ✓ | `Cargo.toml`. |
| 22 | Non-goals (MCP harness, rehearsal-mark inbox, `donate`, triage drivers, findings report, UI, nearest-ancestor) | ✓ | None implemented. |

**Surface beyond the spec (all additive, documented):** `recorded_env` (a client method — the
task-81 taint-guard's fail-loud site, *not* a REPL command); `MaterializedSession::mref()` returns
`Result` (fail-loud on taint, the taint rule); the `--record` flag (live-log output, since the spec
names only `--transcript` for replay); and the local `Server`/`MockServer`/`RegsView`/`ExecResult`/
`Snapshot` seam (rule 2, pending merged 80/81). Nothing removes or narrows a contract.

### The exec seam (review round 2)

- **`exec` advances the tracked V-time.** Against the real verb the guest runs to a completion
  sentinel or the deadline, so V-time moves. After a successful `exec` the session refreshes
  `cur.moment` from the **`regs` verb** (`RegsView` carries the current `Moment`) rather than
  extending `ExecResult` — which would drift the mirrored task-80/81 wire contract. This keeps the
  *next* `exec`'s deadline (`moment + EXEC_BUDGET`) and `moment()`/`mref()` correct. The
  `MockServer`'s `exec` now advances time so the seam is exercised by the gates
  (`exec_advances_the_session_moment`).
- **exec output is recorded losslessly.** Guest serial bytes are arbitrary (not necessarily
  UTF-8); the JSONL transcript is the replayable artifact, so `Outcome::Exec.output_hex` stores the
  **exact bytes** as lower-hex (`String::from_utf8_lossy` would substitute U+FFFD and corrupt both
  the bytes and the byte count). `render_line` presents a human-lossy escaped view over the decoded
  bytes; the artifact round-trips exactly (`exec_output_round_trips_losslessly_including_non_utf8`,
  and the mock now emits a couple of non-UTF-8 bytes so the CLI/proptest exercise the path
  end-to-end). `--transcript`/replay byte-identity is preserved.

### Reproducer-semantics discipline (review round 1)

Three properties the crate exists to embody, each with a regression test:

- **`vary` renders a paste-able address.** The one command whose entire output *is* a
  counterfactual `MomentRef` renders it in **full** (never `short`), so an agent/human consuming
  rendered output — not the JSONL — can paste it straight into `open`
  (`vary_renders_a_pasteable_full_momentref`).
- **A tainted coordinate never lies.** A record observed on a timeline an `exec` improvisation has
  tainted is stamped with `MomentRef::TAINTED_STAMP_PREFIX` (`tainted!…`), not a bare
  reproducible-claiming `MomentRef` — the state is off the record (task 81) and not regenerable
  from `(seed, overrides)`. `MomentRef::parse` refuses the marked form
  (`MRefParseError::Tainted`), so `open` rejects it loudly instead of silently reopening the
  *untainted* pre-`exec` state; the human render flags it with a leading `!`
  (`tainted_records_get_a_non_reproducible_stamp`, `tainted_stamp_is_refused_by_parse`). The
  `--transcript`/replay byte-identity property is preserved (the marker rides the stamp string through the
  one renderer).
- **`replay` restores the world verbatim.** `MockServer` snapshots capture the **whole** timeline
  (world seed + env + moment + taint), so `snapshot-under-A → branch-to-B → replay(snap)` restores
  A's world, not A's moment inside B's world (`replay_restores_the_whole_world_verbatim_after_a_branch`).
  The mock's quiescence point is now derived from the live world on demand, not a stored field, so
  it cannot go stale across a branch/replay.
- **A crash is terminal (round-5/6 fix).** Once a scripted fault crashes the guest, the mock latches
  `Timeline.crashed`: every subsequent `run` re-reports the crash at its `Moment` without advancing
  (so a later `run` can't skip the already-hit override and fabricate post-crash state), and
  `exec` re-reports the terminal condition too (`ok = false`, no output, no advance — a crashed
  guest cannot run a command; the attempt still marks+reports `tainted`) rather than fabricating a
  successful run. Observations stay at the
  crash point until the client re-materializes (`branch`/`replay` installs a fresh/restored
  timeline). `MockServer` is the laptop reference model for session semantics, so this had to be
  right (`a_crashed_timeline_stays_terminal_until_rematerialize`, `exec_on_a_crashed_timeline_does_not_run`).
- **V-time is monotonic — `run` never rewinds (round-8 fix).** `exec` can push `cur.moment` past the
  quiescence point, after which a later `run` computed `target = min(deadline, quiescent) < cur.moment`
  and the assignment *rewound* the timeline (a forward `run` reporting an earlier moment). `run` now
  makes no backward move: if the (clamped) target is at or behind the current point it reports the
  current position (`Quiescent` if at/past quiescence, else the met `Deadline`) without changing
  `cur.moment` (`exec_past_quiescence_does_not_rewind_the_moment`).

## The taint rule (the single source of truth)

> **A tainted timeline — one an `exec` improvisation has poisoned (task 81, `docs/RESOLUTION.md`
> §Improvisations) — has no reproducible coordinate. Therefore: (1) every path that would emit a
> *bare, pasteable* `MomentRef` or reproducer derived from a tainted timeline routes through one
> structural guard (`Session::guard_reproducible`) and fails loudly with `SessionError::Tainted`
> (the one exception is the transcript stamp, which records the non-pasteable `tainted!…` marked
> form so the record stays complete and `open` refuses it); and
> (2) taint is recorded *conservatively* — `cur.tainted = true` is set **before the exec request is
> issued to the server**, not after a successful reply. Once the request may have reached the
> server it may have applied it, even if the reply is then lost, times out, or decodes as a
> transport error; there is no failure point after which "clean" can be reclaimed.**

### The exec flow, every failure point → taint state

`exec` marks taint before the round-trip, so the timeline is tainted at *every* point after the
request leaves the client. Enumerated:

| Failure point | Server-side timeline | Client `exec` returns | Client taint | Coordinate emitters (`mref`/`vary`/`recorded_env`) |
|---|---|---|---|---|
| request send fails (never reached server) | untouched | `Err` | **tainted** (conservative — the client cannot distinguish this from below) | fail `Tainted` |
| applied, but reply lost / decodes as transport error | **improvised** | `Err` | **tainted** | fail `Tainted` |
| reply is a `ControlError` (server rejected) | per server | `Err` | **tainted** | fail `Tainted` |
| success, but the follow-up `regs` refresh fails | improvised | `Err` | **tainted** (moment stays stale) | fail `Tainted` |
| full success | improvised | `Ok(ExecResult)` | **tainted**, moment refreshed | fail `Tainted` |

The conservative mark makes the *send-fails* row (a false positive — the server never saw it) the
price of never producing the far worse false negative: a clean-looking coordinate on a
server-side-improvised timeline. Regression: `exec_reply_lost_still_taints_conservatively` (a mock
that applies the exec then errors the reply). `exec` on a crashed timeline re-reports the terminal
condition (`ok = false`, no output, no advance) rather than fabricating a run — but an exec
*attempt* is still an improvisation, so **every** mock `exec` path (including the crashed refusal)
marks *and reports* `tainted: true`, consistent with the client's conservative mark and the
"`exec` surfaces the taint bit" contract (`exec_on_a_crashed_timeline_does_not_run`).

Pure observations and navigation are always allowed on a tainted timeline — they do not emit a
coordinate. Every verb/accessor, audited against the rule:

| Verb / accessor | Emits a coordinate? | On a tainted timeline | Test (where nontrivial) |
|---|---|---|---|
| `open` / `materialize` | no (a session) | **resets** taint to `false` (fresh branch from genesis) — this is how you "wind back" to vary | `exec_taints_the_fork_and_leaves_the_original_unperturbed` |
| `read` | no (bytes) | allowed (pure observation, hash-invariant) | `observation_never_perturbs_the_hash` |
| `regs` | no (`RegsView`) | allowed (pure; also how `exec` learns the post-exec `Moment`) | `observation_never_perturbs_the_hash` |
| `hash` | no (digest) | allowed (the digest *reflects* taint, so a fork diverges) | `exec_taints_the_fork_…` |
| `run` | no (`StopReason`) | allowed (advances the tainted timeline) | — |
| `exec` | no (`ExecResult`) | **sets** taint — *conservatively, before the round-trip* (see the exec-flow table above) | `exec_reply_lost_still_taints_conservatively`, `taint_is_recorded_before_the_fallible_moment_refresh` |
| `recorded_env` | the reproducer (`EnvSpec`) | **fails `Tainted`** via `guard_reproducible` (before delegating — the server guard is blind to local taint) | `every_reproducer_emitter_refuses_…`, `exec_taints_the_fork_…` |
| `MaterializedSession::mref()` | **yes** (`MomentRef`) | **fails `Tainted`** via `guard_reproducible` | `every_reproducer_emitter_refuses_…`, `exec_advances_…` |
| `moment()` | no (bare `u64` V-time) | allowed (a V-time is not a coordinate) | `exec_advances_…` |
| ~~`env()`~~ | — | **removed** — a raw `&EnvSpec` accessor is reproducer material that would bypass the guard; use `recorded_env` | — |
| REPL `vary` | **yes** (`Varied.mref`) | **fails `Tainted`** via `guard_reproducible` (wind back to vary) | `every_reproducer_emitter_refuses_…`, `vary_on_a_tainted_timeline_fails_loudly` |
| `MomentRef::vary` (pure fn) | a `MomentRef` | n/a — a `MomentRef` *value* has no taint; the REPL guards the *timeline* before calling it | — |
| transcript stamp | the record's `mref` | the non-pasteable `tainted!…` marked form (audit-complete; `open` refuses it) | `tainted_records_get_a_non_reproducible_stamp` |
| `open <tainted!…>` | — | refused (`MRefParseError::Tainted`) | `tainted_stamp_is_refused_by_parse` |
| `Session::current_mref()` | raw (`pub(crate)`) | internal only — the stamp marks it; REPL `vary` guards on `tainted()` first — never a public bare emitter | — |

### The structural choke-point (round-8 — one guard, grep-audited)

The taint-ordering family recurred four times as point fixes, so it is now **structural**: one
private guard, `Session::guard_reproducible(&self) -> Result<(), SessionError>`, checks the **local**
`cur.tainted` bit and is the single gate **every** reproducer/coordinate emitter routes through
*before any server delegation*. The local bit is authoritative because it is set conservatively the
instant an `exec` request is issued (§the exec flow), possibly before the server marks its own
timeline — so `recorded_env` delegating to the server's guard alone would mint a clean reproducer
after a lost/dropped exec reply (`recorded_env`'s exact round-8 hole).

Every emitter and its guard, and the grep that audits nothing bypasses it:

| Emitter | Routes through `guard_reproducible`? |
|---|---|
| `MaterializedSession::mref()` | yes — first line |
| `MaterializedSession::recorded_env()` | yes — first line, **before** `server.recorded_env()` |
| REPL `vary` (`Shell::run_cmd`) | yes — `guard_reproducible().is_err()` before building the counterfactual |
| ~~`MaterializedSession::env()`~~ | **removed** (was an unguarded raw `&EnvSpec` reproducer accessor) |
| `Session::current_mref()` | raw `pub(crate)`, not an emitter: the transcript stamp *marks* its output (`tainted!…`, non-pasteable), and REPL `vary` calls `guard_reproducible` first |

```
$ grep -rn 'server\.recorded_env\|c\.env\.clone()\|current_mref()\|MomentRef::new' dissonance/resolution/src
# non-test hits: mref()/current_mref() build a coordinate; recorded_env() mints the reproducer;
# repl.rs current_mref() is the stamp (marks) + vary (guarded). The three emitters each call
# guard_reproducible first; nothing bypasses it. (MomentRef::new hits in mref.rs are #[cfg(test)].)
```

Regression `every_reproducer_emitter_refuses_on_a_locally_tainted_session` drives a server that is
**clean** (the exec's send failed, so the server never saw it) while the client is conservatively
tainted, and asserts `mref` / `recorded_env` / REPL `vary` all refuse — i.e. the *local* guard, not
the server's, is doing the work. The taint-ordering family closed one level at a time — set-after-
`exec`-before-`regs` (round 3) → set-before-the-round-trip (round 6) → this single structural
choke-point (round 8), which subsumes both. `--transcript`/replay byte-identity is preserved
throughout.

## The load-bearing decision: the `Server` seam (and why not raw `control-proto`)

The task says "code against the wire contract … `read`/`regs`/`exec` … fixed by [tasks 80/81]".
But **`control-proto` on this branch does not yet carry those verbs** — tasks 80/81 are sibling
specs, unmerged — and hard-rule 1 forbids editing `control-proto` from here. So, per **conventions
rule 2 (define interfaces locally)**:

- The `Session` speaks a **locally-defined `Server` trait** — the client's view of a task-58/80/81
  control server. The in-crate `MockServer` is the in-process loopback (the task-58 loopback
  pattern, owned here).
- The verbs `control-proto` **already** carries (`hello`/`snapshot`/`drop`/`branch`/`replay`/
  `run`/`hash`) take and return its **real wire types** (`control_proto::Environment`,
  `StopReason`, `HashScope`, `SnapId`, `Caps`, `ControlError`). `tests/wire.rs` pins that the exact
  request/reply values the client builds — most importantly the `branch` environment
  `materialize` ships (`blob_version` + `EnvSpec::encode()`) — round-trip through `control-proto`'s
  codec byte-for-byte and decode back to the original `EnvSpec`. So the seam cannot drift from the
  wire contract.
- The three unmerged verbs (`read`/`regs`/`exec`) use **local views** (`RegsView`, `ExecResult`)
  and a local `SessionError::Tainted`, shaped exactly as tasks 80/81 fix them.

**Integrator action when 80/81 merge:** collapse the three local views + `Tainted` onto the real
`control-proto` surface, and provide a real-socket `Server` impl (a thin adapter mapping each seam
method to one `encode_request`/`decode_reply` exchange — the shape `explorer::SocketMachine`
already uses). The client's observable behaviour does not change.

> **Discharged by task 107** — with one half deliberately declined. The real-socket `Server` impl
> exists (`SocketServer`, below). The local views were **kept**, not collapsed, and that is now a
> considered decision rather than a stopgap: `RegsView` carries `Serialize`/`Deserialize` (the
> transcript records a register view losslessly) and `control_proto::RegsView` does not; `ExecResult`
> carries the `tainted` bit and the wire's `Reply::ExecResult` does not (taint rides the `Snapshot`
> reply); and `SessionError::Tainted` is raised by the **client's own conservative guard** — set the
> instant an `exec` is issued, before any reply — so it can fire where no wire `ControlError::Tainted`
> ever arrives. Collapsing them would lose all three. The adapter converts between the two shapes in
> exactly one place (`socket.rs`), which is where a mirrored contract belongs.

## Deviations considered

- **`MomentRef.env: EnvSpec`, not `Environment`.** The doc writes the field as `env: Environment`,
  but `environment::Environment` is the *decide-seam trait*, not a data type. The reproducer it
  names is the concrete genesis-complete `EnvSpec` (the value `compose` mints for a `Bug.env`, and
  the value `branch` reseeds with). Using `EnvSpec` is the only coherent reading; documented on the
  struct.
- **`vary` is `MomentRef::vary(&self, &edit)`**, i.e. `vary(mref, edit)` with `mref` as the
  receiver — the spec's "pure function" form. The REPL's `vary` applies it to the currently-open
  `MomentRef`.
- **`recorded_env` is a client method, not a REPL command.** The spec requires "Tainted errors
  surface verbatim," and the REPL is the thin eight-command shell (no room to add a ninth). The
  task-81 taint guard fires at `recorded_env` (mint a reproducer), so that is a
  `MaterializedSession` method (the one client path the guard is observable through) — *not* a REPL
  verb, and *not* `donate` (task 64+, a non-goal). `exec`'s result surfaces the taint bit for the
  REPL's prominent display.
- **CLI flags follow the spec: `--transcript <file>` is *replay* (re-render), `--record <file>` is
  the live-log output.** Task 82 §The transcript documents `resolution --transcript <file>` as the
  re-render form, so `--transcript` re-renders and is **read-only** — never written back (a replay
  can never truncate the recording). The live session's JSONL log gets its own flag, `--record`,
  and the two are mutually exclusive. Both modes render through the *same* `render_transcript`, so
  the one-renderer guarantee holds. (Round-5 fix: an earlier revision had `--transcript` as the
  live-log output written at exit, which meant the spec's own replay invocation truncated the
  recording — a destructive spec violation.)

## The `vary` minimality property, precisely

`vary` edits **exactly one override key**, env otherwise unchanged (proptests
`vary_is_pure_and_minimal`, `vary_on_recorded_base_is_byte_minimal`, `vary_set_writes_exactly_that_key`).
One nuance re the spec's "byte-identical": a `Set` on a **`Seeded`** base necessarily promotes it
to `Recorded` (you cannot hold an override without the `Recorded` variant — this is exactly
`EnvSpec::record`'s own behaviour, and an empty `Recorded` is stream-equivalent to `Seeded`). So on
a `Seeded` base the variant tag changes; on an already-`Recorded` base (the shape a real finding's
`Bug.env` has) the encoding is byte-identical except the one key. The tests assert *logical*
minimality (seed / policy / reseeds / overrides-sans-key unchanged) universally, and *byte*
minimality on `Recorded` bases.

## `MomentRef` textual format

`mref1:<moment-decimal>:<lower-hex of EnvSpec::encode()>` — one line, no spaces, copy/paste-safe.
Canonical (the `EnvSpec` encoder is byte-deterministic; hex is lower-case only), so equal
`MomentRef`s render identically. `parse` is total: any malformed paste is an `MRefParseError`,
never a panic (proptests over arbitrary and structured-garbage strings).

## The mock's scripted guest

`MockServer` is not a VM — it is a deterministic function of a *world seed* (a dependency-free
FNV-1a/SplitMix digest of the active `EnvSpec`, integer math only) and the current `Moment`:

- `regs`/`read` are pure functions of `(world_seed, moment[, addr])` → **observation invariance**
  (they never touch the moment, taint, or hash).
- `hash(Whole)` folds in the taint bit → an `exec`'d fork's hash diverges while a re-materialized
  original's does not.
- editing one override changes the `EnvSpec` bytes → the world seed → every hash: the **counterfactual**
  visibly diverges.
- a staged `CorruptMemory` at a reachable `Moment` crashes the guest there — a real `StopReason::Crash`,
  so a counterfactual can change *behaviour*, not just the hash.

Constants: `READ_CAP` = 64 KiB (oversized `read` rejected before allocation), `DEFAULT_RAM_BYTES`
= 1 GiB (the `read` range ceiling), `EXEC_BUDGET` = 1e6 V-time (default `exec` deadline),
`MAX_HEX_FIELD_BYTES` = 16 MiB (the cap on any hex field decoded from untrusted text). The mock
uses `Moment ≡ run-deadline V-time` 1:1 (the "clock ratio 1" simplification the adapter also uses);
the real substrate's exact-`Moment` force-exit is the box gate's concern.

**Capped untrusted lengths (rule 4), everywhere — not just `read`.** `from_hex` takes a `max_bytes`
and checks the decoded length **before** the `Vec::with_capacity`, so a pasted multi-gigabyte hex
field — an `open`ed `MomentRef` env blob, a `vary … raw` action, an exec output in a replayed
transcript — is rejected cheaply rather than sizing a buffer to it (the `READ_CAP` discipline
generalized). Every caller passes `MAX_HEX_FIELD_BYTES`. Tests
`from_hex_caps_decoded_length_before_allocating` (the mechanism) and
`parse_rejects_an_oversized_env_field_cheaply` (the `open` path) — the latter runs in ~15 ms with the
cap and ~1 s without it, which *is* the cheap-rejection guarantee.

## Portability / quality

- **No `unsafe`, no `cfg(target_os)`, no OS-specific APIs, no `HashMap`/float/wall-clock/`rand`.**
  Pure logic; builds identically on macOS and Linux. Because there is no `unsafe`, the crate needs
  no Miri run and is **not** added to `quality.yml`'s `miri` job.
- **No `tests/public-api.txt`.** The `public-api` CI job enumerates crates explicitly and does not
  include `resolution`, so no baseline is required to pass CI. Adding `resolution` to that job (and
  committing a baseline) is a reasonable integrator follow-up, not a task-82 gate.
- **Dependencies** (all rule-5-whitelisted): `control-proto` + `environment` (the two granted
  exceptions), `thiserror`, `serde`+`serde_json` (transcript JSONL), `clap` (bin only); dev:
  `proptest`, `tempfile`. The mock's digest is hand-rolled integer hashing rather than pulling
  `sha2`/`blake3` — fewer deps, and it need only be deterministic, not cryptographic.
- `Cargo.lock` gains one additive `resolution` block (committed, per the precedent of tasks
  67/69/73).

## Box gate (gate 4) — handed to the foreman

Needs `/dev/kvm`, the patched det-cfl-v1 KVM (stock **1396736**; revert + verify after any patched
run per `docs/BOX-PINNING.md`, `taskset -c 2`), the Postgres image, **and merged tasks 80/81**
(the live `read`/`regs`/`exec` verbs). Procedure once those exist:

1. Provide a real-socket `Server` impl (thin adapter over `control-proto` + the 80/81 verbs; see
   "Integrator action" above) and point the REPL at it instead of `MockServer`.
2. Open a real `MomentRef` from a mid-workload Postgres run (copy it from a finding/log).
3. `regs` + `read` (≥3 probe regions) **twice from genesis** → assert identical (incl. `rip`,
   `Moment`, `hash(Whole)`).
4. One `exec` improvisation on a **fork**; assert the **original** timeline's `hash` is unperturbed
   (re-materialize → identical), and the fork's `recorded_env` fails `Tainted`.
5. One `vary` counterfactual (e.g. add a `CorruptMemory`/`InjectInterrupt`); materialize → assert a
   **different** `hash` (visible divergence).
6. Record the session transcript here.

The laptop gate already exercises the identical client/REPL/transcript logic against the mock
(`materialize_is_deterministic_from_genesis`, `inspection_does_not_change_a_later_hash`,
`exec_taints_the_fork_and_leaves_the_original_unperturbed`,
`vary_counterfactual_visibly_diverges_and_can_crash`, `open_surfaces_an_early_crash_stop`,
`repl_drives_the_whole_investigation`), so the box gate is a live re-confirmation, not new logic.

### Box transcript

_To be recorded by the foreman on the box. No longer blocked on the adapter (task 107 shipped it);
it rides the box's normal schedule._

## Known limitations

- `run`'s `StopMask` is carried but selects nothing on the mock (no decision-surfacing guest, as on
  the task-58 seed-driven server); the mask becomes live when a reactive guest exists.
- The mock's guest is scripted, not a real OS: `read`/`regs` bytes are deterministic noise, not a
  real memory image. It exists to prove the *client/transcript* semantics, which are substrate-agnostic.
  The **real** substrate is now reachable through `SocketServer` (task 107) — see below.

---

# Task 107 — the production socket `Server` (`hm-7j0`)

The seam above promised "a real box connection is a second `Server` implementor handed to the
foreman". It was never built, so the task-86 M0 film live gate had to embed a **test-local** copy of
it (`dissonance/campaign-runner/tests/live_film.rs`), which is the spine finding this task closes.
`SocketServer` is that implementor, in the production surface; the test-local copy is **deleted**.

| Added | What |
|---|---|
| `socket` (module) | `SocketServer<S: Read + Write>` — every `Server` verb as one `control-proto` request/reply on a real stream; plus the paged `sdk_events()` scrape channel |
| `server` | a blanket `impl<S: Server + ?Sized> Server for &mut S` |
| `session` | `Session::connect_rooted(server, genesis)` — connect and root at an **already-captured** snapshot |
| `tests/socket_loopback.rs` | the adapter against a frame-speaking server: the whole verb surface, hash-neutrality, and every trust-boundary path |

## Placement: `dissonance/resolution`, unconditional, no feature gate

The spec floated "likely `dissonance/resolution` behind a feature, or a thin adapter crate". It is
`resolution`, and **not** behind a feature: the adapter needs nothing but `std::io::{Read, Write}`
plus `control-proto` and `environment` — both already dependencies of this crate — so there is no
dependency cost to gate away, no platform fork (it is generic over the stream; the only unix-socket
code in the change is in *tests*), and no third crate to justify. A feature that gates nothing but
pure-std code would only add a build configuration for the gates to have to cross. A separate
adapter crate would have been strictly worse: it would need `resolution` (for the trait) *and* both
wire crates, and would exist to hold one file that has no reason to live outside the crate whose
seam it implements. `film` and `campaign-runner` both already depend on `resolution` with
`default-features = false`, and pick the adapter up for free.

## Two liberties in the test-local adapter that were NOT promoted

Both were documented in the film gate as "required by the gate's geometry". Promoting them as-is
would have baked a lie into a production seam, so each was replaced by an honest construct.

1. **`pin_genesis` → `Session::connect_rooted`.** The test-local adapter let the caller pin a
   pre-taken handle that the *next* `snapshot()` would return instead of capturing anything. But the
   `Server` contract says `snapshot()` **captures state at the current quiescent point**; an impl
   that sometimes hands back a stale handle is a landmine for every consumer that isn't the film
   gate. And the requirement is not really a server requirement at all — it is that a *session* must
   branch from the root its absolute `Moment`s were harvested against (the film gate's frame clock
   is scraped from runs rooted at a specific base). Rooting is a `Session` concern, and this crate's
   own docs already anticipated the seam ("the Archive-era snapshot hint" — `materialize_from`
   always took the root as a parameter). `connect_rooted` is that seam reached from the connect
   side: additive, honest, and it leaves `snapshot()` meaning what it says.
2. **"client-side idempotent `hello`" → `hello` is negotiated once per stream, as a contract.** Same
   observable behaviour, but stated as a rule with a defined failure mode instead of a convenience:
   the wire makes `hello` the first frame of a session (a conforming server may refuse a second),
   so the adapter negotiates once and answers later calls from the cached caps — and offering
   *different* caps after negotiation is a loud `SessionError::Negotiation`, not a silently stale
   answer. This is what lets a raw scrape pass and a `Session` layered over the same adapter share
   one wire session, which is exactly what the film gate needs.

## A bug in the test-local adapter, fixed on promotion

Its `sdk_events()` made a single `Request::SdkEvents { offset: 0 }` call and treated the reply as
the whole capture. The server **bounds each page to the control frame limit** (`page_sdk_events`),
so any capture longer than one page was silently truncated — on the film gate, the frame clock
would simply have stopped early, and the gate would have reported "only N calibrated frames" rather
than a truncation. The production `sdk_events()` pages until an empty page, like
`explorer::SocketMachine` does; `sdk_events_pages_until_the_capture_is_drained` pins it against a
server with a deliberately tiny page size.

## The trust boundary (conventions rule 4)

Every length and index off the wire is checked before it is believed, and every hostile or broken
peer is a **typed `SessionError`** — never a panic, never a silent truncation:

| Wire condition | Result |
|---|---|
| `read(len > READ_CAP)` | `ReadTooLarge`, refused **before any traffic** (the untrusted length never reaches the far end) |
| `Bytes` reply ≠ exactly `len` bytes | loud `Transport` — the contract is "never a truncated success", so a short reply is a broken server, not a partial read |
| reproducer blob in an unknown `blob_version` | `Transport` — refused, not decoded on a guess |
| reply seq ≠ request seq | `Transport` — never paired with the wrong verb |
| reply of the wrong kind | `Transport` |
| torn frame / garbage / EOF mid-verb | `Transport` (the film projector's *recoverable* dropped-session class) |
| over-`MAX_FRAME_LEN` header | the codec rejects it before buffering the body |

`no_reply_bytes_can_panic_the_client` is a 256-case proptest over arbitrary reply bytes: every verb
returns, none panics.

**Error mapping.** The wire's `ReadOutOfRange` / `ReadTooLarge` / `Tainted` are mapped onto the
client's *own* typed variants — the same ones `MockServer` raises for those conditions — so a
consumer's match arms do not depend on which `Server` it holds. Everything else rides through
verbatim as `SessionError::Control` (still visibly server-originated).

## Hash-neutrality (deliverable 4)

Observation must stay observation. The server-side proof (a `state_hash` identical across
interleaved `Read`/`Regs`) is vmm-core's, from PR #51/task 80; this task extends the line to the
*client*, proving it adds no contact of its own:

- `observation_verbs_over_the_wire_are_hash_neutral` — a burst of `read`/`regs`/`hash` moves neither
  the hash, the position, nor the recorded reproducer.
- `an_observation_emits_only_its_own_request_frame` — asserted **at the wire**: the server's request
  log with observations filtered out is exactly the navigation stream. No hidden `run`, no
  re-`branch` — nothing that could touch a draw stream.
- `campaign-runner`'s `resolution_loopback` runs it against the **real substrate hash**: two
  timelines with byte-identical navigation, one peppered with observations at every step, reach the
  same terminal `state_hash`.

## Gates

- `cargo nextest run -p resolution -p campaign-runner --all-features` — **217 passed**, 1 skipped
  (the box-only film gate). 19 new in `resolution`, 1 new in `campaign-runner`.
- `clippy -D warnings` (host **and** `--target x86_64-unknown-linux-gnu`), `fmt --check`,
  `cargo deny check` — all clean.
- **public-api**: `campaign-runner`'s snapshot is unchanged and green (only its tests were touched).
  `resolution` carries no `public-api.txt` — it never has, and it is not in `quality.yml`'s
  public-api `-p` list. Its surface *did* grow here (`SocketServer`, `connect_rooted`, the `&mut`
  blanket impl), so **the integrator may want to add one** — that is a workflow/root-file change I
  left alone (rule 1).
- **Miri**: no `unsafe` was added (the adapter is pure socket/protocol code), so the unsafe⇒Miri
  bar does not bind. The new tests are Miri-viable anyway and were run:
  `MIRIFLAGS=-Zmiri-permissive-provenance cargo +nightly-2026-06-16 miri test -p resolution
  --test socket_loopback` → **18 passed, 1 ignored**, and `--lib` → 18 passed.
  - **What runs under Miri**: the entire verb surface, the trust boundary, hash-neutrality, and the
    totality proptest — all of it, because the far end is an in-memory `Pipe` that services each
    request inline (no socket, no thread, no syscall).
  - **What does not**: exactly one test,
    `socket_server_speaks_the_wire_over_a_real_unix_socketpair`, `#[cfg_attr(miri, ignore)]`d
    because Miri cannot execute socket syscalls — vmm-core's `serve_speaks_frames` precedent. Its
    ignore message names its Miri-run sibling
    (`every_verb_over_the_wire_matches_the_in_process_server`: the same verbs, the same codec).
  - The proptest sets `failure_persistence: None`, without which proptest's regression-file lookup
    calls `getcwd` and aborts under Miri's isolation (the nightly job runs
    `-Zmiri-permissive-provenance` only). Cases drop to 16 under the interpreter.
- **The film live gate is box-only and the box is under the re-cert window, so it was not run.** The
  portable proof the spec asked for is in place: the gate **compiles** against the production
  adapter for `x86_64-unknown-linux-gnu` (clippy `-D warnings`, 0 errors — the only failure when
  *linking* for that target is the absent cross-linker on this Mac), and its loopback shape is
  covered by `campaign-runner`'s `resolution_loopback` against the real `ControlServer`.

## What the integrator must know

- **The film gate's next live run rides the box's normal schedule.** Its logic is unchanged apart
  from the adapter swap and `connect_rooted`; `pin_genesis`'s behaviour is preserved exactly (root
  the session at the pre-taken base), so the gate should behave identically. It is the only
  un-run gate in this change.
- **`campaign-runner`'s `resolution_loopback` is the portable regression net** for the adapter
  against the real server. If a future change to `ControlServer`'s wire breaks resolution, that test
  fails on a laptop rather than on the box.
- The `HOP` in that test is deliberately **off** the mock's 100-ns intercept grid, so a run lands
  *past* its deadline. That overshoot is not a bug — it is the same lower-bound-stamp behaviour the
  film gate calibrates around, and the test asserts the `Session` and the raw adapter agree on the
  landing.
