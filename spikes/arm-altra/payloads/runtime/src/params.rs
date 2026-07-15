// SPDX-License-Identifier: AGPL-3.0-or-later
//! The params page: how the harness tells the guest which scale and seed to run.
//!
//! # The unmanaged fallback, and why it attests
//!
//! Under the KVM harness the page is written before the vCPU runs, so the payload
//! reads a real scale and seed. Under QEMU/TCG nobody writes it, and the page
//! reads as zeroed RAM. Rather than fault, the payload falls back to the smoke
//! scale and the default seed — but it **reports which mode it is in**
//! (`PARAMS mode=managed` vs `mode=self-seeded`) as a protocol line.
//!
//! That report is not decoration. `docs/ARM-ALTRA.md` §Evidence integrity #4
//! requires every stage to prove *in-band* that the claimed mechanism was the one
//! exercised. A harness bug that forgot to publish the page would otherwise
//! silently produce a smoke-scale run that looks like a 1e8 run in the summary —
//! precisely the class of failure the PR-98 review found. With the mode on the
//! wire, the harness asserts `managed` and the TCG golden asserts `self-seeded`,
//! so neither can masquerade as the other.
//!
//! An out-of-range scale index also lands on [`Scale::Smoke`]
//! ([`oracle_model::Scale::from_index`]): a corrupt page must never select a
//! 1e8 run.

use oracle_model::{DEFAULT_SEED, PARAMS_ABI, PARAMS_GPA, PARAMS_MAGIC, Scale};

/// The page's on-wire layout. Little-endian, matching the codebase's wire
/// discipline.
#[repr(C)]
struct ParamsPage {
    /// [`oracle_model::PARAMS_MAGIC`].
    magic: u32,
    /// [`oracle_model::PARAMS_ABI`].
    abi: u32,
    /// A [`Scale`] index.
    scale_index: u32,
    /// Reserved, zero.
    _reserved: u32,
    /// The PRNG seed for `branch-dense`.
    seed: u64,
}

/// Where the parameters came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// The harness published a valid page.
    Managed,
    /// No valid page: the payload used its own defaults (the TCG case).
    SelfSeeded,
}

impl Mode {
    /// The token this mode prints as, on the `PARAMS` protocol line.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Mode::Managed => "managed",
            Mode::SelfSeeded => "self-seeded",
        }
    }
}

/// The run parameters.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// The scale to run.
    pub scale: Scale,
    /// The PRNG seed.
    pub seed: u64,
    /// Whether the harness published them.
    pub mode: Mode,
}

/// Read the params page, falling back to smoke defaults if it is absent.
#[must_use]
pub fn load() -> Params {
    let page = PARAMS_GPA as *const ParamsPage;

    // SAFETY: PARAMS_GPA is the first page of guest RAM, mapped Normal by the
    // boot shim's L1[1] block and deliberately left uncovered by any output
    // section of `linker.ld` (the image starts 512 KiB above it). Reads are
    // volatile because the harness — not this code — is the writer.
    unsafe {
        let magic = core::ptr::read_volatile(&raw const (*page).magic);
        let abi = core::ptr::read_volatile(&raw const (*page).abi);

        if magic == PARAMS_MAGIC && abi == PARAMS_ABI {
            Params {
                scale: Scale::from_index(core::ptr::read_volatile(&raw const (*page).scale_index)),
                seed: core::ptr::read_volatile(&raw const (*page).seed),
                mode: Mode::Managed,
            }
        } else {
            Params {
                scale: Scale::Smoke,
                seed: DEFAULT_SEED,
                mode: Mode::SelfSeeded,
            }
        }
    }
}
