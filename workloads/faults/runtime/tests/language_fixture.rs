// SPDX-License-Identifier: AGPL-3.0-or-later
use antithesis_instrumentation as _;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

static DONE: AtomicBool = AtomicBool::new(false);

fn main() {
    let worker = thread::spawn(|| {
        let mut visits = 0_u64;
        while !DONE.load(Ordering::Relaxed) {
            visits = visits.wrapping_add(1);
            std::hint::black_box(visits);
        }
    });
    println!("HARMONY_LANGUAGE_READY");
    for marker in 1..=20 {
        thread::sleep(Duration::from_millis(10));
        println!("HARMONY_LANGUAGE_MARKER {marker:02}");
    }
    DONE.store(true, Ordering::Relaxed);
    worker.join().unwrap();
}
