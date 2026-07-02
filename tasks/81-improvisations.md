# Task 81 — improvisations: `exec`-in-fork + lineage taint

> **FRONTIER · the interrogation verb.** No channel exists to run a command inside a live guest —
> in any form (`docs/REVIEW-2026-07.md`: the real Linux path traverses zero hypercall seams). The
> ruling in `docs/RESOLUTION.md` §Improvisations makes one buildable *now*: `exec` is an
> **improvisation** — one-off, never recorded into any `Environment`, its timeline never admitted
> — and is therefore **exempt from the determinism discipline**. The channel can be crude (serial
> byte injection); what must be airtight is the **taint guard** that keeps improvised timelines
> out of the reproducer story. **Explicitly does NOT depend on task 61**: the net vertical builds
> the deterministic guest-plane seam; this builds the disposable one.
>
> Depends on **task 58** (server) — task 80 (`read`/`regs`) is a natural companion but not a
> prerequisite.

Read first: `tasks/00-CONVENTIONS.md`, `docs/RESOLUTION.md` ("The search-surface criterion",
"Improvisations"), `docs/DISSONANCE.md` ("The control transport (verbs)" — the no-bare-`restore`
/ fail-loud ethos this extends), `consonance/vmm-core/src/snapshot.rs` (the lineage the taint bit
rides), the serial-console path in `consonance/vmm-core` (task 33 wired serial + IRQ),
`guest/linux/` image config (a serial shell must exist for `exec` to talk to).

## Environment

Taint propagation and the exec-session state machine are pure logic, **macOS + Linux testable**
against mocks, and MUST carry the portable gates. The live `exec` proof is **box-only** (patched
KVM, det-cfl-v1, Postgres image). Pin per `docs/BOX-PINNING.md`; always revert KVM to stock
**1396736** + verify after any patched run.

Surface list (frontier waiver of hard rule 1): `consonance/vmm-core` (serial input injection,
exec session, taint bit on snapshot lineage), `dissonance/control-proto` (the `exec` verb +
`ControlError::Tainted` + a `tainted` flag where snapshots surface), `guest/linux/` (image
config: a root shell on the serial console — note the `MANIFEST.sha256` implications and record
them; coordinate with the task-90 hashed-input ruling).

## What to build

### 1. The `exec` verb (deliberately crude)

`exec { cmd: String, deadline: VTime } → Reply::ExecResult { output: Vec<u8>, ok: bool }` —
inject `cmd` as bytes on the guest's serial input (as if typed at the serial shell), run until a
completion sentinel or the V-time deadline, capture serial output. No protocol with the guest
beyond the shell itself; no determinism guarantee on this path — **by ruling, it does not need
one**. Document the sentinel scheme and its failure modes in `IMPLEMENTATION.md`.

### 2. The taint guard (the airtight part)

- The first `exec` against a live timeline sets its **taint bit**.
- Every snapshot captured from a tainted timeline is tainted; every `branch`/`replay` from a
  tainted snapshot yields a tainted timeline. Taint never clears downstream — an untainted
  state is only reachable from an untainted ancestor.
- **Fail-loud guards:** minting a reproducer from a tainted timeline (`recorded_env` or
  equivalent) → `ControlError::Tainted`. Snapshot replies carry `tainted: bool` so a future
  Archive/donation path (task 64+) can refuse admission without asking. Nothing silently
  succeeds with a lying `Environment`.
- **Fork-first is the usage discipline, not a server rule**: the server does not forbid `exec`
  on any timeline (a caller may deliberately sacrifice one); the taint bit makes the
  consequence structural rather than conventional.

## Acceptance gates

1. **Portable (macOS + Linux):** proptest (≥256) over arbitrary DAGs of
   snapshot/branch/replay/exec: taint propagates exactly along ancestry (never across, never
   cleared), `recorded_env`-on-tainted always errors, untainted lineage is never blocked.
   Exec-session state machine unit-tested against a scripted mock serial.
2. **Box gate — the improvisation:** from a mid-workload Postgres snapshot: `branch` a fork,
   `exec` a real command (e.g. `ps aux` or `ls /`), capture non-empty output; the **original**
   timeline, continued to a later `Moment`, hashes identically to a control run that never
   forked — the improvisation observably cost the search nothing.
3. **Box gate — the guard:** on the exec'd fork, `recorded_env` fails `Tainted`; a snapshot
   taken there reports `tainted: true`; a branch from it also refuses `recorded_env`.
4. Standard suite green on touched crates; existing `live_*` gates byte-identical (the serial
   *input* path must be inert when no exec session is active).

## Box-safety (CRITICAL)

Stock KVM = **1396736**; revert + verify after every run (kill harness → `kvm_intel` users=0 →
rmmod/modprobe → verify on fresh ssh). `taskset -c 2`. Foreground gates only.

## Non-goals

- Determinism of the exec path (ruled out of scope — that is the point); exec as a recorded
  `Action` (rejected ruling, do not implement a hook for it); a guest agent / RPC protocol
  (the serial shell is the v1 transport; a richer channel is a later ruling); `donate`/Archive
  admission (task 64+ consumes the `tainted` flag this task provides); the resolution
  crate/REPL (task 82); any UI.
