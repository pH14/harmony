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

/// Size in bytes of one guest page.
pub const PAGE_SIZE: usize = 4096;

/// The all-zero page, as the comparand of `write_page`'s zero short-circuit. A `static`
/// (not a `const`) so the compare is against one fixed buffer rather than a fresh
/// materialized temporary at each call site.
static ZERO_PAGE: [u8; PAGE_SIZE] = [0u8; PAGE_SIZE];

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
    /// A full guest-memory image had a length other than the configured image size.
    #[error("memory image is {got} bytes, expected {expected}")]
    BadMemoryLength {
        /// The offending image length.
        got: usize,
        /// The configured image length.
        expected: usize,
    },
    /// Resident bytes no longer match the content address sealed for a page.
    #[error("snapshot page integrity check failed at gfn {gfn}")]
    PageIntegrity {
        /// The corrupted guest frame number.
        gfn: u64,
    },
    /// The opaque vCPU/device blob no longer matches its seal-time digest.
    #[error("snapshot vCPU/device state integrity check failed")]
    VmStateIntegrity,
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

/// Hasher for [`PageHash`] keys in [`Store::pages`].
///
/// The key is *already* a 256-bit cryptographic hash, so re-hashing it with SipHash
/// buys nothing but cycles. This folds the written bytes into a `u64` by XOR over
/// 8-byte little-endian chunks: uniform because the digest is, and robust to the
/// standard library's `Hash for [u8; N]` pattern of a length-prefix `write_usize`
/// followed by one `write` of the bytes (the length is a constant 32 for every key, so
/// it contributes a constant term and cannot cluster keys).
#[derive(Default)]
struct PageHashHasher(u64);

impl std::hash::Hasher for PageHashHasher {
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for chunk in chunks {
            self.0 ^= u64::from_le_bytes(*chunk);
        }
        let mut word = [0u8; 8];
        word[..rest.len()].copy_from_slice(rest);
        self.0 ^= u64::from_le_bytes(word);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// [`BuildHasher`](std::hash::BuildHasher) for [`PageHashHasher`].
type BuildPageHashHasher = std::hash::BuildHasherDefault<PageHashHasher>;

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
    /// Seal-time digest checked before the opaque state is decoded.
    vm_state_hash: [u8; 32],
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
    ///
    /// **This map is never iterated.** `store_stats` reads only `.len()`; `gc` iterates
    /// `layers` (a `BTreeMap`), never this. That is what makes an unordered map sound
    /// here: no output, hash, or encoded byte can observe its layout. Any future code
    /// that iterates it must collect-and-sort first, or it is a determinism bug.
    ///
    /// Hash-keyed rather than tree-keyed (task 95 M1.2c): the keys are uniformly-random
    /// BLAKE3 digests, so a `BTreeMap` made every seal/intern/release lookup a
    /// cache-hostile pointer-chasing descent.
    // not order-observable: lookup-only, never iterated (see doc above).
    #[allow(clippy::disallowed_types)]
    pages: HashMap<PageHash, PageEntry, BuildPageHashHasher>,
    /// Hash of the all-zero page. `write_page` detects zero writes by scanning the
    /// bytes rather than hashing them, so this is now only the reference value for the
    /// debug assertion that the two tests agree.
    zero_hash: PageHash,
}

impl Store {
    /// Create an empty store for guest images of `cfg.mem_pages` pages.
    pub fn new(cfg: StoreConfig) -> Store {
        Store {
            cfg,
            next_id: 0,
            layers: BTreeMap::new(),
            // not order-observable: `pages` is lookup-only and never iterated; see its
            // field doc. `HashMap` is the point of M1.2c.
            #[allow(clippy::disallowed_types)]
            pages: HashMap::default(),
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

    /// Seal a new flat base containing the logical image of `parent` plus the
    /// pages in `dirty` that changed after `parent` was sealed.
    ///
    /// This is the bounded-chain flattening path. A normal base walk examines every
    /// page in the configured image. Flattening instead walks the page sets in the
    /// chain rooted at `parent`, adds the newly dirtied frames, then reads only that
    /// union from `memory`. The base builder still performs the canonical zero-page
    /// check, so a page that was changed back to zero is omitted from the flat layer
    /// and resolves to the implicit zero page. `dirty` must be complete when supplied
    /// (the vmm-core caller only passes a successful dirty-log drain).
    ///
    /// The resulting layer is an independent base (`parent = None`) and therefore
    /// has `chain_len == 1`. Its capture cost is O(number of page-set entries since
    /// the chain root + newly dirtied pages), rather than O(logical image size).
    pub fn flatten_base(
        &mut self,
        parent: SnapshotId,
        memory: &[u8],
        dirty: &[u64],
        vm_state: Vec<u8>,
    ) -> Result<SnapshotId, StoreError> {
        let expected = self
            .cfg
            .mem_pages
            .checked_mul(PAGE_SIZE as u64)
            .and_then(|len| usize::try_from(len).ok())
            .ok_or(StoreError::BadMemoryLength {
                got: memory.len(),
                expected: usize::MAX,
            })?;
        if memory.len() != expected {
            return Err(StoreError::BadMemoryLength {
                got: memory.len(),
                expected,
            });
        }

        // Validate the parent before collecting its chain. Every ancestor of a live
        // layer is resident, so the walk is total once this check succeeds.
        self.live_layer(parent)?;
        // Resolve the parent's logical image only over the keys the chain ever
        // wrote. Keeping the resolved PageRef lets the new base retain existing
        // page-address entries directly; unchanged root pages therefore avoid both
        // a RAM read and a second BLAKE3 hash.
        let mut inherited: BTreeMap<u64, PageRef> = BTreeMap::new();
        let mut cur = Some(parent.0);
        while let Some(id) = cur {
            let Some(layer) = self.layers.get(&id) else {
                debug_assert!(false, "dangling parent link");
                break;
            };
            for (&gfn, &pref) in &layer.pages {
                inherited.entry(gfn).or_insert(pref);
            }
            cur = layer.parent;
        }
        let mut candidates: BTreeSet<u64> = inherited.keys().copied().collect();
        let mut dirty_set = BTreeSet::new();
        for &gfn in dirty {
            if gfn >= self.cfg.mem_pages {
                return Err(StoreError::GfnOutOfRange {
                    gfn,
                    mem_pages: self.cfg.mem_pages,
                });
            }
            candidates.insert(gfn);
            dirty_set.insert(gfn);
        }

        let mem_pages = self.cfg.mem_pages;
        let mut builder = self.begin_base();
        for gfn in candidates {
            // The image length and gfn bound above make this checked offset
            // arithmetic infallible, while retaining a total error path if the
            // representation is ever changed independently.
            let inherited = inherited.get(&gfn).copied().unwrap_or(PageRef::Zero);
            if !dirty_set.contains(&gfn) {
                // No write has occurred since `parent`, so the parent's resolved
                // content is already the current content. Reuse the content address
                // without touching the (potentially very large) RAM image.
                builder.core.insert_page_ref(gfn, inherited)?;
                continue;
            }
            let offset = usize::try_from(gfn)
                .ok()
                .and_then(|gfn| gfn.checked_mul(PAGE_SIZE))
                .ok_or(StoreError::GfnOutOfRange { gfn, mem_pages })?;
            let end = offset
                .checked_add(PAGE_SIZE)
                .ok_or(StoreError::GfnOutOfRange { gfn, mem_pages })?;
            builder
                .core
                .write_page_against(gfn, &memory[offset..end], inherited)?;
        }
        Ok(builder.seal(vm_state))
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
                Some(entry) if blake3::hash(&entry.data).as_bytes() == &hash => {
                    out.copy_from_slice(&entry.data);
                }
                Some(_) | None => return Err(StoreError::PageIntegrity { gfn }),
            },
        }
        Ok(())
    }

    /// Return the target contents for every page that may differ between two
    /// snapshots.
    ///
    /// The result is sorted by guest frame number. For `Some(from)`, it contains
    /// the union of the page keys recorded on the two chains, stopping at their
    /// nearest common ancestor; each returned page is resolved from `to`, so a
    /// page that is absent there is returned as an all-zero page. When `from` is
    /// `None`, every logical page is returned, including pages resolved to zero.
    /// Both snapshot ids must still be live.
    pub fn diff_pages(
        &self,
        from: Option<SnapshotId>,
        to: SnapshotId,
    ) -> Result<Vec<(u64, [u8; PAGE_SIZE])>, StoreError> {
        self.live_layer(to)?;

        let Some(from) = from else {
            let mut pages = Vec::new();
            for gfn in 0..self.cfg.mem_pages {
                let mut page = [0u8; PAGE_SIZE];
                self.read_page(to, gfn, &mut page)?;
                pages.push((gfn, page));
            }
            return Ok(pages);
        };

        self.live_layer(from)?;

        // Find the nearest common ancestor by recording `from`'s complete chain
        // and walking `to` toward its root. BTreeSet keeps the traversal's
        // membership checks deterministic; the set is not part of the output.
        let mut from_ancestors = BTreeSet::new();
        let mut cur = Some(from.0);
        while let Some(id) = cur {
            if !from_ancestors.insert(id) {
                // A cycle cannot be produced by the sealed-builder API. Treat
                // corrupted internal topology as an unknown snapshot rather
                // than risking an unbounded walk.
                return Err(StoreError::UnknownSnapshot(SnapshotId(id)));
            }
            let Some(layer) = self.layers.get(&id) else {
                return Err(StoreError::UnknownSnapshot(SnapshotId(id)));
            };
            cur = layer.parent;
        }

        let mut common = None;
        let mut to_visited = BTreeSet::new();
        cur = Some(to.0);
        while let Some(id) = cur {
            if !to_visited.insert(id) {
                return Err(StoreError::UnknownSnapshot(SnapshotId(id)));
            }
            if from_ancestors.contains(&id) {
                common = Some(id);
                break;
            }
            let Some(layer) = self.layers.get(&id) else {
                return Err(StoreError::UnknownSnapshot(SnapshotId(id)));
            };
            cur = layer.parent;
        }

        // Collect page keys from each side, excluding the common ancestor. A
        // cycle guard makes this total even if an internal link is corrupted.
        let mut changed_gfns = BTreeSet::new();
        for start in [from.0, to.0] {
            let mut side_visited = BTreeSet::new();
            cur = Some(start);
            while let Some(id) = cur {
                if Some(id) == common {
                    break;
                }
                if !side_visited.insert(id) {
                    return Err(StoreError::UnknownSnapshot(SnapshotId(id)));
                }
                let Some(layer) = self.layers.get(&id) else {
                    return Err(StoreError::UnknownSnapshot(SnapshotId(id)));
                };
                changed_gfns.extend(layer.pages.keys().copied());
                cur = layer.parent;
            }
        }

        let mut pages = Vec::with_capacity(changed_gfns.len());
        for gfn in changed_gfns {
            let mut page = [0u8; PAGE_SIZE];
            self.read_page(to, gfn, &mut page)?;
            pages.push((gfn, page));
        }
        Ok(pages)
    }

    /// The opaque vCPU/device blob recorded at seal time.
    pub fn vm_state(&self, snap: SnapshotId) -> Result<&[u8], StoreError> {
        let layer = self.live_layer(snap)?;
        if blake3::hash(&layer.vm_state).as_bytes() != &layer.vm_state_hash {
            return Err(StoreError::VmStateIntegrity);
        }
        Ok(&layer.vm_state)
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

        let mut verified_pages = Vec::with_capacity(resolved.len());
        for (&gfn, &pref) in &resolved {
            let PageRef::Data(hash) = pref else {
                continue;
            };
            let Some(entry) = self.pages.get(&hash) else {
                return Err(StoreError::PageIntegrity { gfn });
            };
            if blake3::hash(&entry.data).as_bytes() != &hash {
                return Err(StoreError::PageIntegrity { gfn });
            }
            verified_pages.push((gfn, &*entry.data));
        }

        let file = tempfile::tempfile()?;
        file.set_len(len)?;
        // One write mapping, one memcpy per resolved non-zero page — not a `seek` +
        // `write_all` pair of syscalls each. Zero/absent pages are skipped entirely, so
        // the file stays sparse.
        Mapping::populate(&file, len, verified_pages.into_iter())?;
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

    /// Deliberately corrupt one resident page byte for an integrity negative.
    #[cfg(feature = "test-utils")]
    pub fn corrupt_page_for_test(
        &mut self,
        snap: SnapshotId,
        gfn: u64,
        byte: usize,
        mask: u8,
    ) -> Result<(), StoreError> {
        self.live_layer(snap)?;
        if gfn >= self.cfg.mem_pages {
            return Err(StoreError::GfnOutOfRange {
                gfn,
                mem_pages: self.cfg.mem_pages,
            });
        }
        let PageRef::Data(hash) = self.resolve(snap.0, gfn) else {
            return Err(StoreError::BuilderMisuse(
                "cannot corrupt an implicit zero page",
            ));
        };
        let target = self
            .pages
            .get_mut(&hash)
            .and_then(|entry| entry.data.get_mut(byte))
            .ok_or(StoreError::BuilderMisuse(
                "corruption byte lies outside a resident page",
            ))?;
        *target ^= mask;
        Ok(())
    }

    /// Deliberately corrupt one opaque state byte for an integrity negative.
    #[cfg(feature = "test-utils")]
    pub fn corrupt_vm_state_for_test(
        &mut self,
        snap: SnapshotId,
        byte: usize,
        mask: u8,
    ) -> Result<(), StoreError> {
        let target =
            self.live_layer_mut(snap)?
                .vm_state
                .get_mut(byte)
                .ok_or(StoreError::BuilderMisuse(
                    "corruption byte lies outside vCPU/device state",
                ))?;
        *target ^= mask;
        Ok(())
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
        // A booted guest image is mostly zeros, and `blake3(data) == zero_hash` iff
        // `data` is the zero page (to the collision bound `intern_page` documents), so
        // testing the bytes directly is semantically identical to hashing and comparing
        // — and skips the hash for the majority of frames.
        //
        // `data` is exactly PAGE_SIZE bytes (checked above), so this slice compare
        // lowers to one `bcmp` over two 4 KiB buffers. Measured on the bench machine
        // (Apple M1 Max, release): 0.12 us/page, against 1.35 us/page for the
        // `data.iter().all(|&b| b == 0)` form the task spec suggests — that one does
        // *not* vectorize on aarch64, and over the 393,216 zero frames of a 2 GiB guest
        // the difference is ~0.5 s on every seal. Same semantics, no new dependency.
        let pref = if data == &ZERO_PAGE[..] {
            PageRef::Zero
        } else {
            let hash = *blake3::hash(data).as_bytes();
            debug_assert_ne!(
                hash, self.store.zero_hash,
                "non-zero page hashed to zero_hash"
            );
            self.store.intern_page(hash, data);
            PageRef::Data(hash)
        };
        // Last write to a gfn wins; drop the reference the overwritten one held.
        if let Some(PageRef::Data(old)) = self.pages.insert(gfn, pref) {
            self.store.release_page_ref(old);
        }
        Ok(())
    }

    /// Retain an already-interned page reference in this builder. Used by chain
    /// flattening for frames that did not change after the parent seal, avoiding a
    /// RAM copy and a redundant content hash.
    fn insert_page_ref(&mut self, gfn: u64, pref: PageRef) -> Result<(), StoreError> {
        if let PageRef::Data(hash) = pref {
            let Some(entry) = self.store.pages.get_mut(&hash) else {
                return Err(StoreError::PageIntegrity { gfn });
            };
            entry.refs = entry.refs.saturating_add(1);
        }
        if let Some(PageRef::Data(old)) = self.pages.insert(gfn, pref) {
            self.store.release_page_ref(old);
        }
        Ok(())
    }

    /// Record a page only when its bytes differ from `inherited`.
    ///
    /// A dirty log may conservatively report a write that stores the same bytes.
    /// Comparing against the existing interned page (or the implicit zero page)
    /// avoids hashing such a false positive and keeps flattening proportional to
    /// pages whose content actually changed since the chain root.
    fn write_page_against(
        &mut self,
        gfn: u64,
        data: &[u8],
        inherited: PageRef,
    ) -> Result<(), StoreError> {
        if data.len() != PAGE_SIZE {
            return Err(StoreError::BadPageLength { len: data.len() });
        }
        let unchanged = match inherited {
            PageRef::Zero => data == &ZERO_PAGE[..],
            PageRef::Data(hash) => {
                let Some(entry) = self.store.pages.get(&hash) else {
                    return Err(StoreError::PageIntegrity { gfn });
                };
                entry.data.as_ref() == data
            }
        };
        if unchanged {
            self.insert_page_ref(gfn, inherited)
        } else {
            self.write_page(gfn, data)
        }
    }

    fn seal(mut self, vm_state: Vec<u8>) -> SnapshotId {
        let vm_state_hash = *blake3::hash(&vm_state).as_bytes();
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
                vm_state_hash,
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

    /// Task 95 M1.2c: the XOR-folding hasher must behave as a hasher — equal keys hash
    /// equal, the fold spreads distinct keys, and a `HashMap` under it round-trips.
    #[test]
    fn page_hash_hasher_backs_a_working_map() {
        use std::hash::{BuildHasher, Hash, Hasher};

        fn fold(key: &PageHash) -> u64 {
            let mut h = PageHashHasher::default();
            key.hash(&mut h);
            h.finish()
        }

        // Equal keys fold equally; distinct keys (here: known BLAKE3 digests) do not.
        let a: PageHash = *blake3::hash(b"a").as_bytes();
        let b: PageHash = *blake3::hash(b"b").as_bytes();
        let zero: PageHash = [0u8; 32];
        assert_eq!(fold(&a), fold(&a));
        assert_ne!(fold(&a), fold(&b));

        // `write` alone folds only the bytes; `Hash for [u8; 32]` prepends a
        // `write_usize(32)` length prefix, so `fold` carries that constant term for
        // every key — a constant cannot cluster keys.
        let mut raw = PageHashHasher::default();
        raw.write(&zero);
        assert_eq!(raw.finish(), 0);
        assert_ne!(fold(&zero), 0);

        // A non-word-sized write must fold its padded tail. Full 32-byte page
        // hashes never take this branch, so pin it independently.
        let mut tail = PageHashHasher::default();
        tail.write(&[0x12, 0x34, 0x56]);
        assert_eq!(tail.finish(), 0x56_3412);

        // The `Hasher` contract permits partial writes even though PageHash keys
        // use only complete words. Pin the zero-padded tail and its XOR behavior.
        let mut partial = PageHashHasher::default();
        partial.write(&[1]);
        assert_eq!(partial.finish(), 1);
        partial.write(&[1]);
        assert_eq!(partial.finish(), 0);

        // XOR-folding is not a mixer: two keys whose four 8-byte words cancel to the
        // same value fold identically — [0xFF; 32] and [0; 32] both cancel to 0. That
        // is sound *here* only because these keys are BLAKE3 digests of page content,
        // never attacker-shaped inputs — the same premise `intern_page` documents.
        // Pinned, so that keying this map on anything else trips a failing test.
        assert_eq!(fold(&[0xFFu8; 32]), fold(&zero));

        // A difference in any single byte moves the fold.
        for probe in [0usize, 7, 8, 31] {
            let mut k = zero;
            k[probe] = 1;
            assert_ne!(
                fold(&k),
                fold(&zero),
                "byte {probe} does not reach the fold"
            );
        }

        // Over real digests the fold disperses: 512 distinct keys, 512 distinct folds.
        let keys: Vec<PageHash> = (0..512u32)
            .map(|i| *blake3::hash(&i.to_le_bytes()).as_bytes())
            .collect();
        let folds: BTreeSet<u64> = keys.iter().map(fold).collect();
        assert_eq!(
            folds.len(),
            keys.len(),
            "XOR-fold collided on BLAKE3 digests"
        );

        // Insert / lookup / remove round-trip through the real BuildHasher.
        #[allow(clippy::disallowed_types)] // not order-observable: test-local, never iterated
        let mut map: HashMap<PageHash, u32, BuildPageHashHasher> = HashMap::default();
        for (i, k) in keys.iter().enumerate() {
            assert!(map.insert(*k, i as u32).is_none());
        }
        assert_eq!(map.len(), keys.len());
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(map.get(k), Some(&(i as u32)));
        }
        for k in &keys {
            assert!(map.remove(k).is_some());
        }
        assert!(map.is_empty());
        assert_eq!(BuildPageHashHasher::default().hash_one(a), fold(&a));
    }

    #[test]
    fn flatten_base_walks_chain_and_preserves_zero_transitions() {
        let mut store = Store::new(cfg(8));
        let mut base_image = vec![0u8; 8 * PAGE_SIZE];
        base_image[..PAGE_SIZE].fill(1);
        base_image[PAGE_SIZE..2 * PAGE_SIZE].fill(2);
        let mut builder = store.begin_base();
        for (gfn, frame) in base_image.as_chunks::<PAGE_SIZE>().0.iter().enumerate() {
            builder.write_page(gfn as u64, frame).unwrap();
        }
        let base = builder.seal(Vec::new());

        // Page 0 changes to zero and page 2 changes from implicit zero to 3.
        let mut child_image = base_image.clone();
        child_image[..PAGE_SIZE].fill(0);
        child_image[2 * PAGE_SIZE..3 * PAGE_SIZE].fill(3);
        let mut builder = store.derive(base).unwrap();
        builder.write_page(0, &child_image[..PAGE_SIZE]).unwrap();
        builder
            .write_page(2, &child_image[2 * PAGE_SIZE..3 * PAGE_SIZE])
            .unwrap();
        let child = builder.seal(Vec::new());

        // Page 1 changes to zero, then the current image dirties page 5 to 9.
        let mut grand_image = child_image.clone();
        grand_image[PAGE_SIZE..2 * PAGE_SIZE].fill(0);
        let mut builder = store.derive(child).unwrap();
        builder
            .write_page(1, &grand_image[PAGE_SIZE..2 * PAGE_SIZE])
            .unwrap();
        let grand = builder.seal(Vec::new());
        let mut current = grand_image;
        current[5 * PAGE_SIZE..6 * PAGE_SIZE].fill(9);

        let flat = store
            .flatten_base(grand, &current, &[5], Vec::new())
            .unwrap();
        assert_eq!(store.stats(flat).unwrap().chain_len, 1);
        // Only page 2 and the newly dirty page 5 are non-zero in the candidate
        // union. Pages 0 and 1 are explicitly zero and remain implicit in a base.
        assert_eq!(store.stats(flat).unwrap().owned_pages, 2);
        let mut page = [0u8; PAGE_SIZE];
        for gfn in 0..8 {
            store.read_page(flat, gfn, &mut page).unwrap();
            assert_eq!(
                page,
                current[gfn as usize * PAGE_SIZE..(gfn as usize + 1) * PAGE_SIZE]
            );
        }
    }

    #[test]
    fn flatten_base_rejects_wrong_image_and_dirty_gfn() {
        let mut store = Store::new(cfg(2));
        let base = store.begin_base().seal(Vec::new());
        assert!(matches!(
            store.flatten_base(base, &[0; PAGE_SIZE], &[], Vec::new()),
            Err(StoreError::BadMemoryLength { got, expected })
                if got == PAGE_SIZE && expected == 2 * PAGE_SIZE
        ));
        let memory = vec![0u8; 2 * PAGE_SIZE];
        assert!(matches!(
            store.flatten_base(base, &memory, &[2], Vec::new()),
            Err(StoreError::GfnOutOfRange {
                gfn: 2,
                mem_pages: 2
            })
        ));
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

    /// Task 95 M1.2a: the byte-scan short-circuit must agree with the old
    /// `blake3(data) == zero_hash` test on every shape of "nearly zero" page — a
    /// single non-zero byte anywhere still interns exactly one content.
    #[test]
    fn zero_shortcut_matches_hash_comparison() {
        let mut store = Store::new(cfg(8));
        // A page that is zero everywhere except one byte is *not* the zero page, at
        // either end of the buffer or in the middle.
        for probe in [0usize, 1, PAGE_SIZE / 2, PAGE_SIZE - 2, PAGE_SIZE - 1] {
            let mut data = [0u8; PAGE_SIZE];
            data[probe] = 1;
            assert_ne!(
                *blake3::hash(&data).as_bytes(),
                store.zero_hash,
                "byte {probe} set must not hash to zero_hash"
            );
            let mut b = store.begin_base();
            b.write_page(0, &data).unwrap();
            let snap = b.seal(vec![]);
            assert_eq!(store.stats(snap).unwrap().owned_pages, 1);
            let mut out = [0u8; PAGE_SIZE];
            store.read_page(snap, 0, &mut out).unwrap();
            assert_eq!(out, data);
            store.release(snap).unwrap();
            store.gc();
        }
        assert_eq!(store.store_stats().stored_unique_pages, 0);

        // ...and the all-zero page is still never interned, whichever path wrote it.
        let mut b = store.begin_base();
        b.write_page(0, &[0u8; PAGE_SIZE]).unwrap();
        let snap = b.seal(vec![]);
        assert_eq!(store.stats(snap).unwrap().owned_pages, 0);
        assert_eq!(store.store_stats().stored_unique_pages, 0);
    }

    /// Task 95 M1.2a: a zero write that *overwrites* a buffered non-zero write in the
    /// same builder must release the interned content it displaced.
    #[test]
    fn zero_write_over_buffered_data_releases_the_content() {
        let mut store = Store::new(cfg(8));
        let mut b = store.begin_base();
        b.write_page(0, &[7u8; PAGE_SIZE]).unwrap();
        assert_eq!(b.core.store.store_stats().stored_unique_pages, 1);
        b.write_page(0, &[0u8; PAGE_SIZE]).unwrap(); // zero short-circuit, still frees
        assert_eq!(b.core.store.store_stats().stored_unique_pages, 0);
        let snap = b.seal(vec![]);
        assert_eq!(store.stats(snap).unwrap().owned_pages, 0); // == the implicit zero base
        assert_eq!(store.store_stats().stored_unique_pages, 0);
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
    fn diff_pages_sibling_branches_are_sorted_and_target_resolved() {
        let mut store = Store::new(cfg(4));
        let base = store.begin_base().seal(vec![]);

        let mut left_builder = store.derive(base).unwrap();
        left_builder.write_page(1, &[0x11; PAGE_SIZE]).unwrap();
        let left = left_builder.seal(vec![]);

        let mut right_builder = store.derive(base).unwrap();
        right_builder.write_page(2, &[0x22; PAGE_SIZE]).unwrap();
        let right = right_builder.seal(vec![]);

        // Page 1 is present only on the source side, so the target's implicit
        // zero is returned. Page 2 is present on the target side. The common
        // ancestor's (empty) page set is not included.
        assert_eq!(
            store.diff_pages(Some(left), right).unwrap(),
            vec![(1, [0u8; PAGE_SIZE]), (2, [0x22u8; PAGE_SIZE])]
        );
    }

    #[test]
    fn diff_pages_ancestor_descendant_handles_explicit_and_implicit_zero() {
        let mut store = Store::new(cfg(4));
        let mut base_builder = store.begin_base();
        base_builder.write_page(0, &[0x10; PAGE_SIZE]).unwrap();
        let base = base_builder.seal(vec![]);

        let mut child_builder = store.derive(base).unwrap();
        child_builder.write_page(0, &[0u8; PAGE_SIZE]).unwrap();
        child_builder.write_page(2, &[0x20; PAGE_SIZE]).unwrap();
        let child = child_builder.seal(vec![]);

        assert_eq!(
            store.diff_pages(Some(base), child).unwrap(),
            vec![(0, [0u8; PAGE_SIZE]), (2, [0x20u8; PAGE_SIZE])]
        );
        assert_eq!(
            store.diff_pages(Some(child), base).unwrap(),
            vec![(0, [0x10u8; PAGE_SIZE]), (2, [0u8; PAGE_SIZE])]
        );
    }

    #[test]
    fn diff_pages_identical_snapshots_are_empty() {
        let mut store = Store::new(cfg(4));
        let base = store.begin_base().seal(vec![]);
        let child = store.derive(base).unwrap().seal(vec![]);

        assert!(store.diff_pages(Some(base), base).unwrap().is_empty());
        assert!(store.diff_pages(Some(base), child).unwrap().is_empty());
    }

    #[test]
    fn diff_pages_from_none_returns_full_resolved_image() {
        let mut store = Store::new(cfg(4));
        let mut base_builder = store.begin_base();
        base_builder.write_page(1, &[0x11; PAGE_SIZE]).unwrap();
        let base = base_builder.seal(vec![]);
        let mut child_builder = store.derive(base).unwrap();
        child_builder.write_page(2, &[0x22; PAGE_SIZE]).unwrap();
        let child = child_builder.seal(vec![]);

        assert_eq!(
            store.diff_pages(None, child).unwrap(),
            vec![
                (0, [0u8; PAGE_SIZE]),
                (1, [0x11u8; PAGE_SIZE]),
                (2, [0x22u8; PAGE_SIZE]),
                (3, [0u8; PAGE_SIZE]),
            ]
        );
    }

    #[test]
    fn diff_pages_rejects_unknown_ids() {
        let mut store = Store::new(cfg(1));
        let base = store.begin_base().seal(vec![]);
        let unknown = SnapshotId(u64::MAX);

        assert!(matches!(
            store.diff_pages(Some(unknown), base),
            Err(StoreError::UnknownSnapshot(id)) if id == unknown
        ));
        assert!(matches!(
            store.diff_pages(Some(base), unknown),
            Err(StoreError::UnknownSnapshot(id)) if id == unknown
        ));
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

    #[cfg(feature = "test-utils")]
    #[test]
    fn sealed_page_and_vm_state_corruption_are_detected() {
        let mut store = Store::new(StoreConfig { mem_pages: 2 });
        let mut builder = store.begin_base();
        builder.write_page(1, &[0x5a; PAGE_SIZE]).unwrap();
        let snap = builder.seal(b"vcpu-device-state".to_vec());

        // The mask equals the source byte: XOR clears it while OR and AND both
        // leave it untouched, making the integrity negative mutation-sensitive.
        store.corrupt_page_for_test(snap, 1, 7, 0x5a).unwrap();
        let mut out = [0_u8; PAGE_SIZE];
        assert!(matches!(
            store.read_page(snap, 1, &mut out),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));
        assert!(matches!(
            store.materialize(snap),
            Err(StoreError::PageIntegrity { gfn: 1 })
        ));

        let mut clean = Store::new(StoreConfig { mem_pages: 1 });
        let snap = clean.begin_base().seal(b"vcpu-device-state".to_vec());
        clean.corrupt_vm_state_for_test(snap, 5, b'd').unwrap();
        assert!(matches!(
            clean.vm_state(snap),
            Err(StoreError::VmStateIntegrity)
        ));
    }
}
