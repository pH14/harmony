<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Fault workload runtime

This directory contains the instrumentation runtime composed into fault
workload binaries. It owns the event-kill and event-park protocol, including
command validation, the report hello, kill rarity matching, the park edge
count, and the report-before-signal rule. `fault_runtime_shim.c` connects that runtime to the
generic `harmony_instrumentation_event` hook supplied by `libvoidstar.so`.

Build and run its portable protocol test with:

```sh
make -C workloads/faults/runtime check
```

The resulting `build/libvoidstar.so` is a composed shared library containing
the generic ABI source, the runtime, and its shim. Fault image builds install
this composed library at `/usr/lib/libvoidstar.so` for every instrumented
workload; the composition is fixed by the image recipe and has no workload
switch. A process without the event file descriptor environment remains
inactive, while the host agent still receives the normal coverage and SDK
behavior.

The agent waits for the runtime's 16-byte hello before sending event commands.
Commands use three little-endian `u64` words in a 24-byte frame. The runtime
writes the acknowledgement while holding the callback lock, so an armed
callback cannot fire before its acknowledgement has entered the channel. Kill reports
use a 16-byte rarity/site frame and are sent before the requested signal. A
report write failure leaves the caller alive, so the agent cannot credit an
injection without runtime acknowledgement.

An armed park counts instrumented edges across every thread of the process
and holds the thread whose edge brings the count to `k`, where `k` is from 1
through `1 << 24`. Each edge counts one over its site's visit count, including
this visit, so the first visit to a site counts 1 and the hundredth counts
1/100. A site visited at a steady rate therefore draws parks at the same rate
as any other active site, and code that runs once per operation competes with
the loops inside that operation. The count is kept in units of 2^-20, so a
visit to a site past its millionth visit counts 2^-20. Before the hold it writes one JSON line through `fuzz_json_data`:
`{"harmony_park":{"site":S,"edges":K}}`. When the instrumentation passes a
code address, the site is that address's offset into the loaded module that
contains it, so parks in different processes of one executable share site
values and `addr2line` maps them to source lines. Any other site value, such
as a trace-pc-guard index, is reported unchanged. On Linux the control thread
lists the loaded modules with `dl_iterate_phdr` when it starts and before each
command that follows a load or unload. It appends new segments to a fixed list
of 4,096, and callbacks search that list newest first without a lock. Callbacks
never take the loader lock, so a thread holding it cannot deadlock with an
instrumented thread. A park in a module loaded since the last command, or on
another system, reports its address unchanged. A pending kill takes
priority over a park on the same edge.

Kill rarity is evaluated per instrumentation site. Before each callback the runtime
uses the site's saturating visit count; rarity `r` is eligible only while that
count is below `1 << r`, so rarity zero selects a site's first visit and rarity
63 remains well-defined for large counts. Counts live in a fixed table of 524,288 hashed `u64` counters
(4 MiB per instrumented process). Each callback performs one lookup. Colliding
sites share a saturating count, so a collision can make a site look hotter but
cannot make it look rarer. New sites remain eligible after startup rather than
being excluded by a full exact-site table.

The same table gives the process bucketed edge coverage. Each slot remembers
the highest AFL hit-count bucket its count has entered: 1, 2, 3, 4-7, 8-15,
16-31, 32-127, and 128 or more. When a count enters a higher bucket, the
runtime adds one crossing and adds a hash of the site's module offset and the
bucket to a wrapping sum. A crossing whose module is missing from the list
waits in a queue that the control thread resolves after its next module
listing, before it answers any command. A crossing in a module that unloads
before then hashes its address. A coverage-status command returns both values.
They cover the process since it started. Module offsets make each crossing's
hash independent of where the loader placed the module. Slots are chosen by
address, so which sites share a slot can change with placement. Another 512 KiB
of bucket bytes holds the levels.

A claimed kill keeps the callback lock through its report and signal, so a
later disarm acknowledgement cannot overtake enforcement. Parks release the
lock during the hold: other application threads keep running. Status remains
active until the hold finishes, and a disarm acknowledgement waits for that
completion. The agent can therefore distinguish a claimed park from a recovered
process without guessing a sleep duration.
