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
Commands use five little-endian `u64` words in a 40-byte frame. The runtime
writes the acknowledgement while holding the callback lock, so an armed
callback cannot fire before its acknowledgement has entered the channel. A
callback made while its own thread holds that lock or is starting the runtime,
for example from an instrumented signal handler that interrupts the
acknowledgement write, returns without counting. Kill reports
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
visit to a site past its millionth visit counts 2^-20. A park command's fourth
and fifth words give an optional target: a half-open range of module offsets,
with `(0, 0)` for none. With a target, only edges whose site falls in the range
count toward `k`, and an empty range makes the command invalid. A park armed
by the command keeps the visit share. A park adopted from the park page,
described below, counts from its arming instead: each counted edge counts one
whatever its site's visit count, so the `k`-th counted edge after the resume is
held. Before the hold the runtime writes one JSON line through `fuzz_json_data`:
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

On Linux on x86_64 and arm64, a park also records whether the held thread,
once released, reads shared memory that another process changed during the
hold. Before the hold, the runtime copies every `rw-s` mapping listed in
`/proc/self/maps`, up to 16 mappings and 1 MiB in total. After the hold it
compares each mapping with its copy and removes all access to the pages that
changed, up to 256 pages, then installs a `SIGSEGV` handler. When the released
thread faults on one of those pages, the handler decodes whether the access
is a read from the fault context. On arm64 the syndrome register also gives
the access width; an access of unknown width, and every access on x86_64,
counts as 16 bytes. A read that covers a changed byte marks the watch, and the
handler restores access to the page so the access completes. The thread's
next instrumented edge removes access again. If the thread reads a changed
byte within its first 50 instrumented edges after release, the runtime writes
`{"harmony_park_read":{"site":S,"edges":N}}` through `fuzz_json_data`, where
`S` is the park's site and `N` is the edge count at the read. The watch ends at
that report or at the 50th edge, restoring every page and the previous
handler. Faults from other threads restore their page and leave it
unwatched. A fault outside the watched pages goes to the handler that was
installed before the watch. One park at a time owns the watch; a park that
lands while another hold or watch is in progress is not watched. A forked
child ends any watch it inherits. A shared mapping names state that other
processes can change, so a read of changed bytes right after release marks a
stop placed between reading and using that state.

## Park page

When the supervisor's bundle carries `park-page`, each node process inherits a
one-page shared memory file in `HARMONY_EVENT_PAGE_FD` beside the two event
descriptors. The runtime maps it read-only before it sends the hello, closes
the descriptor, and takes the page's current generation as already seen, so it
never adopts a record written for an earlier process of the node. If the
variable is set but the mapping fails, the runtime does not start and sends no
hello. Without the variable the runtime takes the path described above, with
no page.

The page holds five `u64` words: generation, edge count `k`, hold in
nanoseconds, and the target start and end. The supervisor writes a record
while the node is stopped for a parked pause (ours) and sends `SIGCONT` after
it. It makes the generation odd, writes the fields, and stores the next even
generation with release ordering. The runtime reads the page at the start of
every callback and of every control command, under the callback lock. It loads
the generation with acquire ordering, skips it if it is odd or already seen,
reads the fields, and loads the generation again; a changed generation is
skipped too. A skipped read does not count the callback's edge against the
park and retries at the next callback. An accepted record arms a park that
counts from its arming, as above. A record outside the command's ranges is
marked seen and arms nothing.

The claim that the first visit after the resume counts rests on that order. The
record is complete before `SIGCONT`, every task of the node has been stopped
for the whole pause, so no callback runs while the record is being written,
and each task's first callback after the resume reads the new generation
before it counts its edge. A visit before the pause saw the old generation and
was not counted.

The park status reply's fourth word is the generation the runtime has adopted,
so the supervisor can tell a reply sent before the resume from one sent after
it. A forked child drops the page in a `pthread_atfork` child handler: it
never adopts a later record and keeps whatever park state it inherited, as it
does for a park armed by command. `tests/page_test.c` runs a forked node with a
real `SIGSTOP` and `SIGCONT` and checks that a record published during the stop
fires at the first visit after the resume, that a disarm command stops it, and
that a forked child neither adopts a later record nor reports one.

## Kill rarity and coverage

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
before then hashes its address. A coverage-status command returns both values
and a saturating count of all callbacks. The language park check uses that
count, because hit-count buckets stop changing once they saturate.
They cover the process since it started. Module offsets make each crossing's
hash independent of where the loader placed the module. Slots are chosen by
address, so which sites share a slot can change with placement. Another 512 KiB
of bucket bytes holds the levels.

`tests/language_park_launcher.c` runs a language fixture, parks one of its
threads at a coverage site, and requires the callback count to grow while the
hold lasts. A fixture can print `HARMONY_LANGUAGE_PARK_RANGE start end` to
limit the park to a range of sites. The launcher then arms that range before
the fixture starts its spinner, and requires a new timer marker during the
hold. The Python fixture uses this to park at a Python-level site, which
releases the GIL.

A claimed kill keeps the callback lock through its report and signal, so a
later disarm acknowledgement cannot overtake enforcement. Parks release the
lock during the hold: other application threads keep running. When the last
running hold finishes, the park counts again from zero toward the same `k`, so
one arming can hold threads many times until a disarm arrives. A disarm stops
the count and is acknowledged at once. A thread already in a hold finishes it,
and a new arming during that hold counts from its own arming, so its first
hold can overlap the old one. Park status reports the fire count and two
flags: armed while the park counts toward `k`, and held while any thread is in
a hold. At least one flag stays set from arming until the last hold after a
disarm finishes, so the agent can distinguish a held thread from a recovered
process without guessing a sleep duration.
