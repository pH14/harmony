# bugs/category — one canonical fault type per test

Minimal, purpose-built tests where each entry isolates **one bug type** and is **named after
the fault**. These are the unit tests of the finder: the smallest workload that exhibits the
fault, the single fault surface that triggers it, the cheapest oracle that detects it. When a
search-strategy change lands, this directory answers "which *classes* of bug did we get better
or worse at?"

## Naming

`<fault-name>/` — kebab-case, the fault itself, not the workload. Examples of the intended
first wave (each becomes a directory with a spec when picked up):

| entry | fault surface | oracle sketch |
|---|---|---|
| `missing-fsync/` | kill at a Moment between write and (absent) fsync | recovery-time integrity check on a WAL-toy KV store |
| `missing-dirfsync/` | kill after rename, directory entry never durable | file missing after restart |
| `torn-write/` | kill mid multi-block write, no checksum on the record | recovery reads a half-old/half-new record |
| `missed-wakeup/` | preemption timing (condvar wait without predicate loop) | worker hangs → watchdog marker |
| `aba-reuse/` | SMP interleaving on a lock-free queue | corruption → crash marker |
| `non-idempotent-retry/` | host fault mid-transaction, retry double-applies | balance invariant violated |
| `entropy-branch/` | rare entropy value (tunable prefix match) | crash marker (task-42/60 pattern) |
| `stale-lease/` | long preemption gap between lease check and use | fencing invariant violated |
| `clock-step/` | vtime perturbation (wall clock steps back / timestamps collide) | assertion marker |

## Requirements per entry

- **Deterministically triggerable**: right `(seed, fault schedule)` ⇒ fires every time;
  nominal control ⇒ never (task-60 discipline).
- **One fault type only.** If a test needs two coordinated faults, it's a `toys/` entry.
- **Tunable difficulty** where the fault admits it (entropy prefix length, race-window width),
  so one entry serves as both smoke test and search benchmark.
- Spec records expected branches-to-find at the default knob setting.
