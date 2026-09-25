# Faults workload

`faults-workload` searches a distributed workload for bugs that only appear
when nodes die, stall, or restart at the wrong moment. The workload runs
unmodified inside one deterministic VM; every fault is enforced from inside the
guest by the platform supervisor, and the whole run is a Consonance session, so
a bug reproduces from its action list alone.

## The image contract

A workload image is an OCI image carrying one extra file, `/etc/harmony/bundle`,
in the platform supervisor's bundle format:

| line | meaning |
|---|---|
| `node <name> <argv...>` | one workload process the supervisor supervises |
| `hook <id> <argv...>` | a command the search can run at any moment |
| `setup <argv...>` | runs once, before any node starts |
| `ready <argv...>` | must pass before setup is sealed and before new hooks launch after a supervised node start |
| `workload <argv...>` | one long-lived workload driver started after initial readiness |
| `check <argv...>` | a short-lived oracle command run continuously after initial readiness |

[`prepare`](src/prepare.rs) stages that image, reads the bundle for the action
alphabet, and passes the image to the canonical OCI preparation API with
`/etc/harmony/bundle` selected for structured supervision. The platform control
member contains the read-only execution specification and mounts the pinned
supervisor, SDK devices, and bundle path. The supervisor runs setup and
readiness commands with the resolved image credentials before it publishes the
setup point, then owns the node process groups and hook launches.

After setup, readiness probes run asynchronously while standing-fault polling
continues. The configured command decides readiness, including whether it can
operate with some nodes down. Already-running hooks continue reporting their
assertions across restarts; readiness only checks new launches. Bundles without
a readiness command keep immediate hook launches.

## Actions

Every action records its own duration in 10 ms guest ticks, from 10 ms through
10.24 seconds, and its window lasts that long ([`target`](src/target.rs)). The
search draws one duration per suffix from the adaptive duration policy, and
every action in the suffix takes it:

| action | effect |
|---|---|
| `Wait(ticks)` | the workload runs undisturbed |
| `EventKill(node, rarity, ticks)` | an instrumented runtime kills the node at a selected event, reporting the claimed site before termination |
| `EventPark(node, edges, hold, ticks)` | an instrumented runtime holds the thread whose instrumented edge brings the count since arming to `edges`, for the hold, then counts again from zero until the window closes; each edge counts one over its site's visit count, `edges` is drawn log-uniform from 1 through `(1 << 14) - 1`, and the hold is drawn log-uniform in whole ticks from one tick through the window |
| `Kill(node, ticks)` | the node stays down for the window |
| `Pause(node, ticks)` | the node is stopped for the window, then continued |
| `Restart(node, ticks)` | the node is killed and comes back after a quarter of the window, at least one tick |
| `Hook(id, ticks)` | the supervisor runs that hook once |
| `Interrupt(vector, ticks)` | a host-plane interrupt is staged at the window start, or at the parent endpoint's snapshot moment when that moment is past the window start |

Each fault action except `Interrupt` becomes a standing-fault window on the shared
[`fault-policy`](../fault-policy) wire form. The package answers the platform supervisor's
standing poll with the windows whose half-open span contains the polling
moment, so an input is fully described by its encoded window list and one
branch installs it. A hold still running when an event-park window closes
continues after the window, so the next actions can overlap the held thread.
The park counts as a pending fault until that hold ends.

## Execution

[`consonance`](src/consonance.rs) drives one `consonance_client::session::Session`
per evaluator thread. Each portable action prefix maps to a real whole-VM
snapshot: the session branches its parent under the prefix's window list and
the host-plane effect its last action stages, runs to the action's horizon
deadline, and snapshots the exact stopped endpoint. A terminal stop is recorded
with its original stop and has no successor. If a continuable endpoint cannot
be snapshotted, the session is abandoned and the control diagnostic is
reported. A bounded LRU keeps recent prefixes resident and rebuilds evicted
ones from their longest cached ancestor.
The shared session watchdog follows deterministic virtual-time progress, so a
slowly advancing instrumented guest can finish a long action while one stuck
at a virtual moment still times out. Replay can apply explicit, bounded Wait
actions to obtain current quiescent check evidence; these actions and ticks
are reported separately from the recorded input actions.

A campaign never encodes the virtual-time trace, so the session is configured
to defer sparse checkpoint hashing. Each due checkpoint would otherwise hash
all of a gigabyte-class guest's RAM inside the run that reached it, starting
with the boot that reaches setup.

[`campaign`](src/campaign.rs) implements the game-neutral campaign interface
over that target, and [`archive`](src/archive.rs) supplies the endpoint key,
which captures assertion, liveness, in-flight work, event-firing state, faults
still in effect, and bucketed edge coverage. The edge-digest register sums a hash of every
(edge, hit-count bucket) pair that instrumented nodes have entered. It is
part of the holder identity, so an execution that drives any edge into a new
bucket opens a new slot inside its lifecycle place.
Workloads without the C runtime report a digest of zero. The
raw instrumented site reported by an event kill remains diagnostic evidence; it
is not archive novelty because a large instrumented binary can report a
distinct address at nearly every endpoint.

Assertions arrive as Antithesis SDK JSON records written to `/dev/harmony`.
The host keys each assertion by its `id` (the message when the id is empty)
and keeps its kind, message, and source location. An Always or
AlwaysOrUnreachable assertion evaluated false, or an Unreachable assertion that
was reached, is a violation and marks the execution as a bug. The set of
Sometimes and Reachable assertions that passed enters the archive key as a
count plus a digest of the sorted ids, so the key has no limit on how many
distinct assertions a workload declares. Every process's records feed the key.
`campaign-summary.json` lists every assertion the campaign saw under
`assertions`. A Sometimes or Reachable assertion that was declared but never
passed is a campaign failure: it appears under `never_satisfied` in both
`campaign-summary.json` and `report.json`, and the search prints one
`FAIL: assertion never satisfied` line for each.
`park_sites` in `campaign-summary.json` counts event-park landings by site.
`park_thresholds` counts, for each `floor(log2(edges))`, the park actions the
guest ran at that threshold and the landings at that threshold, so the landings
per action at each threshold show which part of the drawn range a workload's
executions reach. A park re-arms after each hold, so one action can land more
than once. On
the SQLite WAL reset and etcd cases no park with a threshold of `1 << 14` or
more fired, which sets the top of the drawn range.

The generic `execution_work` counter and `report.json`
`execution_ticks` count the guest ticks requested by successfully applied actions
after setup. This logical counter is monotonic across target reset and snapshot
restore; longer waits cost more even when an endpoint is cached. The separate
`guest_horizons` diagnostic counts action evaluations that enter the guest;
cache reuse can change the count and reset clears it. Setup, prefix
reconstruction, and failed actions are outside the logical counter. Replay
continues an enabled continuous checker with waits of 10 ms through 40.96 s,
doubling only while its completed generation is stale or faults remain pending.
`actions_applied` remains the recorded input prefix, while `settle_actions` and
`settle_ticks` account for that deterministic validation tail.

## macOS

Hypervisor.framework allows one virtual machine per process, so a macOS run
takes one worker per process. A worker releases its target when it finishes so
the next one can boot a virtual machine in that process.

## Running it

```
harmony search --package faults IMAGE.oci --backend consonance \
    --kernel vmlinux --base-initramfs initramfs.cpio.gz \
    --seed 1 --workers 8 --executions 100000 \
    --actions 12 --ram-mib 1024 --out run/
harmony search --package faults IMAGE.oci --backend consonance \
    --kernel vmlinux --base-initramfs initramfs.cpio.gz \
    --replay run/bug-1.json --repeat 10 --out confirm/
```

Both modes write `report.json` ([`package`](src/package.rs)) with the pinned
image and kernel hashes, the execution identity, the run bounds, and
either the bugs found or the replay outcomes.

New replay outcomes encode the engine's 32-byte state digest directly as
lowercase hex and mark it with `state_hash_encoding: "engine_digest"`.
Reports written by earlier versions omit that marker and contain SHA-256 of
the digest; readers default a missing marker to the legacy interpretation.
The marker is additive, so older readers continue to parse the report shape.

The search report and `campaign-summary.json` also record
`watchdog_cutoffs`, the number of guest action runs ended by the session's
host watchdog, including cutoffs while reconstructing an evicted prefix. A
reconstruction cutoff ends that preparation attempt, counts one execution failure
and one watchdog cutoff, and leaves the worker available for later jobs. It
produces no suffix action, retained candidate, oracle evidence, or duration
feedback. Other reconstruction errors still fail the campaign. A completed CLI
with such cutoffs remains a measured campaign;
an outer CLI timeout is an infrastructure failure. The reports also expose
`execution_failures`; the nightly check rejects non-watchdog failures. A supervisor
runtime error has a separate SDK status and cannot turn a PID 1 exit into bug
evidence.

The searcher learns the duration of waits and instrumented event holds from
admitted campaign outcomes and logical execution cost. It continues sampling
short and long logarithmic durations while favoring durations whose own action
recently produced useful work. The adapter groups this feedback by node liveness
and whether hooks, workload, checks, or event faults have progressed. Site
identities and workload-specific concepts do not enter the duration policy.
Choices and feedback state are recorded in the campaign stream; input replay
executes the recorded durations directly.

Instrumentation actions become available automatically when the staged image
contains the runtime bridge, nonempty event symbols, and an executable whose
hash matches its instrumentation attestation. A runtime hello identifies the
ready nodes in each incarnation; event faults select only those nodes, so mixed
instrumented and uninstrumented bundles share the same search policy. A step
drawn from the retained-input table is held to the same rule and falls through
to the alphabet when its node is not ready, so the readiness a run recorded
bounds every draw rather than the alphabet alone. Backend capabilities determine
whether host interrupt actions are available. Unsupported alternatives are
excluded from the alphabet. The adapter supplies that alphabet and the duration
each drawn action carries; the searcher owns the suffix draw and the
retained-input table. The continuous client and oracle remain image-owned
commands; the oracle decides when its observations are conclusive, including
when some nodes are down.

Every replay run boots a session no earlier run has touched, so no snapshot
another run cached can stand in for guest execution: each run reaches the
sealed setup point and executes the recorded actions itself. Each run records
the actions it applied beside the horizons it ran in the guest, and the two are
equal when nothing came from a cache. A search replays every bug it records the
same way, and reports the bug as confirmed only when the replay reproduced the
evidence the campaign saw: the assertions it violated, or the same stop when
the stop was the only evidence. `bug_found` and `first_bug_execution` come from
the confirmed bugs, so a hit that no replay reproduced is reported and does not
count as a rediscovery.

Search also writes
`campaign-summary.json`, `stream.jsonl`, `progress.jsonl`,
`first-bug-input.json`, and one `bug-N.json` per recorded bug
([`report`](src/report.rs)); each of those carries the action list and the
encoded window list that reproduces it.

`FaultArchiveKey` has a place, a progress, and a holder identity. The place is
the passed assertion set and every lifecycle count except liveness, plus two
flags for faults still in effect when the state is saved: a thread held by an
event park on any node, and an event kill armed on any node. Liveness and the
edge digest are the holder identity inside the place. The progress is the
number of Sometimes and Reachable assertions the state has passed. The
selector's tiers rank states by that count and draw most parents from the
states that passed the most. Places inside a tier rank by their draw counts.

The Consonance backend needs Linux and KVM. The action model, the bundle
parser, the archive key, the image preparation and the report shapes are
portable and tested everywhere.

Replay summaries include completed-check provenance automatically for bundles
with a continuous `check`. The supervisor records the check run, its pid, and
the disturbance generations at its start and completion. The host adds the
Sometimes and Reachable assertions that pid passed during the run, and the
pending process faults. A case can require evidence from a successful check that
started and finished in the final generation with no outstanding fault.
Assertions passed anywhere else remain exploration evidence and cannot establish
this recovery condition.
Bundles that use drawn hooks have `check: null`; their evidence comes from those
hooks instead.
