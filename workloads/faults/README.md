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
| `node <id> <name> <argv...>` | one workload process the agent supervises |
| `hook <id> <argv...>` | a command the search can run at any moment |
| `ready <argv...>` | must pass before the run's setup point is sealed |
| `setup <argv...>` | runs once, after the nodes start, before readiness |

[`prepare`](src/prepare.rs) stages that image, reads the bundle for the action
alphabet, and assembles a guest initramfs: the base image, the OCI rootfs, and
a control member holding the static fault agent and this package's init. The
init mounts the pseudo-filesystems, binds and chroots into the workload rootfs,
and execs the agent. The control member is appended after the compressed
members and padded to four bytes, which Linux initramfs requires before a raw
`newc` header.

## Actions

An input is a list of actions, each running for one fixed horizon of guest
time ([`target`](src/target.rs)):

| action | effect |
|---|---|
| `Wait` | nothing; the workload runs undisturbed for a horizon |
| `Kill(node)` | the node stays down for the whole horizon |
| `Pause(node, ticks)` | the node is stopped, then continued inside the horizon |
| `Restart(node)` | the node is killed and comes back inside the horizon |
| `Hook(id)` | the agent runs that hook once |
| `Park(node, addr, hits, hold)` | guest threads are held at an execution place |
| `Interrupt(vector)` | a host-plane interrupt is staged at the window start |

Every action but `Interrupt` becomes a standing-fault window on the shared
[`fault-policy`](../fault-policy) wire form. The package answers the agent's
standing poll with the windows whose half-open span contains the polling
moment, so an input is fully described by its encoded window list and one
branch installs it.

## Execution

[`consonance`](src/consonance.rs) drives one `consonance_client::session::Session`
per evaluator thread. Each portable action prefix maps to a real whole-VM
snapshot: the session branches its parent under the prefix's window list, runs
to the action's horizon deadline, and seals the endpoint. Endpoints the session
cannot seal have no successor and the search records them as dead. A bounded
LRU keeps recent prefixes resident and rebuilds evicted ones from their longest
cached ancestor.

[`campaign`](src/campaign.rs) implements the game-neutral campaign interface
over that target, and [`archive`](src/archive.rs) supplies the endpoint key,
which pairs the sometimes-assertion set with the live-node bitmap and the
hook-completion count.

## Running it

```
harmony search --package faults IMAGE.oci --kernel vmlinux --backend consonance \
    --fault-agent fault-agent --seed 1 --workers 8 --executions 100000 \
    --actions 12 --horizon-ms 500 --ram-mib 1024 --out run/
harmony search --package faults IMAGE.oci --kernel vmlinux --backend consonance \
    --fault-agent fault-agent --replay run/bug-1.json --repeat 10 --out confirm/
```

Both modes write `report.json` ([`package`](src/package.rs)) with the pinned
image, kernel and agent hashes, the execution identity, the run bounds, and
either the bugs found or the replay outcomes. Search also writes
`campaign-summary.json`, `stream.jsonl`, `progress.jsonl`,
`first-bug-input.json`, and one `bug-N.json` per recorded bug
([`report`](src/report.rs)); each of those carries the action list and the
encoded window list that reproduces it.

The Consonance backend needs Linux and KVM. The action model, the bundle
parser, the archive key, the image preparation and the report shapes are
portable and tested everywhere.
