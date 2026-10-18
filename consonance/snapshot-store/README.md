# snapshot-store

`snapshot-store` stores layered guest-memory snapshots and opaque vCPU/device
state. A base layer describes the boot image; each child records only pages
whose content differs from its parent. The store is independent of KVM dirty
page harvesting and memslot management.

## Store and snapshots

Create a `Store` with a fixed number of guest pages, write pages through a
`BaseBuilder` or `DeltaBuilder`, and consume the builder with `seal(vm_state)`.
Unwritten pages are implicitly zero. Repeated writes to a frame replace the
previous write, and writes equal to the inherited content are discarded. A base
builder omits zero-page entries immediately, including when a zero write replaces
a buffered nonzero page. Flattening leaves unchanged inherited entries in place
without acquiring and releasing the same reference again.

`read_page` resolves the nearest layer that wrote a frame. Layers are immutable
after sealing; each layer caches inherited lookups to make repeated reads
efficient. Page contents are interned store-wide by BLAKE3, while the all-zero
page is implicit. `vm_state` is opaque but its seal-time digest is checked before
it is returned. Corrupted page data or state produces an integrity error rather
than silently returning bytes.
An intern-table hit also compares the resident bytes with the already-hashed
incoming page before reusing it, so a stale resident entry cannot contaminate
a new write or import. `write_changed_page` verifies an inherited page before
skipping a write whose bytes match it. Flattening verifies all inherited pages
before acquiring their references or creating a new base.

Layer page tables are immutable sorted arrays of guest frame numbers and
word-sized page references. Digests stay in the content index and resident-page
records; layer tables and lookup caches refer to a reusable resident-page slot
instead of copying those digests. A slot remains occupied while any builder or
retained layer owns it. These references are private to the store and never appear in
snapshot exports. Flattening carries inherited references into a new base and
reads only the pages declared dirty from the supplied memory image. It validates
and acquires the inherited references before moving their page table directly
into the builder, avoiding a second tree containing the same entries.

Snapshot IDs increase at seal time, so every parent has a smaller ID than its
children. `diff_pages` advances the newer of two ancestry cursors until they
meet, collecting the frames changed along either path. Identical snapshots need
no ancestry traversal; other diffs stop at the shared ancestor without building
temporary ancestor or visited sets. Unrelated bases meet at the end of their
paths. Returned frames remain sorted and borrow the target snapshot's contents.
`restore_pages(from, to, dirty)` includes the current guest's dirty frames in
that same sorted, deduplicated plan; with no known source snapshot it includes
the complete image. Restore plans and diffs verify each distinct resident page
they emit against its stored BLAKE3 digest before returning borrowed bytes,
including inherited pages selected by a dirty frame. Implicit zero pages need
no read. A local Apple-silicon release microbenchmark measured 33.72 ms to
plan a full 8,192-page image with distinct 4 KiB contents (32 MiB) and 0.17 ms
when those frames shared one interned page. Plans borrow immutable page data
from the store, including a shared zero page, so collecting or mutating the
store is impossible while a plan is in use. Consumers can copy directly into
destination RAM without an intermediate page-sized allocation or copy. Owned
exports copy only the pages that must outlive the store borrow.

`page_delta(base, parent, target)` lists what a target adds over a parent that
itself derives from `base`: pages that differ from the parent, with their
stored BLAKE3 hashes, and the page numbers that returned to the base's content.
With no parent the delta is every page that differs from the base. Before
classifying the delta, the store rehashes each distinct resident page reachable
from the base, parent, or target and rejects stale content. This includes pages
that will be omitted as unchanged or represented only by a return to the base.
Shared resident pages are checked once across those snapshots.
On the same Apple-silicon host, a local release benchmark of ten empty deltas
over 8,192 distinct 4 KiB pages (32 MiB) averaged 22.76 ms per delta; 8,192
references to one interned page averaged 1.32 ms per delta. This verifies the
input snapshots even when the delta itself carries no page bytes.
`DeltaBuilder::write_hashed_page` hashes the page once, rejects it when the
hash differs from the one supplied, and interns it under that hash.
`DeltaBuilder::write_changed_page` compares the page with the parent's stored
copy and records it only when the bytes differ, so a caller that rewrites a
mostly unchanged region does not hash the unchanged pages.
`rebase` requires in-range target page rows in strictly increasing guest-frame
order and rejects out-of-range, reordered, or duplicate rows before it selects
pages from the nearby snapshot. This keeps a malformed delta from restoring an
origin page over a target page or silently dropping a nonexistent zero page
during import. It verifies every distinct reachable resident page of the near
snapshot before reusing that snapshot, including inherited pages absent from
the target rows. It also checks origin pages copied into the rebased result.
`page_ref_eq` verifies referenced resident data before reporting equality.
The BLAKE3 work is proportional to distinct resident pages, rather than
the number of references to interned content. In a release microbenchmark on
Apple silicon with 8,192 distinct 4 KiB pages (32 MiB), ten exports averaged
0.60 ms before verification and 21.51 ms after it; ten empty-row rebases of
the same near image averaged less than 0.01 ms before verification and 21.41 ms
after it. With 8,192 references to one interned page, verified export and
rebase averaged 0.65 ms and 0.45 ms. These are local measurements, not a latency
budget; rehashing distinct bytes is the cost of detecting resident corruption
before their hashes or near-page reuse are trusted.
In the same microbenchmark, flattening a 32 MiB distinct-page image with no
dirty pages rose from 0.47 to 21.35 ms; unchanged-page writes across that image
rose from 1.79 to 21.86 ms. Writing 8,192 pages that intern to one existing page
rose from 20.27 to 21.08 ms, because hashing the incoming bytes already
dominates the additional byte comparison on each index hit.
`verify_snapshot_pages` applies the same distinct-page check to a complete
resolved snapshot, including inherited pages. Sparse archive consumers use it
before trusting a cached base-RAM digest; implicit zero pages need no read.
`verify_snapshots` checks multiple resolved snapshots with one set of verified
resident pages, so content shared by the base and target is hashed once.

`read_transaction` shares the resident-slot verification set across delta
planning, page differences, checked page reads, and complete-image verification.
A transaction holds an immutable store borrow; page storage cannot be rewritten,
released, collected, or reused while that transaction or its returned page
references remain live. Verified slot identities stay private to that store and
transaction. A new transaction verifies the resident bytes again, including
unchanged, inherited, and omitted pages reached by complete-image verification.
Standalone read, delta, difference, and verification operations begin fresh
transactions. This boundary removes duplicate integrity hashing within one
operation without carrying trust across operations or changing commitment bytes.

An inherited lookup caches its answer only on the requested layer, without
populating every traversed ancestor. Cached answers remain available until their
layer is collected; there is no capacity limit or eviction policy. This favors
repeated restores and reads over limiting cache memory. Cache allocation grows
with the distinct inherited frames queried on each resident layer, rather than
multiplying every query across its ancestry. Cache entries do not own page
references: immutable ancestry keeps the resolved content alive for the layer's
lifetime, and collecting the layer also drops its cache.

Snapshot IDs pair the seal-time sequence with a process-local store identity.
The store identity is only a handle guard; it is absent from snapshot bytes,
hashes, and exported records. Passing an ID from another store returns
`UnknownSnapshot`, even when both stores have a layer with the same sequence.
Snapshot IDs are reference-counted. `retain` adds a live reference,
`release` makes an ID unobservable at zero, and `gc` removes layers no longer
reachable from a live snapshot or its ancestors. `stats` and `store_stats`
report logical size, owned pages, chain depth, unique content, and resident
payload bytes.

Each layer counts its resident direct children. Releasing the last external
reference queues a childless layer for collection; removing it can queue its
released parent. Collection follows those newly unreachable chains iteratively,
without scanning live layers. Live snapshot counts and resident VM-state bytes
are maintained at seal, release, and collection, making `store_stats` constant
time. Its `bytes_resident` remains a payload measure: page buffers and VM-state
bytes, excluding allocator overhead, indexes, lookup caches, and materialized
mappings. An empty content pool releases its index and slot allocations; each
collected layer releases its lookup cache.

## Mappings

`Store::materialize` resolves a full image into a sparse temporary file and
returns a private copy-on-write `Mapping`. Writes through the mapping affect
only the mapping; the immutable store and all snapshots remain unchanged.
`Mapping::as_slice`, `as_mut_slice`, `len`, and `is_empty` expose the image.
Population writes through a shared file mapping, then drops it before creating
the private mapping. The temporary file is unlinked and requires no durability,
so population does not synchronously flush its contents to disk. Subsequent
mappings read the populated file through the operating system's page cache.

`Mapping::anonymous` supplies a zero-filled, page-aligned heap backing with the
same interface. It is useful for tests and interpreter-based safety checks,
while production materialization uses the mmap-backed path. Both paths preserve
the page-aligned memory contract expected by backend memory mapping.

The store uses ordered layer/page metadata wherever iteration is observable;
content-addressed page lookup is private and lookup-only. Builder drops release
any buffered page references. Oracle, stateful, integrity, copy-on-write,
and performance-shape tests cover the storage semantics.
