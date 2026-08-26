# SPDX-License-Identifier: AGPL-3.0-or-later

# Machine implementation notes

## PR #193 boundary

The `Machine` trait is the searcher's deterministic target boundary. Its core
verbs mirror consonance control-proto: snapshot, drop, branch, replay, run, and
read. The local types keep the searcher independent of transport details; the
socket adapter converts them to the authoritative control-proto types only when
framing a request or decoding a reply.

The current PR #193 `machine` and `searcher` crates were added beside the existing
LibAFL `fuzzer` member. No existing member was removed or rewritten. This is the
phase-5 dependency boundary: `machine` may now depend on the consonance
`environment` and `control-proto` crates by pinned path/version.

## One reproducer for both emulator builds

`nes::reproducer` no longer invents a private `(buttons, hold)` pair blob. It
encodes the canonical environment version-7 `EnvSpec` with an offered payload
tape, where every entry is exactly `[buttons, bounded_hold_frames]`.
`NesMachine::branch` decodes those same bytes and rejects a foreign version,
malformed entry, or unrelated environment mechanism. The consonance guest path
therefore receives byte-for-byte the artifact the in-process TetaNES path consumes.

## SocketMachine

`SocketMachine<S>` is synchronous and request/reply exact:

- hello requires the current application protocol, environment version 7,
  zero-width coverage, and the cooperating-SDK flag;
- every request has a checked monotonic sequence and accepts only the matching
  reply; partial frames are bounded and EOF is loud;
- control failures remain failures rather than guest stop reasons;
- the atomic snapshot cut (`at`, SDK prefix length, taint) is retained beside its
  handle;
- whole-state hash and paged SDK events are available for M2's causal/differential
  oracles;
- successful restores from an explicitly marked gameplay genesis are counted
  separately from continuation restores.

The unit transport test pins every core verb and rejects a wrong sequence. The
integration test drives the adapter over a real Unix socket into a real
`ControlServer<MockBackend>` and proves environment branch, snapshot cut, read,
hash, replay, drop, and both restore-counter classes.
