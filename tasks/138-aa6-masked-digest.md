# Task 138 — AA-6 masked-register-digest lane (named condition on the full-GO upgrade)

**Bead:** hm-3bwm (pairs with hm-fiqo). **Binding:** `docs/ARM-ALTRA.md` §AA-6 + Paul's
2026-07-22 ratification (Fable second-opinion confirmed) of the four AA-6 gate-semantics
changes. This task is the **NAMED CONDITION** on upgrading the AA-6 disposition from
PROVISIONAL GO to full GO: prove at gate scale that gate-semantics change **#4** — the
LinuxGuest console+vGIC digest narrowing — is **exactly-and-only** the disclosed AA-5(c)
stack-ASLR entropy residual (hm-of6t F12), and NOT masking an injection-path register
divergence.

## What to build (~20 portable lines + one box lane)

1. **Masked register digest** in the existing AA-6 harness (`spikes/arm-altra/`): digest the
   **full LinuxGuest register file MINUS exactly {x29, SP}** (host-time CNTPCT/TIMER_CNT are
   already digest-excluded — keep those exclusions, add no others). The mask is a closed
   list; widening it in any way is a spec violation, not a fix.
2. **hm-fiqo pairing (in scope):** emit `injected_landed_digest` — the register identity at
   the injection Moment — instead of discarding it. This supplies the injection-Moment
   register witness the lane compares.
3. **Box lane:** run the AA-6 **injection** configuration (inject-ppi / inject-at-work ON —
   the same config as the merged PR #139 matrix) for **≥1000 reps**, comparing the masked
   register digest across reps.

Record the injection config (ON, which mode) explicitly in the evidence dir — the run-set
attestation for this doesn't exist yet (hm-oh3v is separate work; do NOT build it here, but
don't leave the config undocumented either).

## Acceptance

- ≥1000 reps with the masked register digest **bit-identical across all reps** (and the
  emitted injection-Moment witness digests bit-identical rep-to-rep).
- The only excluded general registers are {x29, SP}; exclusions are enumerated in the
  evidence output, not implied.
- Portable gates green for any code touched (build/nextest/clippy/fmt/deny); the digest
  logic itself gets a portable unit test (known register file in ⇒ known digest out;
  mask exactly {x29, SP}).

## Disposition

- **All-identical** ⇒ named condition MET. Record the evidence + a condition-met note in
  `docs/ARM-ALTRA.md` §AA-6 (do not flip PROVISIONAL→GO yourself — that upgrade is Paul's
  ruling; the foreman escalates it with your evidence).
- **Any divergence outside {x29, SP}** ⇒ possible injection-path register divergence that
  the console+vGIC narrowing was masking — **P0-class STOP: commit the evidence, PARK,
  escalate**. Never widen the mask, never narrow the digest further to get to green.

## Surface list (frontier task)

`spikes/arm-altra/**` (harness + scripts + evidence), `docs/ARM-ALTRA.md` (§AA-6 evidence
note only). Nothing else — no consonance/dissonance crate code, no CI files.

## Environment

Box: `ssh harmony-arm` (Ampere Altra / N1), handed back **idle and already on the patched
`6.18.35-aa3preempt` kernel** (verify `uname -r` before running; if reverted, grub-reboot
boot-once into aa3preempt — stock stays default). Pin every run with `taskset` per
`docs/BOX-PINNING.md`, SMT sibling idle. **Smoke-fire-once**: a ~20-rep masked-digest batch
first, report it, then the full ≥1000. Bundle-transfer code to the box (git push to box is
classifier-blocked). **Commit+push evidence promptly** (the box was account-wiped once on
2026-07-20). Leave the box on aa3preempt when done. Payload pins: current main's
regenerated-pin basis (post-PR #140) — do not resurrect pre-wipe pins (see the
aa3-recert-pins landmine memory). You are on **Opus 4.8** deliberately (ARM/virt spike
class — standing model-routing rule).
