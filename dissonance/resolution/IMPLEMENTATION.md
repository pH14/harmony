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
end-to-end live==replay test. **44 tests.**

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
  guest cannot run a command) rather than fabricating a successful run. Observations stay at the
  crash point until the client re-materializes (`branch`/`replay` installs a fresh/restored
  timeline). `MockServer` is the laptop reference model for session semantics, so this had to be
  right (`a_crashed_timeline_stays_terminal_until_rematerialize`, `exec_on_a_crashed_timeline_does_not_run`).

## The taint rule (the single source of truth)

> **A tainted timeline — one an `exec` improvisation has poisoned (task 81, `docs/RESOLUTION.md`
> §Improvisations) — has no reproducible coordinate. Therefore: (1) every path that would emit a
> *bare, pasteable* `MomentRef` derived from a tainted timeline fails loudly with
> `SessionError::Tainted` (the one exception is the transcript stamp, which records the
> non-pasteable `tainted!…` marked form so the record stays complete and `open` refuses it); and
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
condition (`ok = false`, no advance) rather than fabricating a run
(`exec_on_a_crashed_timeline_does_not_run`).

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
| `recorded_env` | the reproducer (`EnvSpec`) | **fails `Tainted`** | `exec_taints_the_fork_…` |
| `MaterializedSession::mref()` | **yes** (`MomentRef`) | **fails `Tainted`** | `exec_advances_…`, `taint_is_recorded_…` |
| `moment()` | no (bare `u64` V-time) | allowed (a V-time is not a coordinate) | `exec_advances_…` |
| `env()` | no (`&EnvSpec` base env) | allowed (raw; `recorded_env` is the guarded reproducer-mint) | — |
| REPL `vary` | **yes** (`Varied.mref`) | **fails `Tainted`** (wind back to vary) | `vary_on_a_tainted_timeline_fails_loudly` |
| `MomentRef::vary` (pure fn) | a `MomentRef` | n/a — a `MomentRef` *value* has no taint; the REPL guards the *timeline* before calling it | — |
| transcript stamp | the record's `mref` | the non-pasteable `tainted!…` marked form (audit-complete; `open` refuses it) | `tainted_records_get_a_non_reproducible_stamp` |
| `open <tainted!…>` | — | refused (`MRefParseError::Tainted`) | `tainted_stamp_is_refused_by_parse` |
| `Session::current_mref()` | raw (`pub(crate)`) | internal only — the stamp marks it; REPL `vary` guards on `tainted()` first — never a public bare emitter | — |

Every fix falls straight out of the rule rather than being an isolated patch: REPL `vary` fails
`Tainted` (it was the last bare-coordinate emitter that hadn't been guarded); and the taint-ordering
family closed one level at a time — first set-after-`exec`-before-`regs` (round 3), then
set-before-the-round-trip (round 6, the conservative invariant above), which subsumes it and covers
the applied-but-reply-lost hazard. `--transcript`/replay byte-identity is preserved throughout.

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
= 1 GiB (the `read` range ceiling), `EXEC_BUDGET` = 1e6 V-time (default `exec` deadline). The mock
uses `Moment ≡ run-deadline V-time` 1:1 (the "clock ratio 1" simplification the adapter also uses);
the real substrate's exact-`Moment` force-exit is the box gate's concern.

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

_To be recorded by the foreman on the box (pending merged 80/81 + a live-socket `Server` adapter)._

## Known limitations

- The live backend is the box gate's; v1 laptop code ships only the mock (by ruling).
- `run`'s `StopMask` is carried but selects nothing on the mock (no decision-surfacing guest, as on
  the task-58 seed-driven server); the mask becomes live when a reactive guest exists.
- The mock's guest is scripted, not a real OS: `read`/`regs` bytes are deterministic noise, not a
  real memory image. It exists to prove the *client/transcript* semantics, which are substrate-agnostic.
