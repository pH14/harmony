# Performance

How fast could a search run on a given chip, and where does the time go when
it falls short? We estimate that ceiling from the native CPU time of the
workload, then account for the gap using measurements of host overhead,
repeated work, and worker utilization. The worked examples are measurements
from specific hardware and implementations; they illustrate the model and
will change as the system evolves.

## The model

An execution is one rollout: the worker restores a state, applies a sequence
of actions, and runs to an endpoint. Ideally, it would run at native speed,
skip idle time, and reuse all work from earlier executions. Its minimum cost,
or floor, is therefore the native CPU time needed for new guest work.

The amount of guest work depends on action durations and the rules of the
virtual clock. These choices determine how faithfully the search models the
workload; we take them as given when estimating performance.

Actual executions also pay for exits, snapshots, repeated work, and
coordination:

| Overhead | Cost |
|---|---|
| Guest exit to the host and back | 2 to 3 µs on bare metal, 10x or more under nested virtualization |
| Page copied into or out of a snapshot | About 1 µs, twice that if hashed |
| Guest work run a second time | Its full CPU time |
| Coordinator time per job | The whole search finishes at most one job in that time |

At these costs, an exit every 25 µs of guest work adds about 10% overhead.
Copying a page every 10 µs adds another 10%.

To estimate the whole chip's ceiling, measure the floor on each core type and
add up the resulting per-core execution rates. Reaching that ceiling also
requires enough memory to keep the cores busy and avoid repeating work. Each
worker needs memory for its guest, and each retained state needs space for the
pages changed by its execution. A typical emulator snapshot takes tens of
kilobytes, while a Linux guest snapshot takes megabytes: a gigabyte can hold
tens of thousands of emulator snapshots but only around a hundred Linux
snapshots. When a selected state is no longer in memory, the search has to
reconstruct it by running earlier work again.

## The execution contract

This model applies to a deterministic search that snapshots and restores a
machine, with four assumptions:

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

We measure efficiency as the actual execution rate divided by the chip's
ceiling. Five factors account for the gap between them:

```text
ceiling = measured rate × host overhead × re-execution
                        × cores in use × worker waiting × contention
```

In this model, each factor is at least one. All five must be one for the
search to reach the ceiling.

| Factor | Ratio | Measured by |
|---|---|---|
| Host overhead | Worker busy time over guest time | Guest and host cycle counts, and host time split into exits, pages, and observation |
| Re-execution | Guest work run over new guest work | Virtual time run, split into new and re-run |
| Cores in use | The chip's ceiling over the ceiling of the cores the workers run on | Worker count and core type |
| Worker waiting | Worker wall time over worker busy time | Busy and wall time per worker |
| Contention | Guest time per unit of new work with every worker running, over that time with one | Guest cycles on a full run against a one-worker run on the same core type |

> [!NOTE]
> **Worked example: Pokémon Blue at native speed.**
>
> On an M1 Max (Firestorm), the Game Boy core runs Pokémon Blue at 9,904 frames
> per second in a bare loop and 9,932 frames per second inside one search worker.
> Within measurement error, the search adds no host overhead. It also repeats no
> guest work: every frame it runs belongs to a requested action. Snapshots are
> small enough to retain every endpoint, at 58 KiB each, or about 17,000 per
> gigabyte. That leaves core utilization, worker waiting, and contention to
> explain any gap when scaling to the whole chip.

> [!NOTE]
> **Worked example: etcd on one core.**
>
> For the etcd historical case, we ran a Consonance search with one worker
> pinned to a core of each type. We estimated guest time by multiplying the
> worker's busy time by the fraction of cycles spent in the guest, and measured
> re-execution as total virtual time run divided by new virtual time.
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
> Each worker stayed busy throughout its run, so the worker-waiting factor is
> one. These single-worker runs also provide the baseline for contention. The
> E-core achieved 0.56 of the P-core's execution rate, while the Cortex-A520
> achieved 0.17 of the Cortex-A720's rate.

> [!NOTE]
> **Worked example: etcd on every core of one type.**
>
> Adding workers improved throughput, but the gains fell well short of linear
> scaling. On the Intel 285HX, eight P-core workers reached 5.6 executions per
> second, a 2.6x speedup over one worker. Sixteen E-core workers reached 4.2 per
> second, a 3.5x speedup. On the CIX CP8180, eight Cortex-A720 workers reached
> 1.3 per second, a 3.2x speedup.
>
> | Factor | 8 P-cores | 16 E-cores | 8 Cortex-A720 |
> |---|---|---|---|
> | Host overhead | 2.9x | 3.3x | 4.2x |
> | Re-execution | 4.1x | 4.9x | 3.5x |
> | Worker waiting | 1.5x | 1.9x | 1.6x |
> | Contention | 1.4x | 1.4x | 1.3x |
> | Gap to the cores' ceiling | 24x | 44x | 29x |
>
> Re-execution increased because workers keep separate sets of retained states
> and repeat prefixes already run by other workers. Restores also became more
> expensive per page, suggesting contention for memory bandwidth. Although the
> coordinator spent less than 0.4 ms per job, workers still waited; this includes
> time spent holding finished results until earlier results could be admitted.

## Exits

An exit transfers control from the guest to the host and back. At minimum,
the guest must exit whenever the host arbitrates an event or an idle guest
wakes for a timer deadline. A syscall needs arbitration when it reads time,
performs I/O, or draws entropy. Other exits, including those used only to
advance the virtual clock, add to this minimum.

The cost depends on how often the guest exits relative to the work it does
between exits. Multiply the exit count by the cost per exit, then divide by
guest time to get the overhead. At 2.5 µs per exit, exiting every 25 µs of
guest work adds 10%; exiting every 2.5 µs doubles the total time.

> [!NOTE]
> **Worked example: etcd exit costs.**
>
> The etcd search on one Intel 285HX P-core made 75,000 exits per execution
> during 148 ms of guest time, or roughly one exit every 2 µs. Most were ticks
> used to advance the virtual clock. At about 2.5 µs per exit, the host spent
> 1.3 times as long handling exits as the guest spent running. On a CIX CP8180
> (Cortex-A720), the same search made 262,000 exits per execution at about
> 5.8 µs each, costing roughly three times the guest time.

## Pages

Capturing a snapshot requires copying the pages the guest has written since
its parent state. Restoring a state requires copying the pages that differ
between the worker's current state and the next parent. The farther apart
those states are in the tree, the more pages generally need to be copied.

As with exits, page overhead is the number of copies per execution multiplied
by the cost of each copy, divided by guest time. Hashing pages or scanning
memory to find changed pages adds to that cost.

> [!NOTE]
> **Worked example: snapshot copies on macOS.**
>
> The macOS backend illustrates how expensive this can become without a record
> of changed pages. With Hypervisor.framework, each restore and snapshot copies
> all of guest RAM. In the etcd search on an M1 Max (Firestorm), that meant
> copying all 262,144 pages of a 1 GiB guest: 2.8 µs per page to restore and
> 1.0 µs per page to capture, including hashing. These copies consumed 96% of
> the worker's time, limiting it to 0.20 executions per second.

## Re-execution

Before a worker can execute a job, it needs to restore the selected parent
state. If that state is still retained, the worker can restore it directly.
Otherwise, it restores the nearest retained ancestor and repeats the actions
needed to reach the parent.

The re-execution factor measures this extra work as total guest work divided
by new guest work. We count both in virtual time because action lengths vary
by orders of magnitude. The amount of repeated work depends on three things:

- **Access pattern:** which parents the search selects and in what order.
- **Retention:** which states it keeps, how many fit, and whether workers
  share them.
- **State size:** how many unique bytes each retained state needs. Keeping a
  child is cheap when its parent is already retained, since only changed pages
  need additional space.

A useful lower bound is the best possible retention schedule for a known
sequence of parent selections. Given a memory budget, state sizes, and the
cost of repeating each edge in the tree, this schedule could choose which
states to retain between selections to minimize repeated work. If every
selected state fits, that cost is zero. A live search cannot know future
selections, so it can only match or exceed this offline cost.

Retention decisions affect what gets executed and must therefore use only
deterministic inputs. Also, “new work” here means work that has not already
been run, even if it discovers nothing useful. Search benchmarks evaluate
whether that work was worth choosing.

> [!NOTE]
> **Worked example: repeated work in etcd.**
>
> The etcd search retains 96 prefixes per worker without sharing them between
> workers. Measured in virtual time, total guest work is 2.4 times new work with
> one worker, 4.1 times with eight, and 4.9 times with sixteen.

## Serial work

A deterministic parallel search orders its selection and admission decisions
so that execution timing cannot change them (see [Exploration](EXPLORATION.md)).
This introduces three limits to parallelism:

- **Coordinator occupancy.** With coordinator time t per job and execution
  time T per worker, the search saturates at T divided by t workers. The
  shorter an execution, the fewer workers one coordinator can feed.
- **Admission order.** A result waits until every result before it in the
  order is admitted. A slow execution delays the results behind it, and a
  worker waits once it holds as many finished results as it can buffer.
- **Critical path.** Because a selection can depend on an earlier admitted
  result, some jobs must run in sequence. The longest such chain limits
  speedup to total work divided by the work along that chain, regardless of
  how many workers are available.

> [!NOTE]
> **Worked example: coordinator capacity.**
>
> For Pokémon Blue, an execution takes 1,326 frames, or 134 ms on an M1 Max
> (Firestorm). To keep twenty-four workers busy at that rate, the coordinator
> would need to process each job in under 6 ms. The etcd search leaves much more
> room: on an Intel 285HX, coordination took 0.2 to 0.4 ms per job while worker
> executions took 0.5 to 2 s. At those timings, coordinator occupancy alone
> would allow over a thousand workers.

## Memory

Memory use grows both as workers are added and as they accumulate retained
states. Each worker holds its guest's resident pages plus any snapshots it
retains. When workers do not share snapshots, each pays that storage cost
separately, so capacity planning needs to account for peak resident memory.

> [!NOTE]
> **Worked example: etcd memory use.**
>
> The etcd search with eight workers and 1 GiB of guest RAM each started at
> 8.4 GB resident and reached 29 GB after 2,400 executions. At that footprint,
> a 62 GB host has room for about sixteen workers.

## Scaling

On a single chip, throughput should rise linearly with worker count until it
runs into one of these limits:

| Limit | What stops scaling |
|---|---|
| Memory capacity | Per-worker memory times workers, plus retained states, exceeds host memory |
| Coordinator occupancy | Execution time per worker divided by the worker count falls below coordinator time per job |
| Memory bandwidth | Workers times bytes written per second, times about five, exceeds the chip's bandwidth |
| Ready parents | Fewer parents are ready for selection than there are workers |
| Admission order | Execution times spread widely and workers fill their result buffers |
| Unshared retained states | Each worker re-runs prefixes that another worker already ran |

The bandwidth estimate counts roughly five memory transfers for each guest
write: the write itself, a read and write to capture the page, and another
read and write to restore it.

When measuring scaling, pin workers to one core type so differences in core
speed do not distort the comparison. Even then, changing the worker count
also changes which parents the search selects, so the runs differ in both
work and parallelism. Boot time and final persistence can also dominate short
campaigns spread across many workers.

> [!NOTE]
> **Worked example: etcd scaling.**
>
> For the etcd search, a sweep on each core type produced these execution rates
> per second:
>
> | Workers | Intel 285HX P-cores | CIX CP8180 Cortex-A720 |
> |---|---|---|
> | 1 | 2.14 | 0.40 |
> | 2 | 2.75 | 0.74 |
> | 4 | 4.23 | 1.00 |
> | 8 | 5.61 | 1.30 |
>
> Eight workers achieved 2.6x and 3.2x the throughput of one worker on the Intel
> and CIX chips, respectively. Re-execution, worker waiting, and contention all
> increased with worker count, while coordinator time stayed below 0.4 ms per
> job.

Weak scaling increases the campaign size along with the worker count. This
still runs into coordinator capacity as jobs per second increase, and memory
capacity as more guests need to fit on the host.

## Checks

The estimates above depend on the chip, backend, and workload. Use these
measurements to check the assumptions when applying the model elsewhere:

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
