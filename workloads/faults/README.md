# Faults workload

`faults-workload` searches a distributed workload for bugs that only appear
when nodes die, stall, or restart at the wrong moment. The workload runs
unmodified inside one deterministic VM; every fault is enforced from inside the
guest by [`harmony-fault-agent`](../fault-agent), and the whole run is a
Consonance session, so a bug reproduces from its action list alone.

## The image contract

A workload image is an OCI image carrying one extra file, `/etc/harmony/bundle`,
in the fault agent's bundle format:

| line | meaning |
|---|---|
| `node <name> <argv...>` | one workload process the agent supervises |
| `hook <id> <argv...>` | a command the search can run at any moment |
| `setup <argv...>` | runs once, before any node starts |
| `ready <argv...>` | must pass before the run's setup point is sealed |
| `workload <argv...>` | load started once after ready and never restarted |
| `check <argv...>` | an oracle the agent reruns on its own cadence |

[`prepare`](src/prepare.rs) stages that image, reads the bundle for the action
alphabet, and assembles a guest initramfs: the base image, the OCI rootfs, and
a control member holding the static fault agent and this package's init. The
`EventKill` and `EventPark` actions are added automatically only when the image
contains the
Antithesis runtime bridge, generated symbol metadata, and the build's
instrumented-node hash attestation; this capability is part of the recorded
vocabulary used by replay. The
init mounts the pseudo-filesystems, binds and chroots into the workload rootfs,
and execs the agent. The control member is appended after the compressed
members and padded to four bytes, which Linux initramfs requires before a raw
`newc` header.

## Actions

An input is a list of actions laid end to end over guest time
([`target`](src/target.rs)). Every action but `Wait` runs for one horizon:

| action | effect |
|---|---|
| `Wait(scale)` | nothing; the workload runs undisturbed for `1 << scale` horizons, scale 0 to 7 |
| `Kill(node)` | the node stays down for the whole horizon |
| `EventKill(node, ordinal)` | the instrumented runtime kills the node synchronously at a deterministic event ordinal; the arm stands until it fires or the input ends |
| `Pause(node, ticks)` | the node is stopped, then continued inside the horizon |
| `Restart(node)` | the node is killed and comes back inside the horizon |
| `Hook(id)` | the agent runs that hook once |
| `EventPark(node, rarity, hold)` | the instrumented runtime holds one thread of the node for `hold` at the first callback after the arm whose own site has been visited at most `1 << rarity` times |
| `Park(node, addr, hits, hold)` | guest threads are held at an execution place |
| `Interrupt(vector)` | a host-plane interrupt is staged at the window start, or at the parent endpoint's seal when settling carried it past that start |

Every action but `Interrupt` becomes a standing-fault window on the shared
[`fault-policy`](../fault-policy) wire form. The package answers the agent's
standing poll with the windows whose half-open span contains the polling
moment, so an input is fully described by its encoded window list and one
branch installs it.

## Execution

[`consonance`](src/consonance.rs) drives one `consonance_client::session::Session`
per evaluator thread. Each portable action prefix maps to a real whole-VM
snapshot: the session branches its parent under the prefix's window list and
the host-plane effect its last action stages, runs to the action's horizon
deadline, and seals the endpoint. An endpoint the session cannot seal within
its settle allowance has no successor and the search records it as dead; one
whose guest stopped for good while settling is recorded with that stop. A
bounded LRU keeps recent prefixes resident and rebuilds evicted ones from their
longest cached ancestor.

A campaign never encodes the virtual-time trace, so the session is configured
to defer sparse checkpoint hashing. Each due checkpoint would otherwise hash
all of a gigabyte-class guest's RAM inside the run that reached it, starting
with the boot that reaches setup.

[`campaign`](src/campaign.rs) implements the game-neutral campaign interface
over that target, and [`archive`](src/archive.rs) supplies the endpoint key,
which pairs the sometimes-assertion set with node liveness, unexpected deaths,
EventKill-fired outcomes, hook progress, and whether the workload process was
still running. A check that finishes without emitting an assertion reached no
verdict, so conclusive runs are counted separately from finished ones, and event
parks that actually held are read back from the instrumented runtime because a
hold leaves no other trace. A `check` reports assertions through the same directives a hook
uses, so its evidence joins that key without the search having to draw
anything. `EventKill` outcomes are decoded
from the agent's dedicated monotonic fired counter rather than inferred from
aggregate unexpected deaths. `EventKill` draws choose a binary scale before a coordinate,
so finite event prefixes are searchable without a workload-specific upper
bound. Each observed ordinal remains distinct as fired or unfired evidence; the
coordinator keeps those bounds per action prefix and node, then probes the
exact midpoint while retaining a fired endpoint as a branchable prefix. The
selected parent prefix is materialized for both live and recorded draws, so the
same observation fold and refinement state is rebuilt during stream replay.

## Running it

```
harmony search --package faults IMAGE.oci --backend consonance \
    --kernel vmlinux --base-initramfs initramfs.cpio.gz \
    --fault-agent fault-agent --seed 1 --workers 8 --executions 100000 \
    --actions 12 --horizon-ms 500 --ram-mib 1024 --out run/
harmony search --package faults IMAGE.oci --backend consonance \
    --kernel vmlinux --base-initramfs initramfs.cpio.gz \
    --fault-agent fault-agent --replay run/bug-1.json --repeat 10 --out confirm/
```

Both modes write `report.json` ([`package`](src/package.rs)) with the pinned
image, kernel and agent hashes, the execution identity, the run bounds, and
either the bugs found or the replay outcomes.

Every replay run boots a session no earlier run has touched, so no snapshot
another run cached can stand in for guest execution: each run reaches the
sealed setup point and executes the recorded actions itself. Each run records
the actions it applied beside the horizons it ran in the guest, and the two are
equal when nothing came from a cache. Each run also carries the endpoint's
observations, because a fault whose only trace is a register -- an event kill
that fired, an event park that held -- cannot be compared across runs from the
stop and the assertions alone. A search replays every bug it records the
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

The Consonance backend needs Linux and KVM. The action model, the bundle
parser, the archive key, the image preparation and the report shapes are
portable and tested everywhere.
