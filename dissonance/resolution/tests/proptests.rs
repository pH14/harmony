// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 2 — the property tests (≥256 cases each):
//!
//! 1. [`MomentRef`] display/parse round-trips, and parsing never panics on
//!    adversarial input.
//! 2. `vary` is pure and minimal — edits exactly one override key, env otherwise
//!    unchanged.
//! 3. Transcript replay renders byte-identically to the live rendering for
//!    arbitrary scripted sessions (the one-renderer principle).

use environment::{Action, BitMask, EnvCodec, EnvSpec, FaultPolicy, HostFault, Ratio, Span};
use proptest::prelude::*;
use resolution::{
    Command, MockServer, MomentRef, OverrideEdit, Session, Shell, from_jsonl, render_transcript,
    to_jsonl,
};

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// An arbitrary host-plane [`Action`] — every `HostFault` shape.
fn host_action() -> impl Strategy<Value = Action> {
    prop_oneof![
        any::<u64>().prop_map(|v| Action::Host(HostFault::SkewTime(Span(v)))),
        any::<u8>().prop_map(|vector| Action::Host(HostFault::InjectInterrupt { vector })),
        (any::<u64>(), any::<u64>()).prop_map(|(gpa, m)| Action::Host(HostFault::CorruptMemory {
            gpa,
            mask: BitMask(m)
        })),
        (1u64..=u64::MAX, 1u64..=u64::MAX).prop_map(|(n, d)| {
            // `d >= 1`, so `Ratio::new` never returns `None`.
            Action::Host(HostFault::SetClockRate(Ratio::new(n, d).unwrap()))
        }),
    ]
}

/// An arbitrary genesis-complete [`EnvSpec`] — seeded, or recorded with a few
/// host overrides.
fn env_strategy() -> impl Strategy<Value = EnvSpec> {
    let seeded = any::<u64>().prop_map(|s| EnvCodec::seeded(s, FaultPolicy::none()));
    let recorded = (
        any::<u64>(),
        prop::collection::vec((any::<u64>(), host_action()), 0..5),
    )
        .prop_map(|(seed, entries)| {
            let mut env = EnvCodec::seeded(seed, FaultPolicy::none());
            for (at, action) in entries {
                env.record(at, action);
            }
            env
        });
    prop_oneof![seeded, recorded]
}

/// An arbitrary [`MomentRef`].
fn mref_strategy() -> impl Strategy<Value = MomentRef> {
    (env_strategy(), any::<u64>()).prop_map(|(env, moment)| MomentRef::new(env, moment))
}

/// An arbitrary [`OverrideEdit`].
fn edit_strategy() -> impl Strategy<Value = OverrideEdit> {
    prop_oneof![
        (any::<u64>(), host_action()).prop_map(|(at, action)| OverrideEdit::Set { at, action }),
        any::<u64>().prop_map(|at| OverrideEdit::Remove { at }),
    ]
}

/// A non-`open` REPL command.
fn command_strategy() -> impl Strategy<Value = Command> {
    prop_oneof![
        Just(Command::Regs),
        Just(Command::Hash),
        // `transcript` is a recorded command now, so it's in the byte-identity
        // sweep (an earlier revision had to exclude it — a non-recorded view).
        Just(Command::Transcript),
        (0u64..200_000).prop_map(|until| Command::Run { until }),
        (any::<u64>(), 0u32..256).prop_map(|(gpa, len)| Command::Read { gpa, len }),
        "[a-z ]{0,8}".prop_map(Command::Exec),
        edit_strategy().prop_map(Command::Vary),
    ]
}

/// A scripted session: an `open` followed by arbitrary commands.
fn script_strategy() -> impl Strategy<Value = Vec<Command>> {
    (
        mref_strategy(),
        prop::collection::vec(command_strategy(), 0..12),
    )
        .prop_map(|(m, rest)| {
            let mut cmds = vec![Command::Open(m)];
            cmds.extend(rest);
            cmds
        })
}

/// A fresh connected session over a mock.
fn connect() -> Session<MockServer> {
    Session::connect(MockServer::boot(EnvCodec::seeded(123, FaultPolicy::none()))).unwrap()
}

/// Whether two envs are identical except (at most) the override at `at` — the
/// logical minimality of `vary`. Compares seed, policy, reseed markers, and the
/// override map with key `at` erased. (Standing faults are copied verbatim by
/// `vary` and have no public accessor; `vary` structurally cannot touch them.)
fn same_except_key(a: &EnvSpec, b: &EnvSpec, at: u64) -> bool {
    if a.seed() != b.seed() || a.policy().to_bytes() != b.policy().to_bytes() {
        return false;
    }
    if a.reseeds() != b.reseeds() {
        return false;
    }
    let mut ao = a.overrides().clone();
    let mut bo = b.overrides().clone();
    ao.remove(&at);
    bo.remove(&at);
    ao == bo
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Display then parse is the identity.
    #[test]
    fn mref_round_trips(m in mref_strategy()) {
        let parsed = MomentRef::parse(&m.to_string()).unwrap();
        prop_assert_eq!(parsed, m);
    }

    /// Parsing arbitrary strings never panics.
    #[test]
    fn mref_parse_never_panics(s in ".*") {
        let _ = MomentRef::parse(&s);
    }

    /// Parsing structured-but-hostile strings (right prefix, garbage fields)
    /// never panics.
    #[test]
    fn mref_parse_never_panics_on_structured_garbage(moment in ".*", hex in ".*") {
        let _ = MomentRef::parse(&format!("mref1:{moment}:{hex}"));
    }

    /// `vary` is pure (input untouched, deterministic) and minimal (one key).
    #[test]
    fn vary_is_pure_and_minimal(m in mref_strategy(), edit in edit_strategy()) {
        let before = m.clone();
        let varied = m.vary(&edit);
        // Pure: the input is untouched, and `vary` is deterministic.
        prop_assert_eq!(&m, &before);
        prop_assert_eq!(m.vary(&edit), varied.clone());
        // Same moment.
        prop_assert_eq!(varied.moment, m.moment);
        // Minimal: identical except the one edited key.
        let at = match &edit {
            OverrideEdit::Set { at, .. } | OverrideEdit::Remove { at } => *at,
        };
        prop_assert!(same_except_key(&m.env, &varied.env, at));
    }

    /// On an already-`Recorded` base (no `Seeded`→`Recorded` promotion), `vary`
    /// is byte-identical except the one key: erasing that key from both yields
    /// identical blobs.
    #[test]
    fn vary_on_recorded_base_is_byte_minimal(
        seed in any::<u64>(),
        overrides in prop::collection::vec((any::<u64>(), host_action()), 1..5),
        moment in any::<u64>(),
        edit in edit_strategy(),
    ) {
        let mut env = EnvCodec::seeded(seed, FaultPolicy::none());
        for (at, action) in overrides {
            env.record(at, action);
        }
        let m = MomentRef::new(env, moment);
        let varied = m.vary(&edit);
        let at = match &edit {
            OverrideEdit::Set { at, .. } | OverrideEdit::Remove { at } => *at,
        };
        let erase = OverrideEdit::Remove { at };
        prop_assert_eq!(
            m.vary(&erase).env.encode(),
            varied.vary(&erase).env.encode()
        );
    }

    /// `vary`-Set writes exactly the targeted key.
    #[test]
    fn vary_set_writes_exactly_that_key(
        m in mref_strategy(),
        at in any::<u64>(),
        action in host_action(),
    ) {
        let varied = m.vary(&OverrideEdit::Set { at, action: action.clone() });
        prop_assert_eq!(varied.env.overrides().get(&at), Some(&action));
    }

    /// Transcript replay renders byte-identically to the live rendering.
    #[test]
    fn transcript_replay_is_byte_identical(script in script_strategy()) {
        let mut shell = Shell::new(connect());
        for cmd in script {
            let _ = shell.dispatch(cmd);
        }
        let live = render_transcript(shell.records());

        let jsonl = to_jsonl(shell.records()).unwrap();
        let parsed = from_jsonl(&jsonl).unwrap();
        // Lossless: the JSONL round-trips the records exactly.
        prop_assert_eq!(parsed.as_slice(), shell.records());
        // One renderer: replay == live.
        prop_assert_eq!(render_transcript(&parsed), live);
    }
}
