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
the complete image. Every selected resident page is integrity-checked before
the plan is returned. Plans borrow immutable page data from the store, including
a shared zero page, so collecting or mutating the store is impossible while a
plan is in use. Consumers can copy directly into destination RAM without an
intermediate page-sized allocation or copy. Owned exports copy only the pages
that must outlive the store borrow.

`page_delta(base, parent, target)` lists what a target adds over a parent that
itself derives from `base`: pages that differ from the parent, with their
stored BLAKE3 hashes, and the page numbers that returned to the base's content.
With no parent the delta is every page that differs from the base. The hashes
come from the content index, so an export hashes nothing.
`DeltaBuilder::write_hashed_page` hashes the page once, rejects it when the
hash differs from the one supplied, and interns it under that hash.

An inherited lookup caches its answer only on the requested layer, without
populating every traversed ancestor. Cached answers remain available until their
layer is collected; there is no capacity limit or eviction policy. This favors
repeated restores and reads over limiting cache memory. Cache allocation grows
with the distinct inherited frames queried on each resident layer, rather than
multiplying every query across its ancestry. Cache entries do not own page
references: immutable ancestry keeps the resolved content alive for the layer's
lifetime, and collecting the layer also drops its cache.

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
performance-shape, and public-API tests cover the storage semantics.
