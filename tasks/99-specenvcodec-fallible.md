# Task 99 — Make SpecEnvCodec fallible on malformed public reproducer blobs

Bead: `hm-5d9` (P1, bug). Filed 2026-07-11 from a quality review.

## Problem

`dissonance/explorer`'s `SpecEnvCodec` currently documents and performs **panics** when
public serialized reproducer bytes are malformed or when composition rejects them. That
violates the repository rule that library code never panics on untrusted input: a
reproducer blob is exactly the artifact users will pass around, load from disk, and feed
back in — it is untrusted by definition.

## Deliverable

Make the EnvCodec boundary fallible (or introduce a structurally trusted decoded type), and
propagate typed failures through Explorer and campaign callers **without** turning them
into guest bug outcomes.

1. Malformed, truncated, wrong-version, overflow, and unsupported-composition inputs return
   **typed errors** — never panic, never abort.
2. Explorer and campaign callers surface these as loud **control errors** (the run/campaign
   fails with a decode error), never as findings/crash outcomes attributed to the guest.
3. Property tests exercise hostile blobs (fuzz-shaped: truncations at every boundary, bit
   flips in headers, version skew, length-field overflow, unknown composition tags).
4. Public API snapshots updated (`quality-d-public-api` discipline) and the crate's
   implementation documentation updated to describe the fallible contract.

## Gates

- Standard portable gates: build, nextest, clippy, fmt, deny.
- New property tests green; a regression test for each named malformation class.
- Public-api snapshot job green after the intentional API change.
- No remaining `panic!`/`unwrap`/`expect` on the public decode path (grep-provable; internal
  invariants on *already-validated* data may keep debug assertions).

## Notes

- Determinism caution: the decode path feeds `Environment` composition — do not change the
  semantics of *valid* blobs; byte-for-byte identical valid inputs must decode to identical
  environments (existing round-trip tests must stay green untouched).
- Wire-format changes are OUT of scope; this is error-path plumbing, not a codec redesign.
- Close `hm-5d9` on completion.
