// SPDX-License-Identifier: AGPL-3.0-or-later
//! SPIKE(nested-x86): N-3 repeat gate — **spike-branch-only apparatus**, not
//! production surface (see `docs/NESTED-X86.md` §N-3).
//!
//! Runs ONE corpus payload to terminal at ONE seed, N times, each on a fresh
//! patched-backend VM, and requires every repetition's `state_hash` **and**
//! `observable_digest` to be bit-identical to repetition 0's. Emits
//! machine-readable `N3JSON` lines (start / progress / summary / mismatch) so
//! the harness never hand-copies hashes. The reference hashes in the summary are
//! the cross-substrate comparison artifact (nested vs bare metal).
//!
//! Env parameters (all optional):
//!   N3_REPS   repetitions (default 1000)
//!   N3_ITEM   payload name (default insn-rng — seed-consuming, most sensitive)
//!   N3_SEED   run seed (default the pinned corpus seed 0x0028_C0FF_EE5E_EDC0)
//!   N3_PROGRESS progress line cadence (default 100)
#![cfg(target_os = "linux")]

use unison::{Machine, RunOutcome};
use vmm_core::corpus::boot_patched_payload;

const GUEST_RAM_LEN: usize = 256 << 20;
const LIMIT: u64 = 1_000_000;
const CORPUS_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn hex32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
#[ignore = "SPIKE(nested-x86) live repeat gate; run via the N-3 appliance harness"]
fn n3_repeat_gate() {
    let reps = env_u64("N3_REPS", 1000);
    let seed = env_u64("N3_SEED", CORPUS_SEED);
    let progress_every = env_u64("N3_PROGRESS", 100);
    let item = std::env::var("N3_ITEM").unwrap_or_else(|_| "insn-rng".to_string());

    let payload_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../guest/payloads/target/x86_64-unknown-none/release")
        .join(&item);
    let payload = std::fs::read(&payload_path).unwrap_or_else(|e| {
        panic!(
            "payload `{item}` not built ({e}) at {} — `cd guest/payloads && cargo build --release`",
            payload_path.display()
        )
    });

    println!("N3JSON {{\"event\":\"start\",\"item\":\"{item}\",\"reps\":{reps},\"seed\":{seed}}}");

    let mut reference: Option<(String, String)> = None;
    let mut attempted = 0u64;
    let mut identical = 0u64;
    let mut mismatches: Vec<String> = Vec::new();

    for rep in 0..reps {
        attempted += 1;
        let mut m = boot_patched_payload(&payload, GUEST_RAM_LEN, seed)
            .unwrap_or_else(|e| panic!("boot_patched_payload({item}) failed at rep {rep}: {e}"));
        let outcome = m.run_to(LIMIT).expect("run_to is infallible");
        let sh = hex32(&m.state_hash());
        let od = hex32(&m.observable_digest());
        if outcome != RunOutcome::Halted {
            mismatches.push(format!("rep={rep} outcome={outcome:?}"));
        } else {
            match &reference {
                None => {
                    reference = Some((sh.clone(), od.clone()));
                    identical += 1;
                }
                Some((rsh, rod)) if *rsh == sh && *rod == od => identical += 1,
                Some((rsh, rod)) => mismatches.push(format!(
                    "rep={rep} state_hash={sh} (ref {rsh}) observable_digest={od} (ref {rod})"
                )),
            }
        }
        if mismatches.len() > 8 {
            break; // enough to diagnose; account the abort in the summary
        }
        if progress_every != 0 && (rep + 1) % progress_every == 0 {
            println!(
                "N3JSON {{\"event\":\"progress\",\"attempted\":{attempted},\"identical\":{identical},\"mismatches\":{}}}",
                mismatches.len()
            );
        }
    }

    let (rsh, rod) = reference.unwrap_or_default();
    println!(
        "N3JSON {{\"event\":\"summary\",\"item\":\"{item}\",\"attempted\":{attempted},\"identical\":{identical},\"mismatches\":{},\"state_hash\":\"{rsh}\",\"observable_digest\":\"{rod}\"}}",
        mismatches.len()
    );
    for m in &mismatches {
        println!("N3JSON {{\"event\":\"mismatch\",\"detail\":\"{m}\"}}");
    }
    assert!(
        mismatches.is_empty() && identical == attempted,
        "N-3 repeat gate: {} mismatches over {} attempts — any silent divergence is NO-GO \
         (docs/NESTED-X86.md §N-3)",
        mismatches.len(),
        attempted
    );
}
