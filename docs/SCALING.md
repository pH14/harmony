# Scaling

This document defines how strong and weak scaling apply to Harmony's
components. It uses the standard HPC meanings: strong scaling holds the
total problem fixed and adds processors; weak scaling grows the problem
proportionally with processors. The unit of processing is a physical worker
thread (one OS thread running one target instance).

## Consonance

A consonance machine is single-vCPU by design. One execution timeline is
one ordered sequence of deterministic transitions on one processor; there
is no intra-machine parallelism to scale.

### Strong scaling

Adding processors does not accelerate a single machine execution.
Consonance's transition rate on a timeline is determined by one core's
single-threaded performance: exit handling, device models, memory hashing,
and snapshot operations. The serial fraction is 1. Strong scaling efficiency
across processors is zero for one machine.

### Weak scaling

Each additional processor can run an independent machine instance.
Instances share no mutable state during execution. The snapshot store
uses content-addressed pages that are read-shared across forks but written
independently through copy-on-write layers. Weak scaling is near-linear:
N processors execute N independent timelines at close to the throughput of
one, bounded by host memory bandwidth, TLB pressure from large guest
mappings, and snapshot-store page interning under concurrent forks.

The practical weak-scaling limit is memory. Each machine instance carries
guest RAM, vCPU state, device state, and snapshot layers. Consonance-backed
targets are much more expensive per snapshot than native emulator targets;
the default one-result-per-worker buffering exists to bound this cost.

## Dissonance

Dissonance decomposes a search campaign into a serial coordinator and
parallel worker execution. The coordinator owns selection, mutation
derivation, admission, archive maintenance, stream recording, and memory
budgeting. Workers own target execution: restore, apply actions, observe,
and return results.

### Strong scaling

The total problem is a fixed campaign: a declared execution budget (number
of rollouts or admitted work). Adding workers distributes the execution
phase across more threads.

The coordinator is the serial bottleneck. For each job in the campaign it
performs, in order:

1. **Selection and mutation** — choose a parent, derive a mutation seed,
   expand a suffix. Cost is proportional to archive depth and policy
   complexity, not worker count.
2. **Admission** — process the completed result into the archive. Cost is
   proportional to archive operations (retention, replacement, memory
   compaction), not worker count.
3. **Stream recording** — append the job record. Constant per job.

These coordinator phases are inherently serial and executed in deterministic
reservation order, not completion order. The coordinator's wall time per
job is the strong-scaling bottleneck. As worker count grows, the execution
phase shrinks toward zero and campaign throughput asymptotes at the
coordinator's job-processing rate. Profiling fields (`selection_ns`,
`admission_ns`, `bookkeeping_ns`, `stream_write_ns`, `receive_wait_ns`)
measure these phases directly.

Amdahl's law applies: if coordinator overhead is fraction `s` of total
single-worker campaign time, maximum speedup is `1/s`. For native emulator
targets where each rollout is microseconds of emulation, the coordinator
fraction is larger and strong scaling saturates earlier. For
consonance-backed targets where each rollout is milliseconds of VM
execution, the coordinator fraction is smaller and strong scaling extends
further before saturating.

The admission window (`workers * reservations_per_worker`) bounds the
number of in-flight jobs. With two reservations per worker, a fast worker
can begin its next reserved job while its completed result awaits ordered
admission. This overlaps execution with coordinator wait time and was
measured to improve throughput by 32–42% at 24 workers for native targets,
with negligible RSS increase. The window does not eliminate the serial
coordinator; it hides its latency when execution is the dominant cost.

### Weak scaling

The problem grows with worker count: more workers means more rollouts
explored in the same wall time, expanding coverage of the search space.
Each worker executes independently; the coordinator processes results in
reservation order regardless of which worker finished first.

Weak scaling efficiency depends on whether the coordinator's per-job cost
grows with archive size. Selection walks the archive's group hierarchy;
admission may trigger retention replacement and memory compaction. As the
archive fills under a fixed memory budget, compaction cost can grow.
However, the archive is bounded and compaction amortizes, so per-job
coordinator cost is bounded and weak scaling remains efficient up to the
point where every worker is starved waiting for the coordinator.

Memory is the weak-scaling constraint. Each additional worker holds one
(or two, with two-slot buffering) completed-but-unadmitted result,
including any target snapshot. These results are outside the archive's
logical memory budget and are measured only in host RSS. For
consonance-backed targets, each buffered result includes a whole-VM
snapshot (gigabyte-class guest RAM layers), making the per-worker memory
overhead substantial.

## Harmony as a whole

At the system level, Harmony's scaling is the composition of dissonance's
campaign loop driving consonance (or native emulator) target instances.

### Strong scaling

For a fixed campaign (declared execution budget, fixed workload and seed),
adding workers accelerates completion. The serial fraction is the
coordinator plus any single-machine bottleneck (snapshot hashing, memory
compaction, stream I/O). Measured 24-worker campaigns achieve hundreds of
thousands of admitted frames per second for native targets. The
coordinator's `receive_wait_ns` approaching zero indicates the workers are
no longer the bottleneck.

Parallelism is entirely at the search level: multiple independent
executions of different branches from different archive states. There is
no within-execution parallelism. This means strong scaling cannot reduce
the latency of any individual rollout, only the number of rollouts
completed per unit time.

### Weak scaling

Adding workers to explore more of the state space in the same wall time is
Harmony's primary scaling mode. Each worker runs an independent target
instance with its own snapshot, actions, and observations. The coordinator
feeds them from a shared archive and ingests their results.

Weak scaling is effective because:

- execution is embarrassingly parallel across independent branches;
- the coordinator's per-job cost is bounded by the fixed archive capacity;
- deterministic reservation ordering means adding workers changes wall
  time, not the recorded campaign stream (given sufficient budget).

Weak scaling is bounded by:

- **memory**: archive budget plus per-worker snapshot buffering;
- **coordinator throughput**: the serial job-processing rate caps how many
  workers can stay busy;
- **host resources**: memory bandwidth, cache, and I/O contention as worker
  count approaches the physical core count;
- **target cost**: consonance-backed targets have higher per-worker memory
  and CPU cost than native emulator targets, so the same host supports
  fewer consonance workers.

### Scaling identity

The deterministic admission model gives Harmony an unusual scaling
property: adding workers does not change the logical campaign result. The
same seed, budget, and workload produce the same recorded stream
regardless of worker count (absent a wall-time cutoff). Workers affect
throughput and wall time, not the search decisions or archive contents.
This makes scaling a pure performance concern: more cores finish sooner or
explore more, but the deterministic record is the same sequence either way.
