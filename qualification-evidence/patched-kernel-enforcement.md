# What the determinism kernel changes on this chip: nothing observable

The AMD draft contract column names kernel tag v6.18.35. Item 6 built that kernel with
the SVM half of the determinism series re-anchored by hand, and booted it. This is the
enforcement half of item 7 re-run on it, so the contract column is measured against the
kernel it names rather than against the stock Debian kernel.

Records: `box/stage2/patched-close.log`, and the `*-patched.json` files beside it.
Run on core 3, which is isolated and was not carrying the attested-exit supplement that
occupied the other fifteen cores at the time.

## Result

Every record is byte-identical to the one the stock kernel produced.

| probe | stock record | patched record |
| --- | --- | --- |
| `ae4-freeze` CPUID freeze | `ae4-cpuid-freeze.json` | identical |
| `ae4-msr` MSR default-deny | `ae4-msr-deny.json` | identical |
| `singlestep-driver --mode tf` | `singlestep-tf.json` | identical, all five payloads |
| `singlestep-driver --mode btf` | `singlestep-btf.json` | identical, all five payloads |
| `ae6-rdrand` x5 | `ae6-rdrand.json` | executes, no fault, distinct values |

The two single-step runs exit 1 on both kernels. That exit code is the harness's
convention for "some payload was not exact", and the two inexact payloads are the same
on both kernels and were already characterised on the stock half: TF misses one
instruction behind the `movss` shadow, which is an x86 rule, and BTF delivers no debug
exits at all on this part. Nothing regressed and nothing improved.

## Why that is the expected result and still worth recording

The series' RDTSC intercept is wired in `vmx.c` only, so on SVM there is nothing for it
to enforce; SVM's intercept vector has no RDRAND or RDSEED control at all. Both are
written up in `amd-determinism-kernel-gap.md` and `amd-rdrand-not-interceptable.md`. The
measurement confirms from the guest side what the source reading says: on this vendor the
patched kernel and the stock kernel present the same enforcement surface.

## Whole-stack repetition, supporting evidence only

`ae5-gate --core 3 --event 0x5100d1 --margin 16192 --reps 1000` exits 0:
1,000 repetitions of arm, force-exit, inject a fault at a fixed guest physical address,
and digest the resulting state. All 1,000 took the preemption exit, all 1,000 reached the
guest HLT, and all 1,000 digests are `0x82a17b98225b88de`.

What the file can and cannot support. The harness keeps a head of eight per-repetition
rows plus every row whose digest differs from the first, so the file holding exactly
eight rows is itself the evidence that no repetition diverged. The `all_preempt` and
`all_hlt` flags are aggregates: a repetition after the eighth that failed either one
would set its flag to 0 and fail the run, but would leave no row of its own. So bit
identity across all 1,000 is recoverable from the raw file, and the exit-reason claim
for repetitions 8 through 999 rests on the aggregate. Stage 3 is out of scope for this
program, so this is supporting evidence, not a qualification claim.

This ran while the fifteen-shard supplement occupied every other core, so it is also a
co-tenancy result: a fully loaded box did not perturb it.
