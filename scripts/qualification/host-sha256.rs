// SPDX-License-Identifier: AGPL-3.0-or-later

use std::hint::black_box;
use std::time::Instant;

fn compare<T: Eq + std::fmt::Debug>(
    mut native: impl FnMut() -> T,
    mut software: impl FnMut() -> T,
    kind: &str,
    bytes: usize,
    check: bool,
) {
    let expected = software();
    assert_eq!(native(), expected);
    if check {
        return;
    }
    let iterations = (1024 * 1024 / bytes.max(1)).clamp(1, 10000);
    for sample in 0..9 {
        for arm in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
            let start = Instant::now();
            for _ in 0..iterations {
                assert_eq!(
                    black_box(if arm == 0 { software() } else { native() }),
                    expected
                );
            }
            let ns = start.elapsed().as_nanos() as f64 / iterations as f64;
            let name = if arm == 0 { "software" } else { "native" };
            println!(
                "{{\"kind\":\"{kind}\",\"bytes\":{bytes},\"arm\":\"{name}\",\"sample\":{sample},\"iterations\":{iterations},\"ns\":{ns}}}"
            );
        }
    }
}

fn main() {
    let check = std::env::args().any(|arg| arg == "--check");
    let sizes: Vec<usize> = if check {
        vec![0, 1, 55, 56, 63, 64, 65, 4096]
    } else {
        vec![
            0,
            1,
            55,
            56,
            63,
            64,
            65,
            4096,
            1024 * 1024,
            32 * 1024 * 1024,
        ]
    };
    for len in sizes {
        let data: Vec<u8> = (0..len).map(|i| (i.wrapping_mul(37) >> 5) as u8).collect();
        compare(
            || searcher::search::campaign::postcard_value_sha256(black_box(&data)).unwrap(),
            || {
                searcher_reference::search::campaign::postcard_value_sha256(black_box(&data))
                    .unwrap()
            },
            "campaign_postcard_digest",
            len,
            check,
        );
    }
    compare(
        || searcher::search::campaign::derive_selection_seed(black_box(0x1234)),
        || searcher_reference::search::campaign::derive_selection_seed(black_box(0x1234)),
        "selection_seed",
        12,
        check,
    );
    let mut native = unison::toy::ToyMachine::new(
        vec![
            unison::toy::asm::loadi(0, 42),
            unison::toy::asm::store(0, 1),
        ],
        0x1234,
    );
    let mut software = unison_reference::toy::ToyMachine::new(
        vec![
            unison_reference::toy::asm::loadi(0, 42),
            unison_reference::toy::asm::store(0, 1),
        ],
        0x1234,
    );
    unison::Subject::run_to(&mut native, 2).unwrap();
    unison_reference::Subject::run_to(&mut software, 2).unwrap();
    compare(
        || unison::Subject::state_hash(black_box(&native)).unwrap(),
        || unison_reference::Subject::state_hash(black_box(&software)).unwrap(),
        "unison_state_hash",
        unison::toy::MEM_SIZE,
        check,
    );
    println!("Independent host SHA consumers agree with software controls");
}
