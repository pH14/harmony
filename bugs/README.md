# bugs — the end-to-end bug collection

Workloads with **known bugs** that Harmony's finder (dissonance) is expected to catch. This is
the fuzzer-validation corpus called for in `docs/REVIEW-2026-07.md`: prove the finder against
seeded bugs with known ground truth before investing in search cleverness. Task 60 is the first
consumer (a single planted bug); this directory generalizes it into a permanent regression
suite for the *finder* — when consonance/dissonance improve, the collection measures whether
finding actually got better.

## Layout

| dir | what lives here | named after |
|---|---|---|
| `category/` | minimal single-fault tests — one canonical bug *type* each (missing fsync, torn write, missed wakeup, …) | the **fault** |
| `toys/` | small but real systems with planted bugs (buggy Raft, 2PC with a crash window, …) | the **system** |
| `historical/` | real FOSS software at a pinned pre-fix version, reproducing a documented real-world bug | the **system + bug** |

## Every entry is a triple

A bug that cannot be expressed this way does not belong in the collection:

1. **Workload** — what runs in the guest (payload, container image, or init script; reuse the
   `consonance/acceptance-suite/payloads/` and `harmony-linux/linux/` conventions).
2. **Fault surface** — which Harmony dimension triggers it: timing/interrupt perturbation
   (vtime), entropy values, host-plane faults (task 59), kill/restart at a Moment
   (snapshot/branch), block-layer faults (future), net faults (task 61, future).
3. **Oracle** — how a hit is detected: crash marker on serial (the task-60 path), integrity
   check after restart, invariant-checker process, isolation checker (task 75). No
   human-in-the-loop oracles.

## Entry conventions

Each entry is a directory containing:

- `README.md` — the spec: the bug (mechanism-level), the triple above, trigger conditions,
  **expected difficulty** (order-of-magnitude branches-to-find, per the task-60 gate), the
  **tunable knob** if difficulty is adjustable, and provenance links for `historical/` entries.
- The workload source / image recipe, once implemented.
- A **nominal control**: every entry must define a no-fault configuration under which the bug
  never fires. False-positive rate is measured, not assumed.

Rules of the house apply (`tasks/00-CONVENTIONS.md`): determinism discipline in any host-side
harness code, dependency whitelist, portable gates where logic is portable, box gates for
anything needing `/dev/kvm`.

Ground truth is sacred: for `historical/` entries, affected versions, trigger, and fix commit
must be verified against primary sources (the issue, the fixing commit, the postmortem) and
cited in the entry README — never from memory.
