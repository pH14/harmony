// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use std::hint::black_box;
use std::time::Instant;

#[test]
#[ignore = "live HVF comparison; run release on Apple silicon with --ignored --nocapture"]
fn irq_mask_polling_matches_full_capture() {
    let mut guest = Guest::new(&mixed_work());
    let initial_ram = guest.mem.as_mut_slice().to_vec();
    for include_guest in [false, true] {
        for pair in 0..9 {
            let order = if pair % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            };
            let mut reference = None;
            for narrow in order {
                guest.mem.as_mut_slice().copy_from_slice(&initial_ram);
                guest
                    .backend
                    .invalidate_instruction_cache(guest.mem.ptr as usize, RAM_BYTES);
                guest.restart();
                guest.backend.reset_exit_counts();
                let mut stores = Vec::with_capacity(if include_guest { 20_000 } else { 0 });
                #[allow(clippy::disallowed_methods)]
                let start = Instant::now();
                for _ in 0..20_000 {
                    if include_guest {
                        stores.push(guest.next_store());
                    }
                    let mask = if narrow {
                        guest.backend.read_irq_mask().unwrap().unwrap()
                    } else {
                        guest.backend.save().unwrap().core.pstate & (1 << 7) != 0
                    };
                    assert!(black_box(mask));
                }
                let ns = start.elapsed().as_nanos();
                let mut state = guest.save();
                state.vtimer.counter = 0;
                let endpoint = (
                    stores,
                    state,
                    guest.mem.as_mut_slice().to_vec(),
                    guest.backend.exit_counts(),
                );
                if let Some(expected) = &reference {
                    assert_eq!(&endpoint, expected);
                } else {
                    reference = Some(endpoint);
                }
                println!(
                    "irq-poll guest={include_guest} pair={pair} narrow={narrow} iterations=20000 ns={ns}"
                );
            }
        }
    }
}
