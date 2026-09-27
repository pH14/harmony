# Performance

This document gives the fastest rate a search can reach on a workload and a
chip, called its speed of light. It also gives a method for accounting for the
gap between that rate and a measured one. The model comes first, then the
accounting, then each cost in turn, then scaling.

## The model

An execution is one rollout: a restore, a suffix of actions, and the run to an
endpoint. A deterministic search should run its workload at native speed with
the idle time removed. The floor for one execution is the native CPU time of
its new guest work, meaning work no earlier execution ran. The workload's
action durations and the virtual clock's rules set how much guest work an
execution contains. Those are fidelity choices, so the model takes that work
as given.

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

A chip's ceiling is the sum of its cores' rates at the floor, with each core
type measured separately.

Memory decides how close a search gets to that ceiling. Each worker holds its
guest's resident pages, which limits how many workers a host can run. Each
retained state holds the pages its execution wrote. An emulator snapshot is
tens of kilobytes and a Linux guest's is megabytes. A gigabyte keeps tens of
thousands of the first and on the order of a hundred of the second. When the
retained states fall short of the ones the search draws, the search runs work
a second time.

## The execution contract

The costs hold for any deterministic search that snapshots and restores a
machine, under four assumptions.

1. The machine has one vCPU. Harmony's determinism claim covers single-vCPU
   machines only ([Determinism](DETERMINISM.md)), so a worker occupies one
   core.
2. The host arbitrates every event that carries nondeterministic input into
   the guest: a read of time, an interrupt, an I/O completion, a draw of
   entropy. Each such event costs one exit.
3. A snapshot preserves all state that can affect later behavior: guest RAM,
   registers, virtual time, pending interrupts, device queues, and storage the
   guest writes outside RAM.
4. The search selects a parent only from results it has admitted, so a
   selection that needs an earlier result waits for it.

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
| Host overhead | Worker busy time over guest time | Guest and host cycle counts, and host time split into exits, pages, and observation |
| Re-execution | Guest work run over new guest work | Virtual time run, split into new and re-run |
| Cores in use | The chip's ceiling over the ceiling of the cores the workers run on | Worker count and core type |
| Worker waiting | Worker wall time over worker busy time | Busy and wall time per worker |
| Contention | Guest time per unit of new work with every worker running, over that time with one | Guest cycles on a full run against a one-worker run on the same core type |

> [!NOTE]
> **Worked example: an emulator search at its floor.** On an M1 Max
> (Firestorm), the Game Boy core runs Pokémon Blue at 9,904 frames per second
> in a bare loop with no search around it. One search worker runs it at 9,932
> frames per second, so host overhead is one within measurement error. Every
> frame the search ran was one an action requested, so re-execution is one as
> well. A Pokémon Blue snapshot is 58 KiB, so a gigabyte holds about 17,000
> and the search can keep one for every endpoint. Only cores in use, worker
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
> retained states, so a worker re-runs prefixes other workers already ran.
> Restores cost more per page with every worker running, which points at
> memory bandwidth as the source of contention. The coordinator spent under
> 0.4 ms per job. Worker waiting includes time a finished result waited for an
> earlier one to be admitted.

## Exits

An exit is a transfer from the guest to the host and back. The floor has one
exit per arbitrated event and one per timer deadline an idle guest wakes for. A
syscall is an arbitrated event only when it reads time, performs I/O, or draws
entropy. Any other exit, such as one that only advances the virtual clock, is
above the floor.

Exit overhead is the exit count times the cost per exit, divided by guest time.
The guest time between exits is the number to compare against the cost of one
exit. At 2.5 µs per exit, an exit every 25 µs of guest work costs 10%, and an
exit every 2.5 µs doubles the time.

> [!NOTE]
> **Worked example.** The etcd search on one Intel 285HX P-core took 75,000
> exits per execution against 148 ms of guest time, one per 2 µs of guest work.
> Most were ticks that advance the virtual clock. The host spent about 2.5 µs
> per exit, 1.3 times the guest time in all. On a CIX CP8180 (Cortex-A720) the
> same search took 262,000 exits per execution at about 5.8 µs each, about
> three times the guest time.

## Pages

A snapshot copies each page the guest wrote since the parent. A restore copies
back each page that differs between the worker's current state and the next
parent. That count grows with the distance between the two states in the tree.
Page overhead is the pages copied per execution times the cost per page,
divided by guest time. Hashing pages and scanning memory to find written pages
add to the cost per page.

> [!NOTE]
> **Worked example.** Hypervisor.framework on macOS has no record of the pages
> a guest wrote, so each restore and each snapshot copies all of guest RAM. On
> an M1 Max (Firestorm), the etcd search with 1 GiB of guest RAM copied all
> 262,144 pages at 2.8 µs per page on restore, and 1.0 µs per page with hashing
> on capture. Those copies took 96% of the worker's time, and one worker ran
> 0.20 executions per second.

## Re-execution

The re-execution factor is guest work run divided by new guest work, counted
in virtual time since actions vary in length by orders of magnitude.

A job selects a parent and needs the machine in that state. If the parent's
state is retained, the worker restores it. Otherwise the worker restores the
nearest retained ancestor and runs the actions from there again. Three inputs
set the factor.

- **Access pattern.** Which parents the search draws, and in what order.
- **Retention.** Which states are retained, how many, and whether workers
  share them.
- **Cost of keeping one.** The unique bytes in each retained state. A child is
  cheap when its parent is retained, because only the pages it wrote are new.

Take a byte budget, the cost of re-running each edge of the tree, the size of
each retained state, and the parents the search will draw. The best offline
schedule knows every draw in advance and may change the retained set between
draws, within budget, to minimize the total cost of re-running from each
draw's nearest retained ancestor. That cost is zero when the budget covers
every drawn state. A search sees draws only as they arrive, so its cost is at
least that of the best offline schedule.

Retention decisions change what a search runs, so a deterministic search makes
them from deterministic inputs only.

The floor counts an execution that finds nothing new as new work. Whether the
search chose it well is a question for the search benchmarks.

> [!NOTE]
> **Worked example.** The etcd search retains 96 prefixes per worker, with no
> sharing between workers. Counted in virtual time, one worker runs 2.4 times
> its new guest work, eight run 4.1 times, and sixteen run 4.9 times.

## Serial work

A deterministic parallel search makes its selection and admission decisions in
one serial order, so that timing cannot change them
([Exploration](EXPLORATION.md)). Three limits follow.

- **Coordinator occupancy.** With coordinator time t per job and execution
  time T per worker, the search saturates at T divided by t workers. The
  shorter an execution, the fewer workers one coordinator can feed.
- **Admission order.** A result waits until every result before it in the
  order is admitted. A slow execution delays the results behind it, and a
  worker waits once it holds as many finished results as it can buffer.
- **Critical path.** Under assumption 4, a campaign has a longest chain of
  selections that each need the result before them. Speedup over one worker is
  at most total work divided by that chain, whatever the worker count.

> [!NOTE]
> **Worked example.** A Pokémon Blue execution is 1,326 frames, or 134 ms on
> an M1 Max (Firestorm). Twenty-four workers at that rate need the coordinator
> to finish each job in under 6 ms. An etcd search on an Intel 285HX took 0.2
> to 0.4 ms per job, and each of its workers took 0.5 to 2 s per execution, so
> one coordinator can feed over a thousand such workers.

## Memory

Each worker holds its guest's resident pages. Retained states hold the pages
their executions wrote. Workers that do not share retained states each hold
their own, so memory grows with the worker count as well as with the search.
Capacity planning uses the resident peak.

> [!NOTE]
> **Worked example.** An etcd search with eight workers and 1 GiB of guest RAM
> each started at 8.4 GB resident and reached 29 GB after 2,400 executions, as
> the workers' retained states accumulated. At that size a 62 GB host admits
> about sixteen workers.

## Scaling

On one chip, the rate should grow linearly with workers until one of these
limits binds.

| Limit | Binds when |
|---|---|
| Memory capacity | Per-worker memory times workers, plus retained states, exceeds host memory |
| Coordinator occupancy | Execution time per worker divided by the worker count falls below coordinator time per job |
| Memory bandwidth | Workers times bytes written per second, times about five, exceeds the chip's bandwidth |
| Ready parents | Fewer parents are ready for selection than there are workers |
| Admission order | Execution times spread widely and workers fill their result buffers |
| Unshared retained states | Each worker re-runs prefixes that another worker already ran |

The multiple of five in the bandwidth row covers the guest's write, the read
and write that copy a page into a snapshot, and the read and write that copy
it back on restore.

A sweep over worker count pins workers to one core type, since core types run
at different rates. Changing the worker count also changes which parents the
search selects, so a sweep compares campaigns of different work as well as
different parallelism. Fixed time per campaign, such as boot and final
persistence, matters for short campaigns with many workers.

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
> Eight workers give 2.6 and 3.2 times one worker. Re-execution from unshared
> retained states, worker waiting, and contention each grow with the worker
> count. The coordinator stays under 0.4 ms per job.

Weak scaling grows the campaign with the worker count. Coordinator occupancy
limits it, since the coordinator's load grows with jobs per second. Guest RAM
per worker against host memory limits it too.

## Checks

Each cost above rests on an assumption. The check beside it verifies the
assumption on a new chip or workload.

| Assumption | Check |
|---|---|
| An exit costs 2 to 3 µs on bare metal and 10x or more under nested virtualization | Exit microbenchmark per chip and backend |
| A page costs about 1 µs to copy, twice that if hashed | Page microbenchmark per chip |
| Guest cycles match native cycles for the same work | Guest cycles per instruction against the same code run natively |
| Each core type runs at a fixed fraction of another type's rate | One workload pinned to each core type |
| Per-core rates add across workers | Guest time per unit of new work on a full run against a one-worker run |
| Idle virtual time costs little CPU | Host busy time against idle virtual time per execution |
| A Linux guest's snapshot is megabytes | Pages copied per snapshot |
| Coordinator time per job is milliseconds or less | Coordinator time per job on a many-worker run |
