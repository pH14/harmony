# bugs — the end-to-end bug collection

Workloads with **known bugs** that Harmony's finder (dissonance) is expected to catch. This is
the finder-validation corpus: prove the finder against
seeded bugs with known ground truth before investing in search cleverness. A single planted bug is
the first consumer; this directory generalizes it into a permanent regression
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
   `consonance/harmony-linux/linux/` conventions).
2. **Fault surface** — which Harmony dimension triggers it: timing perturbation
   (vtime), entropy values, host-plane faults, kill/restart at a Moment
   (snapshot/branch), block-layer faults (future), net faults (future).
3. **Oracle** — how a hit is detected: crash marker on serial, integrity
   check after restart, invariant-checker process, isolation checker. No
   human-in-the-loop oracles.

## Entry conventions

Each entry is a directory containing:

- `README.md` — the spec: the bug (mechanism-level), the triple above, trigger conditions,
  **expected difficulty** (order-of-magnitude branches-to-find), the
  **tunable knob** if difficulty is adjustable, and provenance links for `historical/` entries.
- The workload source / image recipe, once implemented.
- Declared clean trajectories for entries whose fault surface admits a no-fault configuration,
  so the false-positive rate on that configuration is measured rather than assumed. A different
  release of the software is not such a configuration: a recorded input can execute differently
  there, so a clean run on it says nothing about the scenario the entry exercises.

Harnesses use portable checks where their logic is portable and hardware checks
for checks that require `/dev/kvm`.

Ground truth is sacred: for `historical/` entries, affected versions, trigger, and fix commit
must be verified against primary sources (the issue, the fixing commit, the postmortem) and
cited in the entry README — never from memory.

## Interleaving campaigns

The built category and toy cases pair one concurrency mistake with its correct
variant. Their image recipes use static C executables on `scratch`, plus the
composed event runtime at the path required by faults image admission. They
fit 256 MiB guests. `interleaving.h` supplies shared assertion emission, kernel
knob parsing, loop pacing and competing instrumented sites; the planted logic
and each oracle remain in the case's C file. It never selects search actions.
An event park stops a thread only at an instrumented site, so every planted
window contains an instrumented call, usually a `noinline` helper's entry.

Build and run from the repository root, with a compatible guest runtime and
a CLI that has the host's virtualization permission:

```sh
python3 workloads/bugs/interleaving.py --case lost-update --build \
  --cli /path/to/harmony --kernel /path/to/Image \
  --initramfs /path/to/initramfs-oci.cpio.gz \
  --seeds 1 2 3 --executions 5000 --ram-mib 256
```

On macOS, build `harmony-cli` in release mode, copy its executable under
`target/`, and sign that copy with
`consonance/vmm-backend/hvf.entitlements.plist`. Re-sign after rebuilding it.
The Docker driver need only support `docker save`; the runner exports each
image to `target/interleaving/images/`. Images use `linux/arm64`; matching the
guest kernel architecture is required.

The runner selects controls before buggy variants for each case. Use repeated
`--case` options to select cases, `--variant` to select one arm, and
`--noise 0 8 32` for a difficulty sweep. It holds a local lock and runs exactly
one search at a time. A search uses four workers; on an HVF host, reserve those
four VM slots and avoid any overlapping VM search, including one outside this
runner. Every invocation creates a new campaign and unique run directories
under `target/`; existing run records are never overwritten or committed.

The printed table includes the first failing execution, elapsed wall time,
replay confirmation, result and violated assertion. JSONL records also retain
commands, achieved execution counts, event-park evidence and search time.
Controls pass only after their full budget, with the case oracle observed and
zero violations. Unexpected node exits, missing oracles, infrastructure
failures and unconfirmed hits cannot pass. A buggy miss remains a miss; inspect
its park sites, read evidence and reached state before changing the workload
or searcher. A landing alone does not establish that its hold was sufficient.

`aba-reuse` and `stale-cache` are held-out cases: evaluate both variants, but
never use either to tune the searcher. Keep measurements in PR descriptions
and run artifacts. `python3 workloads/bugs/interleaving.py --self-test` checks
the runner's control, replay and infrastructure-failure decisions without VMs.
