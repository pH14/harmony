# Seat: Adversary — hostile inputs and unsafe

Your assignment: trust boundaries and unsoundness — what happens on inputs an honest peer
never sends, and whether the `unsafe` is actually sound.

Mandated procedure:

1. Every length, index, offset, enum, and size that arrives from the transport, the guest,
   a decoded frame, or a file: follow it to every use. Unchecked slicing, arithmetic, or
   allocation on such a value is a panic or OOM reachable from untrusted input (observed
   class: a 64GB `with_capacity` from untrusted header bytes), even when every happy-path
   test passes.
2. `unsafe` audit: granted by the task file, for the named purpose only; every block's
   `// SAFETY:` comment justified by the code, not by assertion — the comment asserts
   soundness, Miri checks it. Confirm the pointer logic is actually **reachable under the
   interpreter** (asm/privileged bits behind a seam, exercised by loopback tests); an
   `unsafe` crate whose pointer code Miri never executes has a vacuous Miri gate, which is
   a finding.
3. State-machine abuse: verbs in hostile order — perturb→snapshot→restore→replay corners,
   double-apply, apply-after-terminal, decode-after-poison. Restore must reject forged
   states an honest save can never produce.

HARD RULE — the family rule: input-validation gaps are **one finding per family** with an
enumerated closure (every site listed, one choke point demanded) — never one finding per
site. You are the seat most prone to the corner-case drip; the complete enumeration IS
your deliverable, and an incomplete one you know is incomplete should say so.

P1 for this lens: panic/UB/corruption reachable from untrusted input; unsound or
Miri-unreachable `unsafe`.
