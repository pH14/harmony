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
| `Interrupt(vector)` | a host-plane interrupt is staged at the window start, or at the parent endpoint's snapshot moment when that moment is past the window start |

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
deadline, and snapshots the exact stopped endpoint. A terminal stop is recorded
with its original stop and has no successor. If a continuable endpoint cannot
be snapshotted, the session is abandoned and the control diagnostic is
reported. A bounded LRU keeps recent prefixes resident and rebuilds evicted
ones from their longest cached ancestor.

Action activation and bounded execution use an explicit
[`action cursor`](src/execution.rs): the count of completed actions and whether
the next action is already active. A partial run preserves that activation.
Virtual time can cross several horizons in one guest step, so it cannot replace
the cursor or skip later actions. Each action still installs its own prefix in
order, including when consecutive transitions occur at the same moment.

[`continuation`](src/continuation.rs) owns this cursor across bounded runs and
cold restores. Capturing a checkpoint and restoring it neither run the guest
nor activate an action. Reproduction follows the recorded actions by ordinal;
a time bound returns before activating another action at the same moment.
Extending beyond the last action retains the final environment. A runtime or
capture failure makes the controller unusable until a fresh restore.

A campaign never encodes the virtual-time trace, so the session is configured
to defer sparse checkpoint hashing. Each due checkpoint would otherwise hash
all of a gigabyte-class guest's RAM inside the run that reached it, starting
with the boot that reaches setup.

[`campaign`](src/campaign.rs) implements the game-neutral campaign interface
over that target, and [`archive`](src/archive.rs) supplies the endpoint key,
which pairs the sometimes-assertion set with the live-node bitmap and the
hook-completion count.

## Retained workspace state

[`workspace`](src/workspace.rs) stores immutable moments, findings, evidence,
and committed request results in an ordered journal. A transaction publishes
all of its records together after its referenced checkpoint and evidence blobs
are stored. One process holds the workspace writer lock for the lifetime of the
open workspace. Publication synchronizes files and directories; an uncertain
commit requires reopening the journal before further mutation.

Reopening verifies sequence continuity and referenced blob contents. Unpublished
staging files and unreferenced blobs do not become history. The retained
[`checkpoint`](src/checkpoint.rs) codec preserves the original `HARMCKPT`
version 1 bytes and adds a version 2 action cursor. Legacy checkpoints lack
that transition position; it cannot be reconstructed from their timestamp.
Both readers validate page ordering, lengths, and complete input.
The [`retained execution`](src/retained.rs) envelope (`HARMEXEC` version 1)
stores the machine checkpoint, action windows, and complete action list
together. Cold continuation validates that saved plan before restoring it;
a raw machine checkpoint alone does not supply the plan.
[`declarations`](src/declarations.rs) retains workload-authored descriptions and
property meanings without inferring a verdict from a silent assertion.

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

New replay outcomes encode the engine's 32-byte state digest directly as
lowercase hex and mark it with `state_hash_encoding: "engine_digest"`.
Reports written by earlier versions omit that marker and contain SHA-256 of
the digest; readers default a missing marker to the legacy interpretation.
The marker is additive, so older readers continue to parse the report shape.

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

The Consonance backend needs Linux and KVM. The action model, the bundle
parser, the archive key, the image preparation and the report shapes are
portable and tested everywhere.
