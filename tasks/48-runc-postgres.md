# Task 48 — `runc` actually runs the Postgres OCI container, deterministic-twice (no `unshare` workaround)

> **THE money-shot, and task 47's documented gate-3 frontier.** Task 38 already runs the Postgres OCI
> image deterministically — but via a hand-rolled `unshare`/`chroot`/`setpriv` shim, *because* the real
> `runc`/`containerd` Go runtime busy-spins multi-goroutine and deadlocked under a single-vCPU guest
> with no preemption. **Task 47 (PR #15) delivered exactly the missing primitive** — `run_until`
> (PMU-overflow + single-step preemption at the V-time deadline) — so a busy-spinning Go runtime is now
> preempted deterministically. This task **replaces the workaround with the real thing**: `runc` (the
> actual Go binary) launches the Postgres container, the task-42 `gen_random_uuid()`/time workload runs
> against it, and it comes out **bit-identical across two same-seed runs**.
>
> **Depends on task 47 (PR #15, the preemption primitive) being MERGED to main.** DO NOT AUTO-SPAWN
> until it lands — task 48 implements/validates against the live `run_until` path. **Box-only**
> (patched KVM + Intel PMU); pin per `docs/BOX-PINNING.md`; self-serve box gates via git; ALWAYS
> revert KVM to stock + verify.

Read `tasks/00-CONVENTIONS.md`, `tasks/47-deterministic-preemption-timer.md` +
`consonance/vmm-core/IMPLEMENTATION.md` (the preemption primitive + its gate-3 notes), `tasks/38-*`
(the OCI Postgres image + the `unshare` workaround this replaces), `tasks/42-postgres-workload-uuid-time.md`
(the workload), and `harmony-linux/linux/{build-postgres-image.sh,pg-container-run.sh,pg-init.sh}` first.

## Why — the workaround exists only because preemption didn't

Task 38 proved Postgres-in-a-container is deterministic, but it had to **strip the Go runtime**: it
invoked the raw `unshare`/`chroot`/`setpriv` + cgroup-v2 primitives that `runc` *would* invoke, minus
`runc` itself — because `runc`/`containerd` (Go, heavily multi-goroutine) busy-spin with no VM-exit
under a single vCPU and never yield, deadlocking the guest. Task 47's `run_until` makes the V-time
LAPIC timer **preempt** a busy-spinning thread at a deterministic instant, so the Go scheduler runs and
progress happens. **This task uses the real `runc`** — the precise thing the workaround stood in for.

## What to build

1. **Wire the VMM run-loop to the preemption path** for this workload: drive the guest with
   `run_until(next_timer_deadline)` (not bare `run()`) so the busy-spinning runc/containerd/Go threads
   are preempted at the V-time deadline. (Task 47 added the primitive + its run-loop integration; this
   task exercises it on the real container-runtime stack.)
2. **Launch Postgres via real `runc`** inside the guest: the task-38 baked OCI image (PG17 + the
   pre-initdb'd PGDATA on the RAM-backed ext4) run through the actual `runc` binary
   (`runc run <container>`), **not** the `unshare` shim. containerd is optional — `runc` directly is
   the minimal real path; document which you use.
3. **Run the workload + stream it:** the task-42 `gen_random_uuid()` + `clock_timestamp()` insert/select
   loop streams UUIDs + timestamps + a running aggregate to `ttyS0`, `GUEST_READY`, clean poweroff —
   exactly as task 38/42, but now under real `runc`.

## Determinism (the whole point)

- **Preemption is load-bearing:** runc/containerd/the Go runtime busy-spin; `run_until` preempts them at
  the seed-deterministic V-time instant, so the interleaving is a pure function of the seed → bit-identical.
- **Clock → V-time, RNG → seeded CRNG** (unchanged): `gen_random_uuid()`/timestamps come out identical
  across two same-seed runs (the task-42 showcase), different across seeds.
- If anything escapes (a UUID/timestamp differs same-seed, or the Go runtime introduces nondeterminism
  preemption doesn't cover), **report it as a real determinization finding** — don't paper over it.

## Acceptance gates (box)

1. **runc runs Postgres (real, no unshare):** the actual `runc` binary launches the Postgres container
   (no `unshare`/`chroot` shim), Postgres accepts connections, the workload streams to `ttyS0`,
   `GUEST_READY`, clean poweroff. Quote the serial + confirm it went through `runc` (e.g. the runc/Go
   process tree, not the shim).
2. **Deterministic-twice:** two same-seed patched runs are **bit-identical** — serial (incl. the UUIDs +
   timestamps) **and** `state_hash`. Quote the equal digests + a sample UUID/timestamp.
3. **Seed-sensitivity:** a different seed ⇒ different UUIDs/interleaving (quote both).
4. **No regression:** M1/M2/P6 + acceptance-suite + unison goldens byte-identical; standard gates green;
   revert KVM to stock `1396736` + verify.

**If the Go runtime surfaces a genuinely NEW blocker beyond preemption** (a syscall the backend doesn't
determinize, a nondeterministic runc path), implement as far as the primitive carries, prove the gates
that are reachable, and **document the precise next blocker as the frontier** — do not fake a gate.

## Non-goals

Kubernetes (task 49 — this is single-container runc). The `unshare` workaround (this **replaces** it;
keep task 38's path available for comparison but the gate is real `runc`). Multi-container / pv-net /
intra-guest networking (task 49/50). Changing the determinization mechanism (RDTSC/RDRAND/V-time
unchanged — this *exercises* the preemption primitive). No CPU/MSR contract or `state_hash` schema change.
