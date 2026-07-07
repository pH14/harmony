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
| `main` (bin, `cli`) | the `resolution` REPL: live from stdin (`--transcript` logs JSONL) or `--replay <file>` re-render |

Gates: standard suite green (build / nextest / clippy `-D warnings` / fmt / deny), all-features,
macOS (portable — see below); proptests at 256 cases; the scripted mock investigation; the CLI
end-to-end live==replay test. **36 tests.**

`materialize`/`open` **surface the landing `StopReason`** (`MaterializedSession::stop`, and the
`Opened` transcript record's `stop`/`detail`): a guest that crashes or quiesces *before* the
requested moment lands at that earlier moment and reports the crash/quiescence, never a swallowed
clean open (test `open_surfaces_an_early_crash_stop`).

### Reproducer-semantics discipline (review round 2)

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
  `--replay` byte-identity property is preserved (the marker rides the stamp string through the
  one renderer).
- **`replay` restores the world verbatim.** `MockServer` snapshots capture the **whole** timeline
  (world seed + env + moment + taint), so `snapshot-under-A → branch-to-B → replay(snap)` restores
  A's world, not A's moment inside B's world (`replay_restores_the_whole_world_verbatim_after_a_branch`).
  The mock's quiescence point is now derived from the live world on demand, not a stored field, so
  it cannot go stale across a branch/replay.

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
- **CLI flags: `--transcript <file>` logs the live JSONL; `--replay <file>` re-renders.** The spec
  phrases replay as "`resolution --transcript <file> re-renders`"; splitting into two unambiguous
  flags avoids overloading one flag for both log-target and replay-source. Both modes render
  through the *same* `render_transcript`, so the one-renderer guarantee holds. Rejected: a single
  `--transcript` that guesses live-vs-replay from whether stdin is a TTY (non-deterministic,
  scripting-hostile).

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
