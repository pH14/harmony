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
reads `/proc/self/maps` again and keeps only the mappings whose start, end,
file offset, device and inode are unchanged; another thread can unmap, shrink
or replace a mapping while the parked thread sleeps. Changing a page's
protection splits its mapping into several lines of the file, so the check
joins adjacent shared lines of the same file at consecutive offsets into one
mapping before comparing. It compares each kept
mapping with its copy and removes all access to the pages that changed, up to
256 pages, then installs a `SIGSEGV` handler with `SA_ONSTACK`, so it runs on
the thread's signal stack where Go requires it. A dropped mapping reports
nothing: its changed bytes do not count as changes, and a hold whose mappings
are all dropped is not watched. Two mappings that share an inode and offset,
such as an io_uring ring unmapped and mapped again at the same address, pass
the identity check, so differences between them count as changes.

When the released thread faults on one of those pages, the handler acts only
on an access fault (`SEGV_ACCERR`) at a page the watch protected and has not
released. It reads `/proc/self/maps` again; if the page's mapping no longer
matches, or restoring access fails, the handler releases the page and passes
the fault to the handler installed before the watch, because the fault then
belongs to the program. Otherwise it decodes whether the access is a read from
the fault context. On arm64 the syndrome register also gives the access width;
an access of unknown width, and every access on x86_64, counts as 16 bytes. A
read that covers a changed byte marks the watch, and the handler restores
access to the page so the access completes. The thread's next instrumented
edge removes access again from the pages it touched, after the same identity
check. If the thread reads a changed byte within its first 50 instrumented
edges after release, the runtime writes
`{"harmony_park_read":{"site":S,"edges":N}}` through `fuzz_json_data`, where
`S` is the park's site and `N` is the edge count at the read. The watch ends at
that report or at the 50th edge, restoring access to every page whose mapping
still matches and restoring the previous handler. Faults from other threads
restore their page and leave it unwatched. A fault outside the watched pages,
or a fault of any other kind, goes to the handler that was installed before
the watch. One park at a time owns the watch; a park that lands while another
hold or watch is in progress is not watched. A forked child ends any watch it
inherits. A shared mapping names state that other processes can change, so a
read of changed bytes right after release marks a stop placed between reading
and using that state.

The copy and the comparison never read a recorded mapping through a pointer.
A thread can unmap a mapping between the second read of `/proc/self/maps` and
the comparison, and a load from an unmapped address would raise `SIGSEGV` in
the instrumented thread and end the node as if the program had crashed. The
runtime reads memory with `process_vm_readv` on its own process ID, which
returns `EFAULT` or a short count for an unmapped range. When the call does
not read the whole range, the runtime writes the range into a pipe and reads
it back, 4096 bytes at a time; `write` from an unmapped address returns
`EFAULT` instead of faulting. Any error other than `EFAULT`, whatever its
cause, makes the runtime stop calling `process_vm_readv` for the rest of the
process and use the pipe alone. A range that the pipe cannot carry in full is
dropped as above. The x86_64 KVM guest kernel starts from `allnoconfig`
without `CONFIG_CROSS_MEMORY_ATTACH`, so `process_vm_readv` returns `ENOSYS`
there and guest nodes use the pipe from their first watched hold. The copy and
the comparison each open one pipe and close it when done, so a park on the
pipe path makes at most 2 * (2 * 256 + 3) system calls to read memory: one
write and one read per 4096 bytes of the 1 MiB limit, plus the pipe's
creation and two closes. Each read of `/proc/self/maps` goes through a
256-byte buffer, with no limit on the file's length. The runtime reads the
file once before the hold, once after it, once at each instrumented edge that
has pages to protect again, once per fault on a watched page, and once when
the watch ends.

## Park page

When the supervisor's bundle carries `park-page`, each node process inherits a
one-page shared memory file in `HARMONY_EVENT_PAGE_FD` beside the two event
descriptors. The runtime maps it read-only before it sends the hello and keeps
the descriptor open, so an image the node `exec`s inherits the descriptor with
the event descriptors and maps the page again. A runtime starts with
generation 0 as seen: each node process gets a new page, so the record on the
page is always the node's current parked pause (ours), and a runtime that
starts after a record was written, in a late first callback or after an
`exec`, adopts it at its first callback. If the variable is set but the mapping
fails, the runtime does not start and sends no hello. Without the variable the
runtime takes the path described above, with no page.

The page holds six `u64` words: generation, edge count `k`, hold in
nanoseconds, the target start and end, and the pid of the node process the
supervisor spawned. The supervisor writes the pid once, right after the spawn
and before any record. It writes a record while the node is stopped for a
parked pause (ours) and sends `SIGCONT` after it. It makes the generation odd,
writes the fields, and stores the next even generation with release ordering.
The runtime reads the page at the start of
every callback and of every control command, under the callback lock. It loads
the generation with acquire ordering, stops if it is already seen, and skips it
if it is odd. Otherwise it reads the fields and loads the generation again; a
changed generation is skipped too. A callback that skips a read spends no park
at all, neither the new record's nor one armed before it, and the read retries
at the next callback. An accepted record with zero edges clears a park adopted
from the page. Any other accepted record arms a park that counts from its
arming, as above; a record outside the command's ranges is marked seen and
arms nothing.

Only the spawned process arms a record: a runtime whose `getpid()` differs
from the page's pid word marks each record seen and arms nothing. So a node
whose instrumented process is not the spawned pid never adopts a record. A
wrapper that forks the server instead of `exec`ing it is such a node: its
parked pauses run as plain pauses, and the supervisor reports each one as
`unadopted` (see the supervisor README). A server in its own pid namespace
usually is too, but the check compares bare numbers, so a server whose pid
inside its namespace equals the spawned pid adopts and fires the record. rqlite's node chain keeps the pid: `node.sh` `exec`s `harness-net`,
which `exec`s `ip netns exec`, which `exec`s `rqlited`, and none of the node's
namespaces is a pid namespace. The pid word also covers a
process that forked before its runtime started, which has no fork handler to
drop the page.

A park adopted from the page ends only through the page. A disarm command
leaves it armed, because a disarm the supervisor sent before a pause can be
read after the resume that published the next record. A clearing record leaves
a park armed by command alone, and an arm command replaces a park adopted from
the page with its own.

The claim that the first visit after the resume counts rests on that order and
on the supervisor's stop check. A process-directed `SIGSTOP` marks one thread,
and the node's other threads can run user code until the group stop has taken
effect. So the supervisor writes a record only after `waitid` reports the node
process stopped, which the kernel reports once every thread of the process has
stopped, and it sends `SIGCONT` after the record is complete. No callback runs
while the record is being written, and each thread's first callback after the
resume reads the new generation before it counts its edge. A visit before the
pause saw the old generation and was not counted. A node that has not stopped
within the supervisor's wait, or has exited, gets no record: the supervisor
sends `SIGCONT` alone, the pause runs as a plain pause, and the supervisor
reports it as `not_stopped` or `not_running`.

The park status reply's fourth word is the generation the runtime has seen, so
the supervisor can tell a reply sent before the resume or before a clear from
one sent after it. Its fifth word is the last generation that armed a park in
this process, and flag 4 is set once that park has fired. A clearing record
leaves both unchanged, so the supervisor can still read whether the park it
published fired after the clear has ended it.

A forked child drops the page in a `pthread_atfork` child handler, together
with any park it inherited from the page, and marks its copy of the descriptor
close-on-exec. It never fires its parent's parked pause and never adopts a
later record, and an image it `exec`s finds no page and runs without the event
runtime. It keeps a park armed by command, as before. `tests/page_test.c` runs
each case on its own node with a real `SIGSTOP` and `SIGCONT`: a record written
before the runtime starts is adopted at its first callback; a clearing record
disarms the park; a disarm queued while the node is stopped leaves the record
published before the resume armed; a forked child of a node with an adopted
park neither fires it nor adopts a later record; a process the node forked
before its first callback acknowledges a record and never arms it; an image
the node `exec`s maps the page and fires the record written before the `exec`;
and an image that a forked child `exec`s fires nothing. `tests/event_test.c`
checks that a record whose pid word names another process is marked seen and
leaves the reply's fifth word and fired flag unchanged.

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

A crossing into bucket 1 is the process's first visit to that slot, so the
runtime also keeps the site's module offset, or its address when no listed
module holds it, in a list of new sites. After the control thread answers each
command it takes the list, releases the callback lock, and writes it through
`fuzz_json_data` as lines of the form `{"harmony_sites":[S,...]}` of at most
2,048 bytes, below the SDK device's limit on one JSON write. The list holds at
most 65,536 sites between two commands; later new sites in that interval are
dropped. A forked child stops keeping the list, because it has no control
thread to send it. A runtime started where `/dev/harmony` cannot be opened for
writing keeps no list, so a process outside a Harmony guest writes no site
lines. The fault workload counts these lists per action to weight park sites by
rarity (see the fault workload README).

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
