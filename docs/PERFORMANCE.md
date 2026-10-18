# Performance

How fast could a search run on a given chip, and where does the time go when
it falls short? We estimate that ceiling from the native CPU time of the
workload, then account for the gap using measurements of host overhead,
repeated work, and worker utilization.

The worked examples below are historical measurements, not a baseline for
current executables. Integrity checks, snapshot handling and observation costs
have changed since those runs. The [October 18 measurements](#october-18-2026-measurements)
identify their exact prepared builds and distinguish measured work from proposed
optimizations.

## October 18, 2026 measurements

These experiments address the findings tracked in [#745](https://github.com/pH14/harmony/issues/745).
The combined source builds were prepared branches when measured; individual
changes ship through separate reviewed PRs. Neither experiment compares old and
new builds, and neither establishes a general campaign speedup.

### Retained parent depth

Source `0be4f443003edabb4860d81945587204672e6dab` was measured on an Intel
Core Ultra 9 285HX with Linux KVM, the etcd workload, a 1 GiB guest and an
8 GiB cache budget. Both modes used the same executable, one sample at each depth and no warmup.
The IPC mode used actual worker processes and descriptor transfers. Boot and
prefix construction were excluded from the phase timings.

| Parent depth | Local restore ms | Worker restore ms | Local fresh import ms | Worker fresh import ms |
|---:|---:|---:|---:|---:|
| 1 | 240.279 | 266.193 | 697.479 | 731.707 |
| 8 | 241.341 | 267.566 | 707.251 | 748.849 |
| 32 | 242.591 | 266.733 | 719.560 | 761.850 |
| 128 | 241.098 | 268.808 | 732.870 | 779.255 |
| 1,024 | 245.129 | 271.708 | 779.723 | 825.282 |

Every restore and imported restore performed one endpoint restore, one
observation and one whole-request hash of 1,073,741,824 RAM bytes. None replayed
historical actions or fetched or decoded old SDK events. Imported restores also
performed a separate portable-import target hash and full page validation.
Complete snapshot, certificate, observation and normalized-history comparisons
passed across the equivalent paths. There were no watchdogs, cache refusals or
evictions in the ten samples.

This confirms bounded historical restore work for these retained parents. It
does not make every operation constant-time: imported payload, metadata and
validation work can grow. At depth 1,024, an import copied 40,124,416 bytes into
the worker Store and mapped 32 serialized ranges totaling 148,578,304 bytes.
Those mappings and copies are separate from the one whole-request hash.

Diagnostic ownership queries in this build added RPCs and guard checks outside
the timed phases. Its raw guard and transport counter deltas therefore do not
isolate intrinsic restore cost. The prepared follow-up tracked in
[#763](https://github.com/pH14/harmony/issues/763) separates
work-counter snapshots from those residency queries. No inferred subtraction
was used to manufacture corrected timings.

### Fixed logical work with one and two workers

Source `431ba486bb000f51761785584536ae2422549acf` includes the observation
separation and coordinator-session cleanup. One run with one worker and one run
with two workers used the same binary and etcd workload, seed 7, eight jobs, a
fixed admission window of 16, no warmup, 1 GiB per guest and an 8 GiB shared
cache budget.
Both runs used coordinator CPU 0. The one-worker run used worker CPU 1; the
two-worker run used worker CPUs 1 and 2. These were distinct physical cores in
the same P-core group and NUMA node; the external observer used CPU 3.

| Measurement | One worker | Two workers |
|---|---:|---:|
| Search wall time | 181.530 s | 105.205 s |
| Campaign wall time | 209.273 s | 133.045 s |
| Jobs | 8 | 8 (4 per worker) |
| New logical guest ticks | 2,960 | 2,960 |
| Completed-result buffer peak | 1 | 2 |
| Observed owned-session PSS peak | 1,649,364 KiB | 2,743,499 KiB |

The complete output stream was byte-identical, and the entire logical report
was equal. Normal correctness oracles remained enabled. Both external process
supervisors reported exit zero, no timeout, no cleanup error and no owned
leftovers; postflight checks found no remaining observed process identities or
KVM users. An earlier calibration with leftover descendants was refused and
was not used as the reference.

All eight jobs started at the root and ran one new action lasting 4 to 1,024
logical ticks. This fixed job is not the execution unit in the historical etcd
worked example below; their rates are not comparable. No parent import,
reconstruction or replay occurred, and no useful objective witness was found.
The result tests fixed-work equivalence and provides one timing observation;
it does not test adaptive descendant selection, parent locality, search
effectiveness, NUMA routing or a larger job budget. Sequential run order,
thermal state, caches and host load remain confounders. CPU 2 has no separate
one-worker reference. PSS is a sampled contemporaneous sum, not an allocation bound or guaranteed peak; one final
two-worker memory sample was unavailable after process exit. Unmapped cache
memfd pages are outside process PSS, so it does not measure total cache memory
or total host memory. The shared index charged 181,239,808 bytes in each run.

### Remaining measured costs and accounting rules

Worker-lifetime SDK request and response guard timers totaled 164.007 seconds
with one worker and 173.807 seconds across two workers. These include worker
startup, overlap VM execution scopes, and run concurrently across workers.
They identify substantial guard work, but are not additive wall-time fractions.
Local service answering can remove an outer Decision RPC while retaining these
SDK guards. Sealed executable files do not establish immutable private live
code mappings, so they do not justify deleting live-byte checks.

Shared cache lock hold time was approximately 0.13 milliseconds in each run;
coordinator admission took 2.802 and 0.375 milliseconds respectively. These small
root-only cases do not support cache-lock restructuring or background persistence as the next
bottleneck fix. They also contain no imported-parent evidence.

Per-range backing identity was not measured in the retained-depth experiment.
Final index statistics and the allocator structure provide a structural upper
bound: at depth 8, eight imported ranges remained owned by an index with one
live backing segment and no eviction. At depths 32 and 128, 32 imported ranges
were bounded by two and eight total live segments respectively. The allocator
uses one backing owner per segment and retains it while extents remain live.
These inferred bounds support per-import descriptor deduplication. They do not
establish persistent registration reuse,
fewer mappings, fewer Store copies or immutable shared page ownership. At depth
1,024, the index had 73 segments, so these aggregate statistics do not determine
the number of distinct backings in its 32-range import.

Counter interpretation must preserve ownership and scope:

- Campaign target totals already sum worker profiles. Shared index and launcher
  counters are separate owners, not additional worker copies.
- Sending and receiving the same descriptor are two observations of one
  transfer. Serialized bytes, copied bytes, mapped span and resident memory
  measure different things.
- Guard, VM run and exit timers nest. Concurrent worker timers cannot be added
  to obtain campaign wall time.
- Logical ticks and normalized history are not physical backend entry counts
  or native guest instruction counts.

Arbitrary untracked changes to retained memory still require fresh integrity
verification. An incremental commitment needs an enforced immutable-object and
writer boundary, not merely a cached root.

A proposed SHA-256 main-prefix reuse for the host executable guard was rejected.
A synthetic counterexample using the exact comparison helper changed the current
live and sealed read streams identically. Their equality did not establish that
those bytes still matched admission: the cached prefix retained the old identity,
while fresh main hashing changed it. Fresh main-code hashing therefore remains
required. This was a synthetic proof, not physical fault injection or a benchmark.

### Provenance and qualification limits

The retained-depth executable SHA-256 is
`75b9a15e366ff06b2da701077dc1b70c5f5a45c251a764167b418d4e62cb77a7`.
The fixed-work executable SHA-256 is
`7cb48f4554fc7ee5083dec314ef07fe880083c6ba4fa61bf402197147e9d6589`.
The fixed-work stream SHA-256 is
`a6a1a7d58816d88b84f59f89b54997fb3274caca88f56cf76758115c00e2ac8d`.
The accepted one-worker and two-worker evidence-bundle manifest hashes are
`2dcaf0aaf81e20478ba053b3e8c754d5fedf3850b61ccb7d82420bdbd42c1710`
and `d5affb593636102c2f3f447d112fbe39a9f0caf9d8574719b7f65d23ac4fa127`.
Raw bundles and independent audits are not published with this document. They
are retained locally under `/tmp/consonance-foundation-hardware` and
`/tmp/consonance-fixed-work-scaling-lifecycle-v3`, with remote source, binaries
and runtime inputs on ms02 under `/var/tmp/consonance-foundation-0be4f44` and
`/var/tmp/consonance-fixed-work-scaling-lifecycle-v3-431ba486`. These are task
retention locations, not durable public downloads. Issue #745 records shipping
and acceptance decisions. The hashes identify evidence, not a standalone
reproduction recipe.

Normal sealed executable and workload admission remained active. Existing guest
runtime artifacts passed the normal verifier with **host-only** qualification:
the current source key differs from the artifact key by a host SHA-2 dependency
lock change. No new guest runtime was built, relabeled or exempted from admission.
These results do not claim exact runtime-input qualification or cross-build
replay equivalence. Warm and constrained caches, RAM-size variation, repeated
seed panels, retained-parent scaling and useful-outcome measurements remain
necessary before broader performance conclusions.

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
guest work adds 10%; exiting every 2.5 µs doubles the total time. In a Linux
guest, most exits are ticks that advance the virtual clock, and a register the
guest writes each time it unmasks interrupts can cost more than any device.

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

> [!NOTE]
> **Worked example: etcd on seven Cortex-A720 workers.**
>
> On a CIX CP8180, seven Cortex-A720 workers run the etcd search at about
> 27,700 executions per hour, and one worker runs about 6,000. The shared
> snapshot cache holds every selected state, so re-execution is 1.00, and
> restores take 5% of busy time. Seven workers run 4.6 times one. Most of the
> remaining gap comes from the chip:
>
> | Setup | Guest time per exit |
> |---|---|
> | One search alone on a 2.6 GHz core | 13.2 µs |
> | One search alone on a 2.2 GHz core | 15.7 µs |
> | Four one-worker searches at once | 16.0 to 18.5 µs |
> | One four-worker search on the same cores | 16.9 µs |
>
> The chip's Cortex-A720 cores run at 2.2 to 2.6 GHz, and one worker gets the
> fastest. Separate guests slow each other by about 20% through shared caches
> and memory; a four-worker search is no slower than four separate searches.

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
