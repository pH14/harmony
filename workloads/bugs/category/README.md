# workloads/bugs/category — one canonical fault type per test

Minimal, purpose-built tests where each entry isolates **one bug type** and is **named after
the fault**. These are the unit tests of the finder: the smallest workload that exhibits the
fault, the single fault surface that triggers it, the cheapest oracle that detects it. When a
search-strategy change lands, this directory answers "which *classes* of bug did we get better
or worse at?"

## Built interleaving cases

| entry | fault surface | correct variant | oracle |
|---|---|---|---|
| `lost-update/` | park between reading and writing a counter | atomic increment | finished increments remain counted |
| `torn-read/` | park during a sequenced record copy | final sequence recheck | copied fields match |
| `duplicate-request/` | switch between lookup and insertion | insert-and-apply compare-and-swap | each key is applied at most once |
| `missed-wakeup/` | park between empty check and wait | predicate loop under mutex | queued item never loses its signal |
| `stale-lease/` | park past lease expiry before a store write | store fencing token | accepted write tokens never regress |
| `aba-reuse/` | park before stale stack compare-and-swap | tagged head | owned node is never resurrected |

`aba-reuse` is held out from searcher tuning. The collection README describes
the sequential runner and its control-before-buggy campaign order.

## Naming and planned cases

`<fault-name>/` — kebab-case, the fault itself, not the workload. Examples of the intended
first wave (each becomes a directory with a spec when picked up):

| entry | fault surface | oracle sketch |
|---|---|---|
| `missing-fsync/` | kill at a Moment between write and (absent) fsync | recovery-time integrity check on a WAL-toy KV store |
| `missing-dirfsync/` | kill after rename, directory entry never durable | file missing after restart |
| `torn-write/` | kill mid multi-block write, no checksum on the record | recovery reads a half-old/half-new record |
| `non-idempotent-retry/` | host fault mid-transaction, retry double-applies | balance invariant violated |
| `entropy-branch/` | rare entropy value (tunable prefix match) | crash marker |
| `clock-step/` | vtime perturbation (wall clock steps back / timestamps collide) | assertion marker |

## Requirements per entry

- **Deterministically triggerable**: right `(seed, fault schedule)` ⇒ fires every time;
  nominal control ⇒ never.
- **One fault type only.** If a test needs two coordinated faults, it's a `toys/` entry.
- **Tunable difficulty** where the fault admits it (entropy prefix length, race-window width),
  so one entry serves as both smoke test and search benchmark.
- Spec records expected branches-to-find at the default knob setting.
