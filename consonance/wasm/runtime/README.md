<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Pinned interpreter source

This package compiles the checksummed Wasmi 0.46.0 source archive with the
reviewed continuation patch in `../qualification/wasmi-snapshot.patch`.
Python 3 and `patch` are build prerequisites on Linux and macOS. Preparation
runs only inside Cargo's output directory, with no network access or source
checkout mutation. The original upstream licenses accompany the archive.

The root include removes upstream crate-level documentation and attributes;
the wrapper retains `no_std` and the original recursion limit. It otherwise
compiles the upstream source and patch verbatim. Runtime and patch digests
belong to execution identity. New runtime versions require a new identity,
conformance checks, Miri, and host transfer qualification.

`numerical.patch` prevents NaN f64 constants from being compressed through a
host f32 conversion in the translator's copy, return and branch-table paths.
The original lossless conversion check cannot guarantee that a later conversion
retains the NaN's sign or payload. Full-width constant registers retain the
specified bits. This patch is separately digested in execution identity.

The numerical adapter canonicalizes floating-point arithmetic in both eager
constant evaluation and execution before writing result registers. This prevents
a fuel stop at entry to a guest canonicalization helper from retaining a host's
intermediate NaN bits. Bitwise sign operations and literal NaN payloads retain
their specified bits. NaN-propagation shortcuts that bypass arithmetic are removed.

`import-completion.patch` validates the single i32 result and writes it through
a checked value-stack index. It converts a pending import to a ready continuation
without executing guest instructions or changing fuel. The session then commits
its already-validated host transaction at the same boundary. This prevents a
checkpoint from retaining an applied host effect with an unapplied return value.
The additional patch is included in execution identity.

`validation.patch` compares continuation allocations with the eagerly compiled
functions. Frames must be contiguous after the root result buffer, with the
exact constant prefix and writable register count. Constant bits are immutable,
return destinations stay within the parent's writable registers, and a pending
result names one writable i32 slot. Compiled instruction positions must identify
executable words rather than register lists, indices or other instruction
operands. Every frame belongs to the admitted instance and the first frame is
the declared root. The outer admission contract excludes reference registers,
multiple instances and multi-result functions; artifact decoding enforces the
profile's stack and resource bounds before the unsafe reconstruction call.

`debug-positions.patch` attaches bounded translation-origin spans to instruction
words, preserves them during nearby insertion and merging, and exposes them by
compiled-function identity. It does not change execution or fuel costs. Eager
translation is required for complete positions; synthetic numerical functions
remain explicitly distinguishable from original source functions.
