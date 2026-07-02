# bugs/toys — small real systems with planted bugs

Toy but *honest* implementations of real protocols and system components, each shipped with one
or more **planted bugs** reachable only under injected adversity. Where `category/` isolates a
fault type, a toy exercises the finder against a real protocol state space — multiple
processes, an invariant checker as the oracle, and bugs that require *coordinated* conditions
(a crash in a specific protocol phase, a partition during an election).

Entries are named after the **system**. Planted-bug variants live inside the entry (one buggy
variant per known mistake), alongside a correct baseline that must survive the same campaign
clean — the baseline is the nominal control at protocol scale.

## Intended first wave

- `raft/` — a small Raft with the canonical implementation mistakes as selectable variants:
  `votedFor` not persisted before replying (double-vote after restart), commit-by-count of
  prior-term entries (§5.4.2 violation), election timer reset on invalid AppendEntries.
  Oracle: single-leader-per-term + committed-log-prefix invariants checked by a monitor
  process. Runs as N processes in one guest now; moves to the net-fault boundary when task 61
  lands.
- `two-phase-commit/` — coordinator with a crash window between deciding commit and logging
  it; participants diverge. A showcase for Moment-addressed kill search.
- `lock-service/` — lease-based lock without fencing tokens; a paused-then-resumed client
  writes with a stale lease. Trigger is a long preemption gap — a fault surface unique to a
  deterministic hypervisor.
- `mvcc-kv/` — claims serializable, permits write skew. Oracle: isolation checker (drives
  task 75's oracle work).

## Requirements per entry

- A correct baseline and ≥1 buggy variant; the campaign must find the variant's bug and pass
  the baseline clean.
- Invariant checkers live with the toy but are written to be reusable (they graduate into the
  shared oracle library, task 75).
- Each variant's spec documents the planted mistake, the triggering condition, and expected
  branches-to-find.
