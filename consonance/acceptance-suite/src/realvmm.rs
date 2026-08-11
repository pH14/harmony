// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **real-VMM registry** — the `acceptance-suite` binary's hardware
//! composition root (`docs/TESTING.md`, rung 5).
//!
//! Where the toy registry maps a cell's `source` to a generated `unison`
//! program, this maps it to the item's **built payload**, booted on the patched
//! backend through `vmm_core::vendor::x86::bringup::boot_patched_corpus`. It is
//! the one place this binary names a concrete `(Backend impl, Arch vendor)`
//! pair, mirroring `dissonance/campaign-runner/src/boxrun.rs`.
//!
//! Everything above stays substrate-free: `acceptance_suite::run_item` is
//! generic over [`unison::SubjectFactory`] and never learns which registry made
//! one. A hardware cell and a portable cell differ **only** in that choice.
//!
//! Linux/x86-64 only, behind the `real-vmm` feature — gated on the **arch** as
//! well as the OS (AGENTS.md, cross-arch discipline), because
//! `boot_patched_corpus` names the x86 vendor's patched backend. Every function
//! here needs a real `/dev/kvm`, the loaded patched KVM modules, and built
//! payloads, so no portable test can drive it; its evidence is the hardware lane
//! (`scripts/box-gates.sh`), exactly like `boxrun.rs`.

use std::path::PathBuf;

use acceptance_suite::{CorpusItem, ItemReport, RunConfig, run_item};
use unison::SubjectFactory;
use vmm_backend::{Backend, X86};
use vmm_core::corpus::CorpusMachine;
use vmm_core::vendor::x86::bringup::boot_patched_corpus;

/// 256 MiB of guest RAM — the size the C1 payloads were validated under, and
/// what `consonance/vmm-core/tests/box_corpus.rs` boots them with. Kept
/// identical so the two entry points compare the same quantity.
const GUEST_RAM_LEN: usize = 256 << 20;

/// The repo root, relative to this crate's manifest directory.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// The built payload ELF for a corpus item, at the path
/// `consonance/acceptance-suite/payloads` builds it to.
fn payload_path(name: &str) -> PathBuf {
    repo_root()
        .join("consonance/acceptance-suite/payloads/target/x86_64-unknown-none/release")
        .join(name)
}

/// A [`SubjectFactory`] over one built payload on the patched backend.
///
/// `spawn` is infallible by the trait's shape (bisection re-executes from
/// scratch many times, so it cannot be a `Result`), and a composition failure
/// here means the host baseline is wrong — no `/dev/kvm`, no patched modules, a
/// payload that will not load. That is exactly the case the standing rule says
/// must **fail loudly rather than skip silently** (`docs/TESTING.md`), so it
/// panics with what is missing.
struct PayloadFactory {
    name: String,
    payload: Vec<u8>,
}

impl SubjectFactory for PayloadFactory {
    type M = CorpusMachine<Box<dyn Backend<A = X86>>>;

    fn spawn(&self, seed: u64) -> Self::M {
        boot_patched_corpus(&self.payload, GUEST_RAM_LEN, seed).unwrap_or_else(|e| {
            panic!(
                "payload {:?} failed to boot on the patched backend ({e}) — this host is not a \
                 determinism substrate: it needs /dev/kvm, the LOADED patched KVM modules \
                 (KVM_CAP_X86_DETERMINISTIC_INTERCEPTS), and perf_event",
                self.name
            )
        })
    }
}

/// Run one hardware cell: load its built payload, boot it on the patched
/// backend, and drive the item's declared oracles over it.
///
/// The oracles are the same functions the portable cells run. Only the factory
/// differs.
pub fn run_cell<G: Fn(&str) -> Option<String> + Copy>(
    item: &CorpusItem,
    cfg: &RunConfig,
    read_golden: G,
) -> Result<ItemReport, Box<dyn std::error::Error>> {
    let path = payload_path(&item.name);
    let payload = std::fs::read(&path).map_err(|e| {
        format!(
            "payload for cell {:?} not built ({e}) at {}: build it on the box first with \
             `cd consonance/acceptance-suite/payloads && cargo build --release` \
             (target x86_64-unknown-none)",
            item.name,
            path.display()
        )
    })?;
    let factory = PayloadFactory {
        name: item.name.clone(),
        payload,
    };
    Ok(run_item(item, &factory, cfg, read_golden)?)
}
