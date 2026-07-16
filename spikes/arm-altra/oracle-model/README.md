# `oracle-model/` — the analytical taken-branch oracle

**UNTESTED ON SILICON.** This is derivation, not measurement — nothing here has
been checked against a hardware PMU.

The single definition of every payload parameter and every expected taken-branch
count, compiled into *both* the bare-metal payloads (`no_std`) and the host harness
(`std`) so the asm and the model cannot drift. `docs/ARM-ALTRA.md` §Evidence
integrity #5 forbids judging counts by PMU-vs-PMU comparison (circular); this crate
is the independent oracle counts are judged against instead.

## The model

V-time on ARM counts `BR_RETIRED` (raw `0x21`) = retired **taken** branches
(`docs/ARM-PORT.md`, `docs/ARM-ALTRA.md` §2). A window's count decomposes as

```
measured = certain_taken + reported_taken
         + w_entry·entries + w_eret·erets + w_svc·svcs + w_wfi·wfis
         + window_offset
```

`certain_taken` is derived exactly. The four `w_*` weights and `window_offset` are
the **unknowns the spike measures** — so `Weights` has **no `Default`** and no
invented values: a checker handed no weights refuses to check counts rather than
guess (task 109's "no invented constants"). The payload set is chosen so the four
weights are separately identifiable from measurements (five independent equations,
four unknowns — over-determined); `solve()` recovers them and returns the residual,
so a nonzero residual is evidence the model is wrong about the silicon, not noise.

`Expectation` is **serialize-only**: a manifest can be written, but nothing may read
one back and believe it — consumers recompute from `(payload, scale, seed)`, which
is evidence-integrity #2 enforced by the compiler.

## Test

```sh
cargo test --features std     # 17 derivation tests + 2 TCG-observed accumulator pins
```

`tests/tcg_observed.rs` pins the accumulators the *executed asm* produced under
`qemu-system-aarch64`, so a change to the asm predicates or the model that broke
their agreement fails the build. That validates the branch predicates and the PRNG;
it says nothing about whether hardware counts those branches, which is stage AA-1's.
