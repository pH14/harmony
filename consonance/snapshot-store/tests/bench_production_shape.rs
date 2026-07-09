// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task 95 M1.1 — the production-shape bench (informational, not pass/fail).
//!
//! `tests/bench.rs` measures the store at a 32 MiB toy shape. This file measures it at
//! the shape production actually runs: the 2 GiB guest of
//! `consonance/vmm-core/tests/seal_rate_sweep.rs` (`GUEST_RAM_LEN = 2 << 30`), i.e.
//! 524,288 frames, on a synthetic *booted-guest* image (mostly zeros, some duplicate
//! page contents). Every number printed here goes into `IMPLEMENTATION.md`.
//!
//! Run with:
//!   cargo test -p snapshot-store --release --test bench_production_shape -- --ignored --nocapture
//!
//! Constrained machines: `HARMONY_BENCH_PAGES=<power of two >= 4096>` scales the shape
//! down. The effective shape is printed in every `[BENCH]` line, so a recorded number is
//! always self-describing. At the default shape the peak RSS is ~4 GiB (the
//! `full_image_vec_copy` floor allocates two full images), so the benches take a process-
//! wide lock and run one at a time.

// not order-observable: this is an informational wall-clock benchmark, not library
// state — `Instant::now` measures elapsed time and never reaches any output that
// affects determinism. The determinism lint targets production state, not bench timing.
#![allow(clippy::disallowed_methods)]

use std::hint::black_box;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use memmap2::MmapOptions;
use snapshot_store::{PAGE_SIZE, SnapshotId, Store, StoreConfig};

/// Production shape: 2 GiB guest (seal_rate_sweep's GUEST_RAM_LEN), overridable for
/// constrained machines via HARMONY_BENCH_PAGES (power of two, >= 4096).
const PROD_MEM_PAGES: u64 = 524_288;
/// Non-zero fraction of the synthetic booted image: 1 in 4 pages (a booted guest is
/// mostly zeros); every 8th non-zero page repeats an earlier content (dedup realism).
const NONZERO_STRIDE: u64 = 4;
const DEDUP_GROUP: u64 = 8;

/// The benches allocate multiple GiB each; run them one at a time even when the test
/// harness would otherwise thread them.
static BENCH_LOCK: Mutex<()> = Mutex::new(());

fn serialize() -> std::sync::MutexGuard<'static, ()> {
    // A panicking bench must not poison the rest of the run.
    BENCH_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Effective guest size in pages for this run.
fn bench_pages() -> u64 {
    match std::env::var("HARMONY_BENCH_PAGES") {
        Err(_) => PROD_MEM_PAGES,
        Ok(raw) => {
            let n: u64 = raw
                .trim()
                .parse()
                .expect("HARMONY_BENCH_PAGES must be a u64");
            assert!(
                n >= 4096 && n.is_power_of_two(),
                "HARMONY_BENCH_PAGES must be a power of two >= 4096, got {n}"
            );
            n
        }
    }
}

/// Shape suffix printed on every `[BENCH]` line.
fn shape(mem_pages: u64) -> String {
    let mib = mem_pages * PAGE_SIZE as u64 / (1 << 20);
    format!("mem_pages={mem_pages} image_mib={mib}")
}

// ---------------------------------------------------------------------------
// Deterministic synthetic image (no `rand`, seeded like `bench.rs::page`).
// ---------------------------------------------------------------------------

fn splitmix64(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Fill one page with content unique to `seed` (distinct seeds ⇒ distinct pages).
fn fill_page(buf: &mut [u8], seed: u64) {
    debug_assert_eq!(buf.len(), PAGE_SIZE);
    for (i, chunk) in buf.chunks_exact_mut(8).enumerate() {
        // (seed, i) -> a single u64; injective for seed < 2^32 and i < 2^32.
        let v = splitmix64(seed.wrapping_mul(0x1_0000_0001).wrapping_add(i as u64));
        chunk.copy_from_slice(&v.to_le_bytes());
    }
}

fn page_of(seed: u64) -> Vec<u8> {
    let mut p = vec![0u8; PAGE_SIZE];
    fill_page(&mut p, seed);
    p
}

/// Content seed of `gfn` in the synthetic booted image, or `None` for a zero page.
///
/// Non-zero iff `gfn % 4 == 0`; among the non-zero pages every 8th reuses the content
/// of the first page of its group of 8, so 1/8 of the non-zero pages dedup away.
fn page_seed(gfn: u64) -> Option<u64> {
    if !gfn.is_multiple_of(NONZERO_STRIDE) {
        return None;
    }
    let idx = gfn / NONZERO_STRIDE;
    Some(if idx % DEDUP_GROUP == DEDUP_GROUP - 1 {
        idx - (DEDUP_GROUP - 1)
    } else {
        idx
    })
}

/// The whole synthetic image as one flat buffer (`mem_pages * PAGE_SIZE` bytes).
fn synthetic_image(mem_pages: u64) -> Vec<u8> {
    let mut img = vec![0u8; mem_pages as usize * PAGE_SIZE];
    for gfn in 0..mem_pages {
        if let Some(seed) = page_seed(gfn) {
            let off = gfn as usize * PAGE_SIZE;
            fill_page(&mut img[off..off + PAGE_SIZE], seed);
        }
    }
    img
}

fn frame(img: &[u8], gfn: u64) -> &[u8] {
    let off = gfn as usize * PAGE_SIZE;
    &img[off..off + PAGE_SIZE]
}

/// Fault every page of `img` in before timing anything against it.
///
/// `vec![0u8; n]` is a lazy anonymous mapping: the ~3/4 of the image that stays zero is
/// never touched by `synthetic_image`, so the *first* loop to read it eats several
/// hundred ms of minor faults. Without this the hash-only baseline (which runs first)
/// measures the faults and comes out slower than the full seal it is a baseline for.
fn warm(img: &[u8]) {
    let mut sink = 0u8;
    for off in (0..img.len()).step_by(PAGE_SIZE) {
        sink ^= img[off];
    }
    black_box(sink);
}

/// Seal a base holding the whole synthetic image.
fn seal_full_base(store: &mut Store, img: &[u8], mem_pages: u64) -> SnapshotId {
    let mut b = store.begin_base();
    for gfn in 0..mem_pages {
        b.write_page(gfn, frame(img, gfn)).unwrap();
    }
    b.seal(vec![0u8; 64])
}

// ---------------------------------------------------------------------------
// Timing helpers
// ---------------------------------------------------------------------------

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort_unstable();
    v[v.len() / 2]
}

fn us_per(d: Duration, n: u64) -> f64 {
    d.as_secs_f64() * 1e6 / n as f64
}

// ---------------------------------------------------------------------------
// 1. base_seal — what `SnapshotEngine::snapshot_base` costs at production shape.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "informational bench; run with --release --ignored --nocapture"]
fn base_seal() {
    let _g = serialize();
    let mem_pages = bench_pages();
    let img = synthetic_image(mem_pages);
    warm(&img);

    // Same-shape baseline: just BLAKE3 over every frame, no store at all. The gap
    // between this and `total` is intern/alloc/BTreeMap cost.
    let t = Instant::now();
    let mut sink = 0u8;
    for gfn in 0..mem_pages {
        sink ^= blake3::hash(frame(&img, gfn)).as_bytes()[0];
    }
    let hash_only = t.elapsed();
    black_box(sink);

    // The floor the *post-M1.2a* store can reach: it hashes only the non-zero frames.
    let t = Instant::now();
    let mut sink = 0u8;
    for gfn in 0..mem_pages {
        if page_seed(gfn).is_some() {
            sink ^= blake3::hash(frame(&img, gfn)).as_bytes()[0];
        }
    }
    let hash_nonzero = t.elapsed();
    black_box(sink);

    // Split the write loop (hash + zero-test + intern) from `seal` (the redundancy pass
    // over every buffered write) — they point at different optimizations, and M1.2d's
    // go/no-go turns on which one holds the residual over the hash-only baseline.
    let mut store = Store::new(StoreConfig { mem_pages });
    let t = Instant::now();
    let mut b = store.begin_base();
    for gfn in 0..mem_pages {
        b.write_page(gfn, frame(&img, gfn)).unwrap();
    }
    let writes = t.elapsed();
    let t = Instant::now();
    let base = b.seal(vec![0u8; 64]);
    let seal = t.elapsed();
    let total = writes + seal;

    let owned = store.stats(base).unwrap().owned_pages;
    let unique = store.store_stats().stored_unique_pages;
    println!(
        "[BENCH] base_seal {} total_s={:.3} us_per_page={:.3} writes_s={:.3} seal_s={:.3} \
         owned_pages={owned} stored_unique_pages={unique} hash_only_s={:.3} \
         hash_only_us_per_page={:.3} hash_nonzero_s={:.3}",
        shape(mem_pages),
        total.as_secs_f64(),
        us_per(total, mem_pages),
        writes.as_secs_f64(),
        seal.as_secs_f64(),
        hash_only.as_secs_f64(),
        us_per(hash_only, mem_pages),
        hash_nonzero.as_secs_f64(),
    );
}

// ---------------------------------------------------------------------------
// 2. dirty_delta_seal — the M2.1 payoff curve: seal cost vs dirty-set size.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "informational bench; run with --release --ignored --nocapture"]
fn dirty_delta_seal() {
    let _g = serialize();
    let mem_pages = bench_pages();
    let img = synthetic_image(mem_pages);
    let mut store = Store::new(StoreConfig { mem_pages });
    let base = seal_full_base(&mut store, &img, mem_pages);
    drop(img); // the base owns its own copies; free 2 GiB before the deltas

    for n in [512u64, 4_096, 32_768, 262_144] {
        if n * 2 > mem_pages {
            println!(
                "[BENCH] dirty_delta_seal {} n={n} SKIPPED (shape too small)",
                shape(mem_pages)
            );
            continue;
        }
        // Seconds-scale iterations get one run; cheap ones get the median of three.
        let iters = if n <= 32_768 { 3 } else { 1 };
        let mut samples = Vec::with_capacity(iters);
        for it in 0..iters as u64 {
            // Fresh contents per iteration: no cross-iteration intern hits.
            let mut dirty = vec![0u8; n as usize * PAGE_SIZE];
            for i in 0..n {
                let off = i as usize * PAGE_SIZE;
                fill_page(
                    &mut dirty[off..off + PAGE_SIZE],
                    2_000_000 + it * 1_000_000 + i,
                );
            }

            let t = Instant::now();
            let mut d = store.derive(base).unwrap();
            for i in 0..n {
                d.write_page(i * 2, frame(&dirty, i)).unwrap();
            }
            let child = d.seal(vec![0u8; 64]);
            samples.push(t.elapsed());

            assert_eq!(store.stats(child).unwrap().owned_pages, n);
            store.release(child).unwrap();
            store.gc();
        }
        let d = median(samples);
        println!(
            "[BENCH] dirty_delta_seal {} n={n} total_us={} us_per_page={:.3}",
            shape(mem_pages),
            d.as_micros(),
            us_per(d, n),
        );
    }
}

// ---------------------------------------------------------------------------
// 3. full_rescan_delta_seal — what a derive *without* a dirty set costs
//    (the `dirty: None` fallback path). Pairs with bench 2: scan domination.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "informational bench; run with --release --ignored --nocapture"]
fn full_rescan_delta_seal() {
    let _g = serialize();
    let mem_pages = bench_pages();
    let mut img = synthetic_image(mem_pages);
    warm(&img);
    let mut store = Store::new(StoreConfig { mem_pages });
    let base = seal_full_base(&mut store, &img, mem_pages);

    // Exactly `changed` frames actually differ from the base.
    let changed = std::cmp::min(4_096, mem_pages / NONZERO_STRIDE);
    for k in 0..changed {
        let off = (k * NONZERO_STRIDE) as usize * PAGE_SIZE;
        fill_page(&mut img[off..off + PAGE_SIZE], 9_000_000 + k);
    }

    let t = Instant::now();
    let mut d = store.derive(base).unwrap();
    for gfn in 0..mem_pages {
        d.write_page(gfn, frame(&img, gfn)).unwrap();
    }
    let child = d.seal(vec![0u8; 64]);
    let total = t.elapsed();

    let owned = store.stats(child).unwrap().owned_pages;
    assert_eq!(owned, changed);
    println!(
        "[BENCH] full_rescan_delta_seal {} changed={changed} written_frames={mem_pages} \
         total_s={:.3} us_per_page={:.3} owned_pages={owned}",
        shape(mem_pages),
        total.as_secs_f64(),
        us_per(total, mem_pages),
    );
}

// ---------------------------------------------------------------------------
// 4. materialize_sweep — restore-side cost, plus the two floors it is judged against.
// ---------------------------------------------------------------------------

/// Floor (a): the ideal write path — one write-mapping of the sized tempfile, one
/// memcpy per resolved page, flush. This is exactly what M1.2b makes `materialize` do.
///
/// Timed from tempfile creation, like `Store::materialize`, so the difference between
/// the two is purely (chain resolve + per-page syscalls + `map_copy`) and not tempfile
/// setup. The floor omits the final `map_copy`, which is O(1) — it maps lazily.
fn mmap_memcpy_floor(mem_pages: u64, pages: &[(u64, Vec<u8>)]) -> Duration {
    let len = mem_pages * PAGE_SIZE as u64;
    let t = Instant::now();
    let file = tempfile::tempfile().unwrap();
    file.set_len(len).unwrap();

    // SAFETY: `file` is an anonymous unlinked tempfile created, sized, and written
    // exclusively by this bench; no other handle to it exists, so it cannot be
    // truncated or modified behind the map's back.
    let mut map = unsafe { MmapOptions::new().len(len as usize).map_mut(&file).unwrap() };
    for (gfn, data) in pages {
        let off = *gfn as usize * PAGE_SIZE;
        map[off..off + PAGE_SIZE].copy_from_slice(data);
    }
    map.flush().unwrap();
    drop(map);
    t.elapsed()
}

/// Floor (b): the restore memcpy `vmm.rs::restore_guest_memory` does today, and which
/// M2.2 removes — `ram.copy_from_slice(image)` over the whole image.
fn full_image_vec_copy(mem_pages: u64) -> Duration {
    let len = mem_pages as usize * PAGE_SIZE;
    let src = vec![1u8; len];
    let mut dst = vec![2u8; len]; // pre-touched: measure memcpy, not first-touch faults
    dst.copy_from_slice(&src); // warm
    let t = Instant::now();
    dst.copy_from_slice(&src);
    let d = t.elapsed();
    black_box(&dst);
    d
}

#[test]
#[ignore = "informational bench; run with --release --ignored --nocapture"]
fn materialize_sweep() {
    let _g = serialize();
    let mem_pages = bench_pages();

    /// Pages each interior layer of the chain dirties (small, disjoint from the base).
    const LAYER_DIRTY: u64 = 64;
    const MAX_DEPTH: u64 = 32;

    for resident in [4_096u64, 32_768, 131_072] {
        if resident * NONZERO_STRIDE > mem_pages {
            println!(
                "[BENCH] materialize_sweep {} resident={resident} SKIPPED (shape too small)",
                shape(mem_pages)
            );
            continue;
        }
        let mut store = Store::new(StoreConfig { mem_pages });

        // Base holds the resident set at gfn ≡ 0 (mod 4).
        let mut b = store.begin_base();
        let mut base_pages: Vec<(u64, Vec<u8>)> = Vec::with_capacity(resident as usize);
        for i in 0..resident {
            let gfn = i * NONZERO_STRIDE;
            let p = page_of(i);
            b.write_page(gfn, &p).unwrap();
            base_pages.push((gfn, p));
        }
        let base = b.seal(vec![0u8; 64]);

        // One 32-deep chain; the depth-1/8/32 snapshots are prefixes of it. Interior
        // layers dirty small disjoint sets at gfn ≡ 1 (mod 4).
        let mut at_depth: Vec<SnapshotId> = vec![base];
        let mut tip = base;
        for layer in 1..MAX_DEPTH {
            let mut d = store.derive(tip).unwrap();
            for k in 0..LAYER_DIRTY {
                let idx = (layer - 1) * LAYER_DIRTY + k;
                let gfn = 1 + idx * NONZERO_STRIDE;
                assert!(gfn < mem_pages, "chain dirty set overflows the shape");
                d.write_page(gfn, &page_of(5_000_000 + idx)).unwrap();
            }
            tip = d.seal(vec![0u8; 64]);
            at_depth.push(tip);
        }

        for depth in [1usize, 8, 32] {
            let snap = at_depth[depth - 1];
            assert_eq!(store.stats(snap).unwrap().chain_len as usize, depth);
            let samples: Vec<Duration> = (0..3)
                .map(|_| {
                    let t = Instant::now();
                    let m = store.materialize(snap).unwrap();
                    let d = t.elapsed();
                    black_box(m.len());
                    d
                })
                .collect();
            let d = median(samples);
            let resolved = resident + (depth as u64 - 1) * LAYER_DIRTY;
            println!(
                "[BENCH] materialize_sweep {} resident={resident} depth={depth} \
                 resolved_pages={resolved} materialize_ms={:.2}",
                shape(mem_pages),
                d.as_secs_f64() * 1e3,
            );
        }

        let floor = mmap_memcpy_floor(mem_pages, &base_pages);
        println!(
            "[BENCH] materialize_floor_mmap_memcpy {} resident={resident} ms={:.2}",
            shape(mem_pages),
            floor.as_secs_f64() * 1e3,
        );
    }

    let vec_copy = full_image_vec_copy(mem_pages);
    let gib_s =
        (mem_pages as f64 * PAGE_SIZE as f64 / (1u64 << 30) as f64) / vec_copy.as_secs_f64();
    println!(
        "[BENCH] restore_floor_full_image_vec_copy {} ms={:.2} gib_per_s={:.2}",
        shape(mem_pages),
        vec_copy.as_secs_f64() * 1e3,
        gib_s,
    );
}

// ---------------------------------------------------------------------------
// 5. gc_reap — 64 layers, release all but the tip, time `gc()`.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "informational bench; run with --release --ignored --nocapture"]
fn gc_reap() {
    let _g = serialize();
    let mem_pages = bench_pages();
    const LAYERS: u64 = 64;
    const PER_LAYER: u64 = 1_024;

    // (i) 64 siblings off one base: releasing all but the tip actually reaps 63 layers.
    let mut store = Store::new(StoreConfig { mem_pages });
    let base = store.begin_base().seal(vec![0u8; 64]);
    let mut sibs = Vec::with_capacity(LAYERS as usize);
    for l in 0..LAYERS {
        let mut d = store.derive(base).unwrap();
        for k in 0..PER_LAYER {
            d.write_page(k, &page_of(l * PER_LAYER + k)).unwrap();
        }
        sibs.push(d.seal(vec![0u8; 64]));
    }
    store.release(base).unwrap();
    for &s in &sibs[..sibs.len() - 1] {
        store.release(s).unwrap();
    }
    let t = Instant::now();
    let freed = store.gc();
    let reap = t.elapsed();

    // (ii) A 64-deep chain: the tip needs every ancestor, so `gc` reaps nothing and the
    // number is the pure reachability walk.
    let mut store = Store::new(StoreConfig { mem_pages });
    let mut tip = store.begin_base().seal(vec![0u8; 64]);
    let mut chain = vec![tip];
    for l in 0..LAYERS - 1 {
        let mut d = store.derive(tip).unwrap();
        for k in 0..PER_LAYER {
            d.write_page(k, &page_of(7_000_000 + l * PER_LAYER + k))
                .unwrap();
        }
        tip = d.seal(vec![0u8; 64]);
        chain.push(tip);
    }
    for &s in &chain[..chain.len() - 1] {
        store.release(s).unwrap();
    }
    let t = Instant::now();
    let chain_freed = store.gc();
    let walk = t.elapsed();

    println!(
        "[BENCH] gc_reap {} layers={LAYERS} per_layer_pages={PER_LAYER} \
         sibling_reap_ms={:.3} freed_mib={:.1} chain_walk_ms={:.3} chain_freed={chain_freed}",
        shape(mem_pages),
        reap.as_secs_f64() * 1e3,
        freed as f64 / (1u64 << 20) as f64,
        walk.as_secs_f64() * 1e3,
    );
}
