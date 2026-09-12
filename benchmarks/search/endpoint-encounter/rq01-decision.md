# RQ01: native qualification of the bounded retry implementation

The source and fixture pass is complete within its 45-minute limit. It does not
establish native gain. Freeze three cells on msr1 from the original E01 root,
with the same reused qualification seed2026090902 and ordinary draws. No new
root, parameter search or performance panel is included.

1. Default control: exactly the previous AQ01 one-slot 16-job request, unchanged
   bytes. Compare stream, complete campaign summary, root/final checkpoints,
   root snapshot, result and witness inputs with the saved historical output.
   Run this first; failure prevents either candidate dispatch.
2. Candidate: 128 jobs with `local_terminal_retry: true`, one result slot.
3. Same candidate with two result slots. Candidate cells may occupy disjoint
   four-CPU sets together only after the control identity check passes.

All cells have a 100k admitted frame cap, 4,096 input/attempt cap, 128 MiB archive,
60-second search wall, 180-second outer wall, 210-second service runtime, 512 MiB
process memory, 32 MiB per file and 128 MiB output envelope. Each direct helper
budget is800k frames. A separate4M known auxiliary ceiling covers admitted and
replayed-admitted work plus all helper setup/replay frames. Earlier physical-cost
gaps remain explicit. No cell retry, root/seed replacement or wall extension.

Pass requires complete registered jobs, full campaign/checkpoint replay, an
actually surviving local retry whose **linear producing input matches the
original worker-snapshot digest**, and two root-local/two full-prefix witness
replays. A nonzero retry count alone is insufficient. Candidate artifacts must
be identical across result buffering. Helpers consume no inputs from the failed
branch in a surviving witness; all attempted work remains charged by the worker
lifetime frame counter. The new game-policy field pins execution and the result
encoding extension; unknown/mismatched policies fail replay resolution.

This is integration qualification, not independent efficacy or fresh discovery.
A pass earns design/registration of one cheap matched-work conditional falsifier.
It does not promote the mechanism, claim a boss defeat, reset failed earlier
gates or authorize a longer search. A qualification failure stops dispatch;
fix correctness before any new allocation is considered.
