# Performance

How fast could a search run on a given chip, and where does the time go when
it falls short? Estimate that ceiling from the native CPU time of the workload,
then account for the gap using host overhead, repeated work and worker
utilization. The cost ranges and arithmetic examples below are illustrative
model assumptions, not a baseline for the current executable. Measure them for
the chip, backend and workload being evaluated.

## Measurement boundaries

Measure a fixed logical job separately from a whole campaign. Restore,
extension, capture and import exercise different paths. Report whether an
extension uses a fused operation or separate apply and capture calls; their
summed timings do not establish fused-operation throughput. Include creation,
boot, prefix construction and final persistence in the campaign total, or
state explicitly which phase excludes them.

A retained parent can restore without replaying historical actions. A missing
parent may require an ancestor restore and reconstruction. Record the actual
source, selected depth, reconstruction work and executing worker before
interpreting locality or restore cost. A direct-target import experiment does
not measure adaptive selection, admission or campaign effectiveness.

### Counter ownership

Cumulative work counters and live residency diagnostics answer different
questions. Snapshot work counters without walking retained snapshots for
ownership queries. Request residency diagnostics explicitly, and preserve an
unknown charge as unavailable. Work collection still has a fixed guarded
worker telemetry exchange; record that observer effect rather than subtracting
an estimated cost from elapsed time.

- Campaign target totals already include worker profiles. Shared index,
  launcher and retired coordinator profiles have separate owners.
- Sending and receiving a descriptor observe the same transfer. Extent
  references, distinct backing allocations, FD handles, mapped views, copied
  bytes and resident memory are distinct quantities. Repeated allocation
  identity does not establish unique physical RAM or persistent registration.
- Guard, SDK callback, VM run and import timers can nest. Concurrent worker
  times cannot be added to obtain campaign wall time. Event shape witnesses
  are subsets of opcode totals, not additional work or semantic outcomes.
- Logical ticks and normalized history are separate from physical backend
  entries. Callback counts describe actual callback sites, not an assumed
  request-to-response ratio or successful response writes.
- Report counter overflow, saturation and missing coverage. A successful run
  with no reported errors does not qualify failing-path observation retention.

### Integrity and callback phases

Retained memory can change outside tracked writes. Fresh integrity checks must
therefore remain at their operation boundaries. A cached digest or a previously
verified page ID cannot authorize current bytes. A sealed executable file does
not make private live code mappings immutable; matching live and backing read
streams does not establish that either still matches admission.

A sparse import resolves its extent chain before importing it. Resident page
verification occurs before the user service-factory callback and again after
it. Keep the verification sets separate: the callback can change retained
memory. Repeated resident hash bytes can represent these distinct fresh phases,
not necessarily duplicate imports or amplification with parent depth. Measure
phase-specific work before attributing a cause. A faster membership data
structure may reduce bookkeeping while retaining every required page hash.

### Comparable experiments

Bind raw results to source, executable, toolchain, lock files, workload inputs,
backend and admission receipts. Preserve the complete typed stream and logical
report. Compare equivalent paths within the same binary; a different build
needs its own reference and cannot inherit an earlier replay oracle. Record
host-only runtime qualification separately from an exact runtime-input match.
Keep run records, measured tables and raw output outside the repository; put
the numbers supporting a change and their limitations in its pull request.

Hold logical work, seed, admission window, RAM and cache budgets fixed when
varying workers. Compare complete streams and logical reports, and record
actual reservations and worker assignments. Measure parent reuse as well as
root-only work. Distinguish performance from useful outcomes: a faster fixed
job does not establish better search effectiveness. Independent campaigns on
separate hosts do not preserve one campaign's global ordering.

Use repeated, bounded trials and report their spread, run order and host load.
Pin equivalent core types and include a single-worker reference for each core
used. Separate cold, warm and constrained-cache cases. Sampled PSS is neither a
guaranteed peak nor total cache ownership; account for unmapped cache backing
and use cgroup memory for campaign capacity. Preserve owned-session exit
receipts and sampled process identities, and refuse uncertain cleanup instead
of treating leader exit as proof that all workers stopped.

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

A search report's `telemetry` records the inputs to each factor: worker busy,
idle, and scheduler wait time, host time by exit site and snapshot operation,
and new against re-run work in both host and virtual time. Estimate guest time
as worker busy time multiplied by the fraction of cycles spent in the guest.

## Exits

An exit transfers control from the guest to the host and back. At minimum,
the guest must exit whenever the host arbitrates an event or an idle guest
wakes for a timer deadline. A syscall needs arbitration when it reads time,
performs I/O, or draws entropy. Other exits, including those used only to
advance the virtual clock, add to this minimum.

The cost depends on how often the guest exits relative to the work it does
between exits. Multiply the exit count by the cost per exit, then divide by
guest time to get the overhead. At 2.5 µs per exit, exiting every 25 µs of
guest work adds 10%; exiting every 2.5 µs doubles the total time. Attribute physical exits and SDK scheduling exchanges by operation before
identifying a dominant cost. Coverage exchanges select scheduling decisions
and the next threshold; they are distinct from logical kernel ticks. Callback
shape alone does not establish that register publication dominates.

## Pages

Capturing a snapshot requires copying the pages the guest has written since
its parent state. Restoring a state requires copying the pages that differ
between the worker's current state and the next parent. The farther apart
those states are in the tree, the more pages generally need to be copied.

As with exits, page overhead is the number of copies per execution multiplied
by the cost of each copy, divided by guest time. Hashing pages or scanning
memory to find changed pages adds to that cost. A backend without a record of
changed pages copies all of guest RAM on every restore and snapshot, which
for a 1 GiB guest is 262,144 pages. A restore copies the fewest pages when the
store still holds the image the VM is on, so the store keeps that image while
the VM uses it.

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

Workers that retain states separately re-run prefixes that other workers
already ran, so re-execution grows with the worker count. One snapshot cache
shared by all workers removes that repeat when the selected states fit in its
budget.

Retention decisions affect what gets executed and must therefore use only
deterministic inputs. Also, “new work” here means work that has not already
been run, even if it discovers nothing useful. Search benchmarks evaluate
whether that work was worth choosing.

## Serial work

A deterministic parallel search orders its selection and admission decisions
so that execution timing cannot change them (see [Exploration](EXPLORATION.md)).
This introduces three limits to parallelism:

- **Coordinator occupancy.** With coordinator time t per job and execution
  time T per worker, the search saturates at T divided by t workers. The
  shorter an execution, the fewer workers one coordinator can feed.
- **Admission order.** Results are admitted in planned-finish order, which
  sums each job's action costs. A result waits until every result with an
  earlier planned finish is admitted, so an execution that runs slower than
  its planned cost delays the results behind it. Jobs
  count against one bound from dispatch until admission, and an idle worker
  waits once that bound is full.
- **Critical path.** Because a selection can depend on an earlier admitted
  result, some jobs must run in sequence. The longest such chain limits
  speedup to total work divided by the work along that chain, regardless of
  how many workers are available.

Short executions leave the coordinator little time per job: with 134 ms
executions, keeping 24 workers busy needs coordinator time under 6 ms per job.
When execution times vary, allowing several reserved jobs and finished results
per worker keeps workers busy while earlier results wait for admission.

## Memory

Memory use grows both as workers are added and as they accumulate retained
states. Each worker holds its guest's resident pages plus any snapshots it
retains. When workers do not share snapshots, each pays that storage cost
separately, so capacity planning needs to account for peak resident memory.

A search sizes itself from the memory its cgroup allows, or the host's free
memory outside a cgroup ([`harmony search`](../cli/README.md)). Each worker
needs room for its guest. The remainder is one budget shared by the snapshot
cache and every worker's snapshot store. The shared cache lives in memfd files
that process RSS omits, so measure a search's memory from its cgroup.

## Scaling

On a single chip, throughput should rise linearly with worker count until it
runs into one of these limits:

| Limit | What stops scaling |
|---|---|
| Memory capacity | Per-worker memory times workers, plus retained states, exceeds host memory |
| Coordinator occupancy | Execution time per worker divided by the worker count falls below coordinator time per job |
| Memory bandwidth | Workers times bytes written per second, times about five, exceeds the chip's bandwidth |
| Ready parents | Fewer parents are ready for selection than there are workers |
| Admission order | Execution times depart from planned costs and workers fill their result buffers |
| Unshared retained states | Each worker re-runs prefixes that another worker already ran |

The bandwidth estimate counts roughly five memory transfers for each guest
write: the write itself, a read and write to capture the page, and another
read and write to restore it.

When measuring scaling, pin workers to one core type so differences in core
speed do not distort the comparison. With a fixed admission window the search
selects the same parents at every worker count, so the runs differ only in
parallelism. Cores of one type can run at different clock speeds, and a
one-worker run gets the fastest, so compare against one-worker runs on each
core the full run uses. Boot time and final persistence can also dominate
short campaigns spread across many workers.

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
