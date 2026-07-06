// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`MomentRef`] — the copyable coordinate, its versioned textual encoding, and
//! the pure [`vary`](MomentRef::vary) counterfactual.
//!
//! A `MomentRef` is the universal handle every strong record/replay system
//! converges on (`docs/RESOLUTION.md` §"The moment address"): a
//! genesis-complete reproducer ([`EnvSpec`]) plus an absolute [`Moment`] on the
//! deterministic axis. Because the substrate is deterministic and every
//! reproducer is genesis-complete, the pair is **self-contained** — anyone, on
//! any box, can re-reach exactly that instant. The whole point is that it is an
//! *artifact*: a short line a user copies out of a finding, a log, or a
//! transcript and pastes into a session. So it has a **textual** encoding
//! (`Display` + [`parse`](MomentRef::parse)) that round-trips and never panics
//! on hostile input.

use std::fmt;
use std::str::FromStr;

use environment::{Action, EnvSpec, Moment};
use thiserror::Error;

/// The moment address: a genesis-complete reproducer and an absolute
/// [`Moment`]. Copyable, versioned, self-contained (`docs/RESOLUTION.md`).
///
/// > **On the `env` field's type.** The design doc writes the field as
/// > `env: Environment`, but `environment::Environment` is the *decide-seam
/// > trait*, not a data type; the reproducer it names is the concrete
/// > genesis-complete [`EnvSpec`] (the same value `compose` mints for a
/// > `Bug.env`). This crate uses `EnvSpec` — the one type that is both the
/// > reproducer *and* the value `branch` reseeds with.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MomentRef {
    /// The genesis-complete reproducer this moment lives in.
    pub env: EnvSpec,
    /// The absolute position on the deterministic axis (a retired-instruction
    /// count — `environment::Moment`).
    pub moment: Moment,
}

/// One edit applied to a copy of a [`MomentRef`]'s `env` by
/// [`vary`](MomentRef::vary): the whole counterfactual (replay-with-one-change)
/// vocabulary. Exactly one override key is touched.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum OverrideEdit {
    /// Insert or replace the [`Action`] at this [`Moment`] (covers *add* and
    /// *change*).
    Set {
        /// The override key to write.
        at: Moment,
        /// The action to place there.
        action: Action,
    },
    /// Remove any [`Action`] at this [`Moment`] (a no-op if none is present, in
    /// which case the env is byte-identical).
    Remove {
        /// The override key to clear.
        at: Moment,
    },
}

impl MomentRef {
    /// The textual-encoding version tag. Bumps if the on-the-wire text form
    /// (below) ever changes, so an old paste is rejected loudly rather than
    /// mis-parsed.
    pub const TEXT_VERSION: u16 = 1;

    /// The fixed prefix of the textual encoding.
    const PREFIX: &'static str = "mref1";

    /// Build a `MomentRef` from its parts.
    pub fn new(env: EnvSpec, moment: Moment) -> Self {
        Self { env, moment }
    }

    /// **Counterfactual (replay-with-one-change).** Return a new `MomentRef`
    /// whose `env` is a copy of `self.env` with exactly one override
    /// [`edit`](OverrideEdit) applied, at the *same* [`Moment`] — materializing
    /// the result is the counterfactual run. A **pure** function: `self` is
    /// untouched, and the native `BTreeMap<Moment, Action>` data model does all
    /// the work (no re-encode/parse round-trip). Everything but the one edited
    /// key is preserved byte-for-byte (seed, policy, standing faults, reseed
    /// markers, and every other override).
    #[must_use]
    pub fn vary(&self, edit: &OverrideEdit) -> MomentRef {
        let mut env = self.env.clone();
        match edit {
            OverrideEdit::Set { at, action } => {
                // `record` promotes a `Seeded` spec to `Recorded` and does a
                // last-write-wins insert at exactly `at` — the one-key edit.
                env.record(*at, action.clone());
            }
            OverrideEdit::Remove { at } => {
                remove_override(&mut env, *at);
            }
        }
        MomentRef {
            env,
            moment: self.moment,
        }
    }

    /// Parse the textual encoding produced by [`Display`](fmt::Display). Total:
    /// arbitrary, truncated, or mutated input yields an
    /// [`MRefParseError`](MRefParseError), never a panic (the artifact is copied
    /// from untrusted logs — conventions rule 4).
    ///
    /// Form: `mref1:<moment-decimal>:<lower-hex of EnvSpec::encode()>`.
    pub fn parse(s: &str) -> Result<MomentRef, MRefParseError> {
        // Exactly three colon-separated fields; the hex field never contains a
        // colon, so a plain 3-way split is unambiguous.
        let mut parts = s.split(':');
        let tag = parts.next().ok_or(MRefParseError::Empty)?;
        if tag != Self::PREFIX {
            return Err(MRefParseError::BadPrefix);
        }
        let moment_str = parts.next().ok_or(MRefParseError::Truncated)?;
        let env_hex = parts.next().ok_or(MRefParseError::Truncated)?;
        if parts.next().is_some() {
            return Err(MRefParseError::Trailing);
        }
        let moment: Moment = moment_str.parse().map_err(|_| MRefParseError::BadMoment)?;
        let env_bytes = crate::from_hex(env_hex).ok_or(MRefParseError::BadEnvHex)?;
        let env = EnvSpec::decode(&env_bytes).map_err(|_| MRefParseError::BadEnv)?;
        Ok(MomentRef { env, moment })
    }
}

/// Remove the override at `at` from an [`EnvSpec`] without disturbing anything
/// else. A `Seeded` spec (no overrides) is left untouched — so a `Remove` of an
/// absent key leaves the env byte-identical, which the `vary` minimality
/// property relies on. A `Recorded` spec whose last override this removes stays
/// `Recorded` (empty map) — deliberately *not* demoted to `Seeded`, so the edit
/// touches only the override map and nothing else.
fn remove_override(env: &mut EnvSpec, at: Moment) {
    if let EnvSpec::Recorded { overrides, .. } = env {
        overrides.remove(&at);
    }
}

impl fmt::Display for MomentRef {
    /// `mref1:<moment>:<hex(EnvSpec::encode())>` — one line, no spaces, safe to
    /// copy/paste. The `EnvSpec` blob is canonical (its own encoder emits bytes
    /// in `Moment` order), so equal `MomentRef`s always render to the identical
    /// string.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            Self::PREFIX,
            self.moment,
            crate::to_hex(&self.env.encode())
        )
    }
}

impl FromStr for MomentRef {
    type Err = MRefParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        MomentRef::parse(s)
    }
}

/// Why a textual [`MomentRef`] failed to [`parse`](MomentRef::parse). Every
/// variant is a total, panic-free rejection of untrusted input.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Error)]
pub enum MRefParseError {
    /// The input was empty.
    #[error("empty moment reference")]
    Empty,
    /// The `mref1` version/prefix tag was missing or wrong.
    #[error("not a versioned moment reference (expected the `mref1` prefix)")]
    BadPrefix,
    /// A required `:`-separated field was missing.
    #[error("truncated moment reference (missing a field)")]
    Truncated,
    /// Extra `:`-separated fields followed the encoding.
    #[error("trailing data after the moment reference")]
    Trailing,
    /// The moment field was not a decimal `u64`.
    #[error("malformed moment (expected a decimal count)")]
    BadMoment,
    /// The env field was not valid lower-case hex.
    #[error("malformed env encoding (expected lower-case hex)")]
    BadEnvHex,
    /// The env bytes decoded from hex were not a valid `EnvSpec`.
    #[error("env bytes are not a valid reproducer")]
    BadEnv,
}

#[cfg(test)]
mod tests {
    use super::*;
    use environment::{EnvCodec, FaultPolicy, HostFault, VTime};

    fn seeded(seed: u64) -> EnvSpec {
        EnvCodec::seeded(seed, FaultPolicy::none())
    }

    #[test]
    fn round_trips_a_seeded_env() {
        let m = MomentRef::new(seeded(0xDEAD_BEEF), 4242);
        let text = m.to_string();
        assert!(text.starts_with("mref1:4242:"));
        assert_eq!(MomentRef::parse(&text).unwrap(), m);
    }

    #[test]
    fn round_trips_a_recorded_env_with_overrides() {
        let mut env = seeded(7);
        env.perturb(HostFault::SkewTime(VTime(9)), 100);
        env.perturb(HostFault::InjectInterrupt { vector: 200 }, 250);
        let m = MomentRef::new(env, u64::MAX);
        assert_eq!(MomentRef::parse(&m.to_string()).unwrap(), m);
    }

    #[test]
    fn parse_rejects_garbage_without_panicking() {
        for bad in [
            "",
            "mref1",
            "mref1:",
            "mref2:1:00",
            "mref1:notanumber:00",
            "mref1:1:zz",
            "mref1:1:0", // odd hex length
            "mref1:1:00:extra",
            "mref1:1:00",                          // valid hex, invalid EnvSpec blob
            "mref1:99999999999999999999999999:00", // moment overflows u64
        ] {
            assert!(
                MomentRef::parse(bad).is_err(),
                "expected parse error for {bad:?}"
            );
        }
    }

    #[test]
    fn vary_set_touches_exactly_one_key() {
        let base = MomentRef::new(seeded(1), 50);
        let varied = base.vary(&OverrideEdit::Set {
            at: 10,
            action: Action::Host(HostFault::InjectInterrupt { vector: 32 }),
        });
        // Pure: base untouched.
        assert_eq!(base.env, seeded(1));
        // Same moment, one override added.
        assert_eq!(varied.moment, 50);
        assert_eq!(varied.env.overrides().len(), 1);
        assert!(varied.env.overrides().contains_key(&10));
    }

    #[test]
    fn vary_remove_of_absent_key_is_byte_identical() {
        let base = MomentRef::new(seeded(1), 50);
        let varied = base.vary(&OverrideEdit::Remove { at: 999 });
        assert_eq!(base.env.encode(), varied.env.encode());
    }
}
