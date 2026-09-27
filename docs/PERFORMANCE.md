# Performance

This document gives the fastest rate a search can reach on a workload and a
chip, called its speed of light. It also gives a method for accounting for the
gap between that rate and a measured one. The model and worked examples come
first. The assumptions behind the model follow, then consonance and
dissonance measured against it.

## The model

An execution is one rollout: a restore, a suffix of actions, and the run to an
endpoint. A deterministic search should run its workload at native speed with
the idle time removed. The floor for one execution is the native CPU time of
its new guest work, meaning work no earlier execution ran. The workload's
action durations and the V-time clock's advancement rules set how much guest
work an execution contains. Those are fidelity choices, so the model takes that
work as given.

Everything above the floor is overhead, and each kind has a cost.

| Overhead | Cost |
|---|---|
| Guest exit to the host and back | 2 to 3 µs on bare metal, 10x or more under nested virtualization |
| Page copied into or out of a snapshot | About 1 µs, twice that if hashed |
| Guest work run a second time | Its full CPU time |
| Coordinator time per job | The whole search finishes at most one job in that time |

Two rules of thumb follow from the table. Exits cost 10% of guest time when one
arrives every 25 µs of guest work. Page copies cost 10% when the guest writes a
new page every 10 µs.

A chip's ceiling is the sum of its cores' rates at the floor, with performance
and efficiency cores measured separately. A search confined to one core type
reaches it only with one search per type.

Memory decides how close a search gets to that ceiling. Each worker holds its
guest's resident pages, which limits how many workers a host can run. Each
retained endpoint holds the pages its execution wrote. An emulator snapshot is
tens of kilobytes and a Linux guest's is megabytes. A gigabyte keeps tens of
thousands of the first and on the order of a hundred of the second. When the
retained endpoints fall short of the ones the selector draws, the search runs
work a second time.

## Accounting for the gap

A search's efficiency is its measured rate divided by the chip's ceiling. The
gap is the product of five factors. Each factor is at least one, and a search
at its ceiling has all five at one.

```text
ceiling = measured rate × host overhead × re-execution
                        × cores in use × worker waiting × contention
```

| Factor | Ratio | Measured by |
|---|---|---|
| Host overhead | Worker busy time over guest time | Guest and host cycle counts, and per-phase accounting of exits, pages, and observation |
| Re-execution | Guest work run over new guest work | Virtual time run, split into new and re-run |
| Cores in use | The chip's ceiling over the ceiling of the cores the workers run on | Worker count and core type |
| Worker waiting | Worker wall time over worker busy time | Per-worker timelines, split into waiting for admission and waiting for a reservation |
| Contention | Guest time per unit of new work with every worker running, over that time with one | Guest cycles on a full run against a one-worker run on the same core type |

> [!NOTE]
> **Worked example: an emulator search at its floor.** On an M1 Max
> (Firestorm), the Game Boy core runs Pokémon Blue at 9,904 frames per second
> in a bare loop with no search around it. One search worker runs it at 9,932
> frames per second, so host overhead is one within measurement error. Every
> frame the search ran was one an action requested, so re-execution is one as
> well. A Pokémon Blue snapshot is 58 KiB, so a gigabyte holds about 17,000
> and the archive can keep one for every endpoint. Only cores in use, worker
> waiting, and contention remain.

> [!NOTE]
> **Worked example: one Linux guest search on four core types.** The etcd
> historical case ran as a consonance search with one worker pinned to one
> core. Guest time is the worker's busy time scaled by its share of guest
> cycles. Re-execution is virtual time run over new virtual time.
>
> | | Intel 285HX P-core | Intel 285HX E-core | CIX CP8180 (Cortex-A720) | CIX CP8180 (Cortex-A520) |
> |---|---|---|---|---|
> | New guest time per execution | 60 ms | 87 ms | 215 ms | 838 ms |
> | Ceiling per core | 16.6/s | 11.5/s | 4.7/s | 1.2/s |
> | Measured rate | 2.14/s | 1.19/s | 0.40/s | 0.066/s |
> | Host overhead | 3.2x | 4.0x | 4.9x | 8.2x |
> | Re-execution | 2.4x | 2.4x | 2.4x | 2.2x |
> | Gap | 7.8x | 9.7x | 11.7x | 18x |
>
> One worker was busy for the whole run, so worker waiting and contention are
> one. An E-core ran the same executions at 0.56 of a P-core's rate, and a
> Cortex-A520 at 0.17 of a Cortex-A720's.

> [!NOTE]
> **Worked example: the same search on every core of one type.** On the Intel
> 285HX, eight workers on the P-cores ran 5.6 executions per second, 2.6 times
> one worker. Sixteen workers on the E-cores ran 4.2 per second, 3.5 times one
> worker. Eight workers on the Cortex-A720 cores of a CIX CP8180 ran 1.3 per
> second, 3.2 times one worker.
>
> | Factor | 8 P-cores | 16 E-cores | 8 Cortex-A720 |
> |---|---|---|---|
> | Host overhead | 2.9x | 3.3x | 4.2x |
> | Re-execution | 4.1x | 4.9x | 3.5x |
> | Worker waiting | 1.5x | 1.9x | 1.6x |
> | Contention | 1.4x | 1.4x | 1.3x |
> | Gap to the cores' ceiling | 24x | 44x | 29x |
>
> Re-execution grows with the worker count because each worker keeps its own
> cache of 96 prefixes, so a worker re-runs prefixes other workers already
> ran. Restores also cost more per page with every worker running, which
> points at memory bandwidth as the source of contention. The coordinator
> spent under 0.4 ms per job and waited on results for over 99% of each run,
> so it plays no part in the worker waiting.

## Assumptions behind the costs

### The execution contract

The costs hold for any deterministic search that snapshots and restores a
machine, under four assumptions. Changing one changes the floor.

1. The guest has one vCPU. Several vCPUs need a deterministic order for
   conflicting memory accesses, which adds a cost the model leaves out.
2. The host arbitrates every event that carries nondeterministic input into
   the guest: a read of time, an interrupt, an I/O completion, a draw of
   entropy. Each such event costs one exit.
3. A snapshot boundary preserves all state that can affect later behavior:
   guest RAM, registers, virtual time, pending interrupts, device queues, and
   storage the guest writes outside RAM.
4. The search selects a parent only from results it has admitted, so a
   selection that needs an earlier result waits for it.

### Exits

The floor has one exit per arbitrated event and one per timer deadline an idle
guest wakes for. A syscall is an arbitrated event only when it reads time,
performs I/O, or draws entropy. Exits that only advance the clock are above the
floor. A clock counted in guest memory, with an exit only at a deadline or an
arbitrated event, removes them.

### Pages

Preservation copies each page the guest wrote since the parent into the
snapshot. Restore copies back each page that differs between the worker's
current state and the next parent. That count grows with the distance between
the two states in the tree, so a scheduler that gives a worker parents near its
current state lowers it. Restore by remapping pages replaces each copy with a
translation fault of about the same cost, paid only on pages the guest touches.
Hashing pages for content addressing and scanning memory for written pages are
above the floor.

### The search

- **Critical path.** Under assumption 4, a campaign has a critical path: the
  longest chain of selections that each wait on the result before them.
  Speedup over one worker is at most total work divided by that path, whatever
  the worker count.
- **Ordered admission.** Admitting results in a fixed order delays each result
  until every earlier one completes, and buffering leaves that delay
  unchanged. Workers stay busy while enough results can wait unadmitted and
  the next job does not depend on them.
- **Re-execution against memory.** Take a byte budget, the cost of re-running
  each edge of the tree, the size of each retained state, and the parents the
  selector will draw. The best static choice retains the set within budget
  that minimizes the total cost of re-running from each draw's nearest retained
  ancestor. That cost is zero when the budget covers every drawn state. An
  online policy sees draws only as they arrive, so its cost is at least that
  of the static choice.

## consonance against the model

| Cost | Floor | consonance |
|---|---|---|
| Guest time | Native CPU time of new guest work | The guest runs natively under KVM or HVF. The guest's clock driver and paravirtual devices add instructions. |
| Exits | One per arbitrated event | One per arbitrated event, plus one per execution tick, which the guest takes at every syscall, context switch, and idle-poll turn. On arm64 the guest also exits after every interrupt unmask, and KVM completes each MMIO exit with a second call into the kernel. |
| Restore | Pages that differ from the next parent | Those pages are copied in place. |
| Preservation | Pages written since the parent | KVM's dirty log finds the written pages. HVF has no dirty log, so every boundary captures all of guest RAM. Pages are hashed with BLAKE3 and interned. |
| Whole-state hash | None | SHA-256 over all guest RAM for each checkpoint that requests one. |

Restore and preservation are paid once per snapshot boundary. A workload that
snapshots every action pays them per action, and one that snapshots only the
endpoint pays them per execution.

The execution tick is the largest exit term above the floor, since syscalls and
context switches far outnumber arbitrated events.

> [!NOTE]
> **Worked example.** The etcd search on one Intel 285HX P-core took 75,000
> exits per execution against 148 ms of guest time, one per 2 µs of guest work.
> Most were the execution tick's port writes. The host spent about 2.5 µs per
> exit, 1.3 times the guest time in all. On a CIX CP8180 (Cortex-A720) the
> same search took 262,000 exits per execution at about 5.8 µs each. The
> largest source there was the unmask fence, and each of its MMIO exits
> returned to user space twice. At ten times the cost under nested
> virtualization, this workload's exits would cost about thirteen times its
> guest time.

> [!NOTE]
> **Worked example.** On an M1 Max (Firestorm), the etcd search with 1 GiB of
> guest RAM ran 0.20 executions per second on one worker. Each restore copied
> all 262,144 pages at 2.8 µs per page, and each boundary captured all of them
> at 1.0 µs per page with hashing. Those copies took 96% of the worker's time.
> An Icestorm core at background priority ran the same executions at 0.04 per
> second, with each page costing five to six times as much.

Write-protecting guest pages after each boundary would give HVF a dirty log, at
one fault per first write to a page. Hypervisor.framework allows one VM per
process and a search runs in one process, so a search on macOS uses one core.

## dissonance against the model

### Re-execution

The re-execution factor is guest work run divided by new guest work. It is
counted in execution ticks, since actions vary in length by orders of
magnitude. A requested action whose endpoint is cached is served by a replay
with no guest run.

The archive is a tree of endpoints. A job selects a parent and needs the
machine in that state. If the parent's snapshot is retained, the worker
restores it. Otherwise the worker restores a retained ancestor, the parent's
keyframe, and runs the actions from there again. Three inputs set the factor.

- **Access pattern.** Which parents the selector draws, and in what order. The
  campaign stream records every draw in reservation order.
- **Retention policy.** Which endpoints keep a snapshot, how many, and whether
  workers share them.
- **Cost of keeping one.** The unique bytes in each retained snapshot. A child
  is cheap when its parent is kept, because only its delta is new. Evicting a
  child frees nothing while a retained descendant still references its pages.

A workload that keeps its snapshots in the archive re-runs work only after an
eviction under the logical budget. A workload that caches prefixes per worker
also re-runs work that another worker already ran.

> [!NOTE]
> **Worked example.** An etcd search caches 96 prefixes per worker. Counted in
> virtual time, one worker runs 2.4 times its new guest work, eight run 4.1
> times, and sixteen run 4.9 times. Replaying one campaign stream through
> that cache gives 91,521 actions run for 28,878 new ones. On the same
> stream, an unbounded cache per worker cuts the actions run 2.3x. A cache
> shared across workers cuts them 3.2x, to new work only.

Eviction decisions are part of the recorded campaign and must replay, so a
retention policy uses only deterministic inputs, such as the selector's draw
distribution and each delta's page count.

The floor counts an execution that finds nothing new as new work. Whether the
search chose it well is a question for the search benchmarks.

### Coordinator

The coordinator is serial in two ways.

| Term | Grows with | Floor |
|---|---|---|
| Occupancy per job | Archive size, key cost, stream append and flush | A constant-time draw plus one append |
| Admission wait | Spread in execution time, result slots per worker, window size | Zero while enough results can wait and the next selection does not need them |

Occupancy counts time blocked on the stream sink as well as CPU time, since
either one holds the only coordinator. With occupancy t per job and execution
time T per worker, the coordinator saturates at T divided by t workers. The
shorter an execution, the fewer workers one coordinator can feed.

> [!NOTE]
> **Worked example.** A Pokémon Blue execution is 1,326 frames, or 134 ms on
> an M1 Max (Firestorm). Twenty-four workers at that rate need the coordinator
> to finish each job in under 6 ms. An etcd search on an Intel 285HX took 0.2
> to 0.4 ms per job, and each of its workers took 0.5 to 2 s per execution, so
> one coordinator could feed over a thousand such workers.

Results are admitted in reservation order. Each worker holds one or two result
slots, which free at admission, and the window bounds how far reservations run
ahead. A long job at the front holds every result behind it. Once the workers
behind it fill their slots, they wait. The admission order is part of the
recorded campaign. The wait that more slots cannot remove comes from the
critical path.

> [!NOTE]
> **Worked example.** Faults waits and event holds run from 10 ms to 10.24 s,
> three orders of magnitude apart. A faults search should therefore reach the
> admission wait before coordinator occupancy limits it.

### Memory per campaign

| Term | Grows with | Notes |
|---|---|---|
| Per worker | Guest RAM times workers | Sets how many workers a host admits |
| Shared | Archive entries plus retained snapshots | Grows with the archive when workers share the store, and with the worker count otherwise |
| Result buffers | Unadmitted results per worker | Doubled by a second result slot, and outside the logical budget |
| Reservation pins | Snapshots pinned by outstanding reservations | Charged to the budget until the last reservation that needs them is admitted |

The logical budget and the host's resident size are different numbers. The
logical budget drives eviction, and eviction is recorded, so the budget is
counted deterministically. Eviction runs a bounded number of visits per
admission, so resident bytes can sit above the limit while it catches up. The
kernel enforces resident size. Capacity planning uses the resident peak.

> [!NOTE]
> **Worked example.** A Metroid search with an 8 GiB logical budget peaks at
> 10.2 to 11.0 GB resident, 19% to 28% above the budget. An etcd search with
> eight workers and 1 GiB of guest RAM each started at 8.4 GB resident and
> reached 29 GB after 2,400 executions, as the workers' snapshots accumulated.
> At that size a 62 GB host admits about sixteen workers.

## Scaling

On one chip, the rate should grow linearly with workers until one of these
limits binds.

| Limit | Binds when |
|---|---|
| Memory capacity | Per-worker memory times workers, plus retained snapshots, exceeds host memory |
| Coordinator | Execution time per worker divided by the worker count falls below coordinator time per job |
| Memory bandwidth | Workers times bytes written per second, times about five, exceeds the chip's bandwidth |
| Ready parents | Fewer parents are ready for selection than there are workers |
| Admission wait | Execution times spread widely and workers fill their result slots |
| Per-worker caches | Each worker re-runs prefixes that another worker already ran |

The multiple of five in the bandwidth row covers the guest's write, the copy
into a snapshot, the hash, and the copy back on restore.

A sweep over worker count pins workers to one core type, since performance and
efficiency cores run at different rates. consonance refuses an affinity that
spans core types, so a search on a hybrid chip runs on one type. Changing the
worker count also changes which parents the search selects, so a sweep
compares campaigns of different work as well as different parallelism. Boot,
setup, confirmation replays, and final persistence add fixed time per
campaign, which matters for short campaigns with many workers.

> [!NOTE]
> **Worked example.** The etcd search, in executions per second, on one core
> type per sweep.
>
> | Workers | Intel 285HX P-cores | CIX CP8180 Cortex-A720 |
> |---|---|---|
> | 1 | 2.14 | 0.40 |
> | 2 | 2.75 | 0.74 |
> | 4 | 4.23 | 1.00 |
> | 8 | 5.61 | 1.30 |
>
> Eight workers give 2.6 and 3.2 times one worker. Re-execution from
> per-worker caches, worker waiting, and contention each grow with the worker
> count. The coordinator stays idle for over 99% of each run.

Weak scaling grows the campaign with the worker count. The coordinator limits
it, since its occupancy grows with jobs per second. Guest RAM per worker against
host memory limits it too.

## Instruments

1. Per-phase accounting on every execution: execution ticks split into new and
   re-run, and into busy and idle; guest cycles; exits by reason; pages
   restored and preserved; host time in restore, run, preservation, and
   observation. It gives host overhead and re-execution for one workload.
2. Per-worker timelines that split wall time into executing, waiting for
   admission, and waiting for a reservation, beside the coordinator profile
   enabled with `HARMONY_COORDINATOR_PROFILE=1`.
3. Microbenchmarks of exit cost and page cost per chip and backend.
4. One fixed workload run pinned to each core type, for the per-core rates
   that sum to a chip's ceiling.
5. A trace simulator over the campaign stream. It reports re-execution as a
   function of retained bytes and sharing, and the admission wait as a function
   of workers, result slots, and window, with no guest run. Changing workers or
   window changes later selections, so its answer is a scheduling estimate
   over a fixed trace.
6. Resident memory sampled per phase beside the snapshot store's counters, so a
   peak can be traced to a phase.

## Assumptions

Each cost above rests on an assumption. The check beside it verifies the
assumption on a new chip or workload.

| Assumption | Check |
|---|---|
| An exit costs 2 to 3 µs on bare metal and 10x or more under nested virtualization | Exit microbenchmark per chip and backend |
| A page costs about 1 µs to copy, twice that if hashed | Page microbenchmark per chip |
| Guest cycles match native cycles for the same work | Guest cycles per instruction under the search against the same code run natively |
| An efficiency core runs at a fixed fraction of a performance core's rate | Fixed workload pinned to each core type |
| Per-core rates add across workers | Guest time per unit of new work on a full run against a one-worker run |
| Idle virtual time costs little CPU | Busy and idle split in per-phase accounting |
| A Linux guest's snapshot is megabytes | Pages preserved per boundary in per-phase accounting |
| Coordinator time per job is milliseconds | Coordinator profile on a many-worker run |
| Spread in execution time causes the admission wait | Admission simulation over an existing campaign stream |
| Resident memory above the logical budget is per-worker state, result buffers, and eviction that trails admission | Resident memory per phase |
