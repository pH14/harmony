// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 6 (informational, not pass/fail): timing numbers for IMPLEMENTATION.md.
//! Run with:
//!   cargo test -p snapshot-store --release --test bench -- --ignored --nocapture

// not order-observable: this is an informational wall-clock benchmark, not library
// state — `Instant::now` measures elapsed time and never reaches any output that
// affects determinism. The determinism lint targets production state, not bench timing.
#![allow(clippy::disallowed_methods)]

use std::time::Instant;

use snapshot_store::{PAGE_SIZE, Store, StoreConfig};

fn page(seed: u32) -> [u8; PAGE_SIZE] {
    let mut p = [0u8; PAGE_SIZE];
    for (i, b) in p.iter_mut().enumerate() {
        *b = (seed as usize).wrapping_mul(31).wrapping_add(i) as u8;
    }
    p
}

#[test]
#[ignore = "informational bench; run with --release --ignored --nocapture"]
fn bench_seal_1000_dirty_page_delta() {
    const DIRTY: u32 = 1000;
    let mut store = Store::new(StoreConfig { mem_pages: 8192 });
    let mut b = store.begin_base();
    for gfn in 0..2048u64 {
        b.write_page(gfn, &page(gfn as u32)).unwrap();
    }
    let base = b.seal(vec![0; 64]);

    let t_total = Instant::now();
    let mut d = store.derive(base).unwrap();
    for i in 0..DIRTY {
        // distinct contents, spread over the gfn space
        d.write_page(u64::from(i) * 7 % 8192, &page(100_000 + i))
            .unwrap();
    }
    let t_seal = Instant::now();
    let snap = d.seal(vec![0; 64]);
    let seal_us = t_seal.elapsed().as_micros();
    let total_us = t_total.elapsed().as_micros();

    let owned = store.stats(snap).unwrap().owned_pages;
    println!(
        "seal of a {DIRTY}-dirty-page delta: seal() {seal_us} us, \
         derive+writes+seal {total_us} us ({owned} owned pages)"
    );
}

#[test]
#[ignore = "informational bench; run with --release --ignored --nocapture"]
fn bench_read_page_at_chain_depth_64() {
    const DEPTH: usize = 64;
    const MEM_PAGES: u64 = 1024;
    let mut store = Store::new(StoreConfig {
        mem_pages: MEM_PAGES,
    });
    let mut b = store.begin_base();
    for gfn in 0..MEM_PAGES / 2 {
        b.write_page(gfn * 2, &page(gfn as u32)).unwrap();
    }
    let mut snap = b.seal(vec![]);
    for layer in 1..DEPTH {
        let mut d = store.derive(snap).unwrap();
        for k in 0..16u64 {
            let gfn = (layer as u64 * 37 + k * 61) % MEM_PAGES;
            d.write_page(gfn, &page((layer * 1000 + k as usize) as u32))
                .unwrap();
        }
        snap = d.seal(vec![]);
    }

    let mut out = [0u8; PAGE_SIZE];
    let mut sink = 0u64;

    // Cold: first read of each gfn walks the chain (then memoizes along the path).
    let t = Instant::now();
    for gfn in 0..MEM_PAGES {
        store.read_page(snap, gfn, &mut out).unwrap();
        sink = sink.wrapping_add(u64::from(out[0]));
    }
    let cold = t.elapsed();

    // Warm: repeated reads hit the per-layer memo index.
    const WARM_PASSES: u64 = 200;
    let t = Instant::now();
    for _ in 0..WARM_PASSES {
        for gfn in 0..MEM_PAGES {
            store.read_page(snap, gfn, &mut out).unwrap();
            sink = sink.wrapping_add(u64::from(out[0]));
        }
    }
    let warm = t.elapsed();

    let cold_per = cold.as_nanos() / u128::from(MEM_PAGES);
    let warm_reads = u128::from(MEM_PAGES * WARM_PASSES);
    let warm_per = warm.as_nanos() / warm_reads;
    let warm_throughput = (warm_reads as f64 / warm.as_secs_f64()) / 1e6;
    println!(
        "read_page at chain depth {DEPTH}: cold {cold_per} ns/read, \
         warm {warm_per} ns/read ({warm_throughput:.1} M reads/s, sink {sink})"
    );
}
