// SPDX-License-Identifier: AGPL-3.0-or-later
//! Layered copy-on-write storage for guest-memory snapshots.
//!
//! The deterministic hypervisor snapshots a running VM thousands of times per run and
//! branches from interesting states. [`Store`] is the storage engine behind that: a base
//! layer holds the booted guest image, and every later snapshot records only the pages
//! dirtied since its parent plus a small opaque vCPU/device blob. A snapshot's full
//! memory image is reconstructed by resolving down the layer chain (worst case O(chain
//! length), with a per-layer memo index making repeated reads O(1)); identical page
//! contents are stored once store-wide, content-addressed by BLAKE3; and
//! [`Store::materialize`] hands out a private copy-on-write mapping of the full image
//! backed by a sparse tempfile. This crate is built and tested standalone against plain
//! memory — KVM integration (dirty-page harvesting, memslot remapping) lives elsewhere.

#![warn(missing_docs)]

mod mapping;

pub use mapping::Mapping;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
// not order-observable: the per-layer resolve cache (`Layer::resolve_cache`) is a
// lookup-only memo keyed by gfn; it is never iterated, so its unordered layout
// cannot reach any output, hash, or encoded byte. See the field doc below.
#[allow(clippy::disallowed_types)]
use std::collections::HashMap;
use std::io::{Seek, SeekFrom, Write};

/// Size in bytes of one guest page.
pub const PAGE_SIZE: usize = 4096;

/// Opaque identifier of a sealed snapshot.
///
/// Ids are assigned monotonically at seal time and are never reused by a given
/// [`Store`]. Ids from one store are meaningless in another.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct SnapshotId(u64);

/// Configuration for a [`Store`].
#[derive(Copy, Clone, Debug)]
pub struct StoreConfig {
    /// Guest memory size in pages; every snapshot's logical image is this many pages.
    pub mem_pages: u64,
}

/// Errors returned by [`Store`] operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The snapshot id is not known to this store, or the snapshot's refcount has
    /// dropped to zero (a released snapshot behaves as unknown).
    #[error("unknown (or fully released) snapshot {0:?}")]
    UnknownSnapshot(SnapshotId),
    /// The guest frame number lies outside configured guest memory.
    #[error("gfn {gfn} out of range: guest memory is {mem_pages} pages")]
    GfnOutOfRange {
        /// The offending guest frame number.
        gfn: u64,
        /// The configured guest memory size in pages.
        mem_pages: u64,
    },
    /// A page buffer had a length other than [`PAGE_SIZE`].
    #[error("page buffer is {len} bytes, expected {PAGE_SIZE}")]
    BadPageLength {
        /// The offending buffer length.
        len: usize,
    },
    /// A builder was used in an unsupported way.
    ///
    /// Single use of builders is enforced at compile time (`seal` consumes the builder
    /// and builders hold `&mut Store`), so this variant is reserved for future
    /// runtime-checked misuse; no current operation returns it.
    #[error("builder misuse: {0}")]
    BuilderMisuse(&'static str),
    /// An underlying I/O operation failed (tempfile creation, sizing, write, or mmap).
    #[error("i/o error")]
    Io(#[from] std::io::Error),
}

/// Per-snapshot statistics, see [`Store::stats`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SnapStats {
    /// Size of the snapshot's logical memory image in pages
    /// (always [`StoreConfig::mem_pages`]).
    pub logical_pages: u64,
    /// Pages this layer records that no ancestor layer provides identically.
    /// Writes whose content equals what the parent chain already resolves to are
    /// discarded at seal time, so they never count here.
    pub owned_pages: u64,
    /// Number of layers in this snapshot's chain, itself included (a base is 1).
    pub chain_len: u32,
}

/// Store-wide statistics, see [`Store::store_stats`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct StoreStats {
    /// Number of live (refcount > 0) snapshots.
    pub snapshots: u64,
    /// Number of distinct page contents resident store-wide. The all-zero page is
    /// implicit and never stored, so it does not count.
    pub stored_unique_pages: u64,
    /// Sum of logical image sizes over live snapshots, in pages.
    pub logical_pages_total: u64,
    /// Bytes of payload the store keeps resident: unique page data plus the
    /// vCPU/device blobs of every resident layer (live or retained as an ancestor).
    /// Bookkeeping overhead (maps, indexes) is not counted.
    pub bytes_resident: u64,
}

/// BLAKE3 digest of one page's content; the store-wide content address.
type PageHash = [u8; 32];

/// What a layer records (or a resolution yields) for one gfn.
///
/// The all-zero page is special-cased: it is never interned, so sparse images cost
/// nothing and `stored_unique_pages` only counts real content.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum PageRef {
    /// Explicitly (or implicitly) the all-zero page.
    Zero,
    /// Content stored in `Store::pages` under this hash.
    Data(PageHash),
}

/// One distinct page content, shared store-wide.
struct PageEntry {
    /// Exactly `PAGE_SIZE` bytes.
    data: Box<[u8]>,
    /// Number of (layer, gfn) slots referencing this content.
    refs: u64,
}

/// One snapshot layer. Layers stay resident after their snapshot is released for as
/// long as a live descendant needs them; `gc` reaps the rest.
struct Layer {
    parent: Option<u64>,
    /// Pages this layer provides, i.e. dirtied relative to its parent.
    pages: BTreeMap<u64, PageRef>,
    /// Opaque vCPU/device state recorded at seal time.
    vm_state: Vec<u8>,
    /// Live references; 0 means released (observable only as an ancestor).
    refcount: u64,
    /// Layers from here to the root, inclusive.
    chain_len: u32,
    /// Memoized chain resolutions: gfn -> what this layer's image holds there.
    /// Sound because sealed images are immutable and every ancestor of a resident
    /// layer is itself resident (gc preserves ancestors), so a cached `PageRef::Data`
    /// can never dangle. Lookup-only — never iterated, so the unordered map cannot
    /// leak nondeterminism into any output.
    // not order-observable: lookup-only memo, never iterated (see doc above).
    #[allow(clippy::disallowed_types)]
    resolve_cache: RefCell<HashMap<u64, PageRef>>,
}

/// Layered copy-on-write guest-memory snapshot store. See the crate docs.
pub struct Store {
    cfg: StoreConfig,
    next_id: u64,
    /// All resident layers, keyed by raw snapshot id (BTreeMap for deterministic
    /// iteration in `gc` and `store_stats`).
    layers: BTreeMap<u64, Layer>,
    /// Content-addressed page storage: one entry per distinct page content.
    pages: BTreeMap<PageHash, PageEntry>,
    /// Hash of the all-zero page, precomputed to detect zero writes.
    zero_hash: PageHash,
}

impl Store {
    /// Create an empty store for guest images of `cfg.mem_pages` pages.
    pub fn new(cfg: StoreConfig) -> Store {
        Store {
            cfg,
            next_id: 0,
            layers: BTreeMap::new(),
            pages: BTreeMap::new(),
            zero_hash: *blake3::hash(&[0u8; PAGE_SIZE]).as_bytes(),
        }
    }

    /// Build the base layer. Pages not written before `seal()` are implicitly zero.
    ///
    /// Each call starts a new independent root layer; the common case is exactly one
    /// base per store.
    pub fn begin_base(&mut self) -> BaseBuilder<'_> {
        BaseBuilder {
            core: BuilderCore {
                store: self,
                parent: None,
                pages: BTreeMap::new(),
            },
        }
    }

    /// Begin a child snapshot of `parent`. Errors if `parent` is unknown or no longer
    /// live. (Unsealed snapshots have no id yet, so they are unnameable here.)
    pub fn derive(&mut self, parent: SnapshotId) -> Result<DeltaBuilder<'_>, StoreError> {
        self.live_layer(parent)?;
        Ok(DeltaBuilder {
            core: BuilderCore {
                store: self,
                parent: Some(parent.0),
                pages: BTreeMap::new(),
            },
        })
    }

    /// Read one page of `snap`'s logical memory image into `out` (length
    /// [`PAGE_SIZE`]), resolving through the layer chain; zero page if never written.
    pub fn read_page(&self, snap: SnapshotId, gfn: u64, out: &mut [u8]) -> Result<(), StoreError> {
        self.live_layer(snap)?;
        if out.len() != PAGE_SIZE {
            return Err(StoreError::BadPageLength { len: out.len() });
        }
        if gfn >= self.cfg.mem_pages {
            return Err(StoreError::GfnOutOfRange {
                gfn,
                mem_pages: self.cfg.mem_pages,
            });
        }
        match self.resolve(snap.0, gfn) {
            PageRef::Zero => out.fill(0),
            PageRef::Data(hash) => match self.pages.get(&hash) {
                Some(entry) => out.copy_from_slice(&entry.data),
                None => {
                    // Unreachable: every PageRef::Data held by a resident layer keeps
                    // its entry's refcount >= 1. Degrade to zeros rather than panic.
                    debug_assert!(false, "dangling page ref");
                    out.fill(0);
                }
            },
        }
        Ok(())
    }

    /// The opaque vCPU/device blob recorded at seal time.
    pub fn vm_state(&self, snap: SnapshotId) -> Result<&[u8], StoreError> {
        Ok(&self.live_layer(snap)?.vm_state)
    }

    /// Materialize the full logical image as a private copy-on-write mapping.
    ///
    /// The image is resolved into a freshly created flat tempfile — sparse, so
    /// never-written (zero) pages cost neither disk nor memory — which is then mapped
    /// copy-on-write (`MAP_PRIVATE`; portable across macOS and Linux). The mapping is
    /// mutable; writes touch only private pages and never reach the file or the store.
    /// The tempfile is owned by the returned [`Mapping`] and is reclaimed when it drops.
    pub fn materialize(&self, snap: SnapshotId) -> Result<Mapping, StoreError> {
        self.live_layer(snap)?;
        let len = self
            .cfg
            .mem_pages
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| {
                StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "logical image size overflows u64",
                ))
            })?;

        // First writer in the chain wins for each gfn; deeper layers fill only gaps.
        let mut resolved: BTreeMap<u64, PageRef> = BTreeMap::new();
        let mut cur = Some(snap.0);
        while let Some(id) = cur {
            let Some(layer) = self.layers.get(&id) else {
                // Unreachable: ancestors of resident layers are resident.
                debug_assert!(false, "dangling parent link");
                break;
            };
            for (&gfn, &pref) in &layer.pages {
                resolved.entry(gfn).or_insert(pref);
            }
            cur = layer.parent;
        }

        let mut file = tempfile::tempfile()?;
        file.set_len(len)?;
        for (&gfn, &pref) in &resolved {
            if let PageRef::Data(hash) = pref {
                if let Some(entry) = self.pages.get(&hash) {
                    // gfn < mem_pages and mem_pages * PAGE_SIZE fits in u64 (checked
                    // above), so this offset cannot overflow.
                    file.seek(SeekFrom::Start(gfn * PAGE_SIZE as u64))?;
                    file.write_all(&entry.data)?;
                } else {
                    debug_assert!(false, "dangling page ref");
                }
            }
        }
        Ok(Mapping::new(file, len)?)
    }

    /// Increment `snap`'s refcount. Snapshots start with refcount 1, held by the
    /// creator. Errors once the refcount has reached zero: released snapshots are
    /// gone for good and cannot be resurrected.
    pub fn retain(&mut self, snap: SnapshotId) -> Result<(), StoreError> {
        let layer = self.live_layer_mut(snap)?;
        layer.refcount = layer.refcount.saturating_add(1);
        Ok(())
    }

    /// Decrement `snap`'s refcount. At zero the snapshot is immediately unobservable
    /// (every operation on its id errors); its layer data stays resident only while a
    /// live descendant's chain needs it, and is reclaimed by [`Store::gc`].
    pub fn release(&mut self, snap: SnapshotId) -> Result<(), StoreError> {
        let layer = self.live_layer_mut(snap)?;
        layer.refcount -= 1;
        Ok(())
    }

    /// Drop layers unreachable from any live (refcount > 0) snapshot or its ancestors.
    /// Returns bytes freed: page data whose last reference went away, plus the
    /// vCPU/device blobs of dropped layers (bookkeeping overhead is not counted).
    pub fn gc(&mut self) -> u64 {
        let mut reachable: BTreeSet<u64> = BTreeSet::new();
        for (&id, layer) in &self.layers {
            if layer.refcount == 0 {
                continue;
            }
            let mut cur = Some(id);
            while let Some(c) = cur {
                if !reachable.insert(c) {
                    break; // already walked from here up
                }
                cur = self.layers.get(&c).and_then(|l| l.parent);
            }
        }
        let dead: Vec<u64> = self
            .layers
            .keys()
            .copied()
            .filter(|id| !reachable.contains(id))
            .collect();
        let mut freed = 0u64;
        for id in dead {
            if let Some(layer) = self.layers.remove(&id) {
                freed += layer.vm_state.len() as u64;
                for (_gfn, pref) in layer.pages {
                    if let PageRef::Data(hash) = pref {
                        freed += self.release_page_ref(hash);
                    }
                }
            }
        }
        freed
    }

    /// Statistics for one live snapshot.
    pub fn stats(&self, snap: SnapshotId) -> Result<SnapStats, StoreError> {
        let layer = self.live_layer(snap)?;
        Ok(SnapStats {
            logical_pages: self.cfg.mem_pages,
            owned_pages: layer.pages.len() as u64,
            chain_len: layer.chain_len,
        })
    }

    /// Store-wide statistics.
    pub fn store_stats(&self) -> StoreStats {
        let snapshots = self.layers.values().filter(|l| l.refcount > 0).count() as u64;
        let vm_state_bytes: u64 = self.layers.values().map(|l| l.vm_state.len() as u64).sum();
        StoreStats {
            snapshots,
            stored_unique_pages: self.pages.len() as u64,
            logical_pages_total: snapshots.saturating_mul(self.cfg.mem_pages),
            bytes_resident: (self.pages.len() as u64).saturating_mul(PAGE_SIZE as u64)
                + vm_state_bytes,
        }
    }

    /// Look up a snapshot that is still live (refcount > 0). Released snapshots are
    /// indistinguishable from unknown ones at the public API.
    fn live_layer(&self, snap: SnapshotId) -> Result<&Layer, StoreError> {
        self.layers
            .get(&snap.0)
            .filter(|l| l.refcount > 0)
            .ok_or(StoreError::UnknownSnapshot(snap))
    }

    fn live_layer_mut(&mut self, snap: SnapshotId) -> Result<&mut Layer, StoreError> {
        self.layers
            .get_mut(&snap.0)
            .filter(|l| l.refcount > 0)
            .ok_or(StoreError::UnknownSnapshot(snap))
    }

    /// Resolve what `start`'s logical image holds at `gfn` by walking the chain:
    /// nearest layer (self included) that wrote the gfn wins, else zero. Worst case
    /// O(chain length); every layer visited on a miss memoizes the answer, making
    /// repeated reads of the same gfn O(1) for the whole visited path.
    fn resolve(&self, start: u64, gfn: u64) -> PageRef {
        let mut visited: Vec<u64> = Vec::new();
        let mut cur = Some(start);
        let mut result = PageRef::Zero;
        while let Some(id) = cur {
            let Some(layer) = self.layers.get(&id) else {
                // Unreachable: ancestors of resident layers are resident.
                debug_assert!(false, "dangling parent link");
                break;
            };
            if let Some(&p) = layer.pages.get(&gfn) {
                result = p;
                break;
            }
            if let Some(&p) = layer.resolve_cache.borrow().get(&gfn) {
                result = p;
                break;
            }
            visited.push(id);
            cur = layer.parent;
        }
        // A hit found below `visited[i]` is, by construction, also the resolution for
        // every visited layer (none of them wrote the gfn), so memoize it on the path.
        for id in visited {
            if let Some(layer) = self.layers.get(&id) {
                layer.resolve_cache.borrow_mut().insert(gfn, result);
            }
        }
        result
    }

    /// Intern one page's content, bumping its refcount.
    ///
    /// Content addressing treats BLAKE3 equality as content equality. BLAKE3 is a
    /// 256-bit cryptographic hash: the chance of two distinct pages colliding is
    /// ~2^-128 even after hashing astronomically many pages — far below e.g. the rate
    /// of undetected RAM corruption — so, like git or any content-addressed store, we
    /// accept that theoretical risk and never do byte-wise confirmation.
    fn intern_page(&mut self, hash: PageHash, data: &[u8]) {
        self.pages
            .entry(hash)
            .and_modify(|e| e.refs = e.refs.saturating_add(1))
            .or_insert_with(|| PageEntry {
                data: data.into(),
                refs: 1,
            });
    }

    /// Drop one reference to a stored page, removing it at zero.
    /// Returns the number of payload bytes freed (0 or PAGE_SIZE).
    fn release_page_ref(&mut self, hash: PageHash) -> u64 {
        match self.pages.get_mut(&hash) {
            Some(entry) if entry.refs > 1 => {
                entry.refs -= 1;
                0
            }
            Some(_) => {
                self.pages.remove(&hash);
                PAGE_SIZE as u64
            }
            None => {
                // Unreachable: refs are only handed out by intern_page.
                debug_assert!(false, "release of untracked page");
                0
            }
        }
    }
}

/// Shared guts of [`BaseBuilder`] and [`DeltaBuilder`]: buffered (gfn -> interned
/// content) writes on top of an optional parent.
struct BuilderCore<'a> {
    store: &'a mut Store,
    parent: Option<u64>,
    pages: BTreeMap<u64, PageRef>,
}

impl BuilderCore<'_> {
    fn write_page(&mut self, gfn: u64, data: &[u8]) -> Result<(), StoreError> {
        if data.len() != PAGE_SIZE {
            return Err(StoreError::BadPageLength { len: data.len() });
        }
        if gfn >= self.store.cfg.mem_pages {
            return Err(StoreError::GfnOutOfRange {
                gfn,
                mem_pages: self.store.cfg.mem_pages,
            });
        }
        let hash = *blake3::hash(data).as_bytes();
        let pref = if hash == self.store.zero_hash {
            PageRef::Zero
        } else {
            self.store.intern_page(hash, data);
            PageRef::Data(hash)
        };
        // Last write to a gfn wins; drop the reference the overwritten one held.
        if let Some(PageRef::Data(old)) = self.pages.insert(gfn, pref) {
            self.store.release_page_ref(old);
        }
        Ok(())
    }

    fn seal(mut self, vm_state: Vec<u8>) -> SnapshotId {
        let pages = std::mem::take(&mut self.pages); // leaves Drop nothing to undo
        let mut kept: BTreeMap<u64, PageRef> = BTreeMap::new();
        for (gfn, pref) in pages {
            // A write whose content equals what the chain already resolves to is
            // redundant: resolution yields identical bytes either way, and ancestors
            // are sealed so that can never change. Dropping it keeps `owned_pages`
            // honest ("pages no ancestor provides identically") and snapshots cheap.
            let inherited = match self.parent {
                Some(p) => self.store.resolve(p, gfn),
                None => PageRef::Zero, // base inherits the implicit zero image
            };
            if pref == inherited {
                if let PageRef::Data(hash) = pref {
                    self.store.release_page_ref(hash);
                }
            } else {
                kept.insert(gfn, pref);
            }
        }
        let chain_len = match self.parent {
            Some(p) => self
                .store
                .layers
                .get(&p)
                .map_or(1, |l| l.chain_len.saturating_add(1)),
            None => 1,
        };
        let id = self.store.next_id;
        self.store.next_id += 1;
        self.store.layers.insert(
            id,
            Layer {
                parent: self.parent,
                pages: kept,
                vm_state,
                refcount: 1,
                chain_len,
                // not order-observable: lookup-only memo, never iterated.
                #[allow(clippy::disallowed_types)]
                resolve_cache: RefCell::new(HashMap::new()),
            },
        );
        SnapshotId(id)
    }
}

impl Drop for BuilderCore<'_> {
    /// An abandoned builder must not leak interned pages.
    fn drop(&mut self) {
        let pages = std::mem::take(&mut self.pages);
        for (_gfn, pref) in pages {
            if let PageRef::Data(hash) = pref {
                self.store.release_page_ref(hash);
            }
        }
    }
}

/// Builder for the base layer, from [`Store::begin_base`]. Pages not written before
/// [`BaseBuilder::seal`] are implicitly zero. `seal` consumes the builder, so writing
/// after sealing (or sealing twice) is a compile-time error; dropping the builder
/// without sealing discards all buffered writes.
pub struct BaseBuilder<'a> {
    core: BuilderCore<'a>,
}

impl BaseBuilder<'_> {
    /// Record the content of one page (`data` must be exactly [`PAGE_SIZE`] bytes).
    /// Writing the same gfn again replaces the earlier content.
    pub fn write_page(&mut self, gfn: u64, data: &[u8]) -> Result<(), StoreError> {
        self.core.write_page(gfn, data)
    }

    /// Seal the base layer with its opaque vCPU/device blob, yielding its id.
    /// The new snapshot starts with refcount 1, held by the caller.
    pub fn seal(self, vm_state: Vec<u8>) -> SnapshotId {
        self.core.seal(vm_state)
    }
}

/// Builder for a delta layer, from [`Store::derive`]. Pages not written resolve
/// through the parent chain. Same single-use discipline as [`BaseBuilder`].
pub struct DeltaBuilder<'a> {
    core: BuilderCore<'a>,
}

impl DeltaBuilder<'_> {
    /// Record the content of one page (`data` must be exactly [`PAGE_SIZE`] bytes).
    /// Writing the same gfn again replaces the earlier content.
    pub fn write_page(&mut self, gfn: u64, data: &[u8]) -> Result<(), StoreError> {
        self.core.write_page(gfn, data)
    }

    /// Seal the delta with its opaque vCPU/device blob, yielding its id.
    /// The new snapshot starts with refcount 1, held by the caller.
    pub fn seal(self, vm_state: Vec<u8>) -> SnapshotId {
        self.core.seal(vm_state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(mem_pages: u64) -> StoreConfig {
        StoreConfig { mem_pages }
    }

    #[test]
    fn abandoned_builder_leaks_nothing() {
        let mut store = Store::new(cfg(8));
        let mut b = store.begin_base();
        b.write_page(0, &[1u8; PAGE_SIZE]).unwrap();
        b.write_page(1, &[2u8; PAGE_SIZE]).unwrap();
        drop(b);
        let s = store.store_stats();
        assert_eq!(s.stored_unique_pages, 0);
        assert_eq!(s.bytes_resident, 0);
        assert_eq!(s.snapshots, 0);
    }

    #[test]
    fn overwrite_in_builder_releases_old_content() {
        let mut store = Store::new(cfg(8));
        let mut b = store.begin_base();
        b.write_page(0, &[1u8; PAGE_SIZE]).unwrap();
        b.write_page(0, &[2u8; PAGE_SIZE]).unwrap(); // replaces, must drop [1; ..]
        let id = b.seal(vec![]);
        let s = store.store_stats();
        assert_eq!(s.stored_unique_pages, 1);
        let mut out = [0u8; PAGE_SIZE];
        store.read_page(id, 0, &mut out).unwrap();
        assert_eq!(out, [2u8; PAGE_SIZE]);
    }

    #[test]
    fn zero_writes_are_never_stored() {
        let mut store = Store::new(cfg(8));
        let mut b = store.begin_base();
        b.write_page(3, &[0u8; PAGE_SIZE]).unwrap(); // explicit zeros == implicit zeros
        let base = b.seal(vec![]);
        assert_eq!(store.store_stats().stored_unique_pages, 0);
        assert_eq!(store.stats(base).unwrap().owned_pages, 0);
    }

    #[test]
    fn zero_write_over_data_is_owned_but_unstored() {
        let mut store = Store::new(cfg(8));
        let mut b = store.begin_base();
        b.write_page(0, &[7u8; PAGE_SIZE]).unwrap();
        let base = b.seal(vec![]);
        let mut d = store.derive(base).unwrap();
        d.write_page(0, &[0u8; PAGE_SIZE]).unwrap(); // masks parent data with zeros
        let child = d.seal(vec![]);
        assert_eq!(store.stats(child).unwrap().owned_pages, 1);
        assert_eq!(store.store_stats().stored_unique_pages, 1); // only [7; ..]
        let mut out = [1u8; PAGE_SIZE];
        store.read_page(child, 0, &mut out).unwrap();
        assert_eq!(out, [0u8; PAGE_SIZE]);
        store.read_page(base, 0, &mut out).unwrap();
        assert_eq!(out, [7u8; PAGE_SIZE]);
    }

    #[test]
    fn resolve_memoizes_along_the_path() {
        let mut store = Store::new(cfg(4));
        let base = store.begin_base().seal(vec![]);
        let mut b = store.derive(base).unwrap();
        b.write_page(0, &[9u8; PAGE_SIZE]).unwrap();
        let mid = b.seal(vec![]);
        let leaf = store.derive(mid).unwrap().seal(vec![]);
        let mut out = [0u8; PAGE_SIZE];
        store.read_page(leaf, 0, &mut out).unwrap();
        // leaf missed its own pages and memoized the answer found at `mid`.
        assert_eq!(
            store.layers[&leaf.0].resolve_cache.borrow().get(&0),
            Some(&PageRef::Data(*blake3::hash(&[9u8; PAGE_SIZE]).as_bytes()))
        );
        // a second read hits the memo (observable only as identical results)
        store.read_page(leaf, 0, &mut out).unwrap();
        assert_eq!(out, [9u8; PAGE_SIZE]);
    }

    #[test]
    fn released_snapshot_behaves_as_unknown() {
        let mut store = Store::new(cfg(4));
        let base = store.begin_base().seal(vec![]);
        store.release(base).unwrap();
        let mut out = [0u8; PAGE_SIZE];
        assert!(matches!(
            store.read_page(base, 0, &mut out),
            Err(StoreError::UnknownSnapshot(_))
        ));
        assert!(matches!(
            store.retain(base),
            Err(StoreError::UnknownSnapshot(_))
        ));
        assert!(matches!(
            store.release(base),
            Err(StoreError::UnknownSnapshot(_))
        ));
        assert!(matches!(
            store.derive(base),
            Err(StoreError::UnknownSnapshot(_))
        ));
        assert!(matches!(
            store.stats(base),
            Err(StoreError::UnknownSnapshot(_))
        ));
    }

    #[test]
    fn empty_store_and_zero_sized_image() {
        let mut store = Store::new(cfg(0));
        let base = store.begin_base().seal(b"state".to_vec());
        assert_eq!(store.vm_state(base).unwrap(), b"state");
        let mapping = store.materialize(base).unwrap();
        assert_eq!(mapping.len(), 0);
        assert!(mapping.is_empty());
        assert_eq!(mapping.as_slice(), &[] as &[u8]);
        let mut out = [0u8; PAGE_SIZE];
        assert!(matches!(
            store.read_page(base, 0, &mut out),
            Err(StoreError::GfnOutOfRange { .. })
        ));
    }
}
