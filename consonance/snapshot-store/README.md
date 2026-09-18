# snapshot-store

`snapshot-store` stores layered guest-memory snapshots and opaque vCPU/device
state. A base layer describes the boot image; each child records only pages
whose content differs from its parent. The store is independent of KVM dirty
page harvesting and memslot management.

## Store and snapshots

Create a `Store` with a fixed number of guest pages, write pages through a
`BaseBuilder` or `DeltaBuilder`, and consume the builder with `seal(vm_state)`.
Unwritten pages are implicitly zero. Repeated writes to a frame replace the
previous write, and writes equal to the inherited content are discarded.

`read_page` resolves the nearest layer that wrote a frame. Layers are immutable
after sealing; a lookup cache makes repeated reads efficient. Page contents are
interned store-wide by BLAKE3, while the all-zero page is implicit. `vm_state`
is opaque but its seal-time digest is checked before it is returned. Corrupted
page data or state produces an integrity error rather than silently returning
bytes.

Snapshot IDs are reference-counted. `retain` adds a live reference,
`release` makes an ID unobservable at zero, and `gc` removes layers no longer
reachable from a live snapshot or its ancestors. `stats` and `store_stats`
report logical size, owned pages, chain depth, unique content, and resident
payload bytes.

## Mappings

`Store::materialize` resolves a full image into a sparse temporary file and
returns a private copy-on-write `Mapping`. Writes through the mapping affect
only the mapping; the immutable store and all snapshots remain unchanged.
`Mapping::as_slice`, `as_mut_slice`, `len`, and `is_empty` expose the image.

`Mapping::anonymous` supplies a zero-filled, page-aligned heap backing with the
same interface. It is useful for tests and interpreter-based safety checks,
while production materialization uses the mmap-backed path. Both paths preserve
the page-aligned memory contract expected by backend memory mapping.

The store uses ordered layer/page metadata wherever iteration is observable;
content-addressed page lookup is private and lookup-only. Builder drops release
any buffered page references. Oracle, stateful, integrity, copy-on-write,
performance-shape, and public-API tests cover the storage semantics.
