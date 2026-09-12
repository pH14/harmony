# SR01: native scoped-return implementation qualification

This allocation follows the source contract in scoped-return-design.md. Source
is d8f795c218ad66c4535e457f44009eab71449261, published with 1,184 normal pre-push
checks passing and 23 skipped. Its native binary and source/build hashes are
frozen in sr01-registration.json. No SR01 emulator execution has occurred at
publication.

Run exactly four sequential cells on ms02 CPUs4–7: ordinary/matched coin,
progress/matched coin, progress/half return, scoped capacity/half return. All use
the same new frozen qualification seed, feature build, supplied root, alphabet,
thresholds and250,000-frame horizon. A cell stops at a living Ridley defeat or
the fixed horizon; its20,000-execution and60-second search ceilings are safety
stops, not substitutes for reaching that horizon. Whole processes have90seconds;
the service420seconds,4GiB and zero swap. Maximum output is1GiB,128MiB per file.
At most10,100,000 physical frames are allocated; the registration derives the
bound from caller limits and the bounded4-worker drain. Unused work is released.

All four must pass source/asset/root identities, complete replay and witness
checks, exact physical accounting and expected policy identities. Optional arms
must actually admit alternatives. Progress/control and progress/half must have
identical recorded histories up to a first changed group-walk parent, with the
same mutation seed and selector record. This checks activation of the compiled
mechanism; no outcome comparison, p-value or efficiency claim is made. Zero
activation fails qualification without seed replacement. Source tests establish
the rule's scoped/resource/eligibility constraints; native complete replay checks
the actual changed history. Ordinary and capacity are included so their real
serialized contracts are qualified before a later screen.

The scorer tests load all three actual PG01 arm reports, plus both metered PG02
reports, preserving omission of ordinary slot_retention. New-ID fixtures are
explicitly synthetic schema checks; planted wrong selectors, cost gaps, replay
failures, short horizons and false activation must fail. The wrapper preserves
start/process/usage records and its cgroup receipts before exiting; it returns a
complete report separately from the qualification verdict. No collect-on-failure
step can erase a receipt. Stop on the first invalid cell and preserve every
launched cost. No retries, old-seed reuse, pooling with PG02, or automatic efficacy
allocation. The20-minute native deadline and35-minute synthesis deadline are
absolute timestamps in the published registration.

The full fresh-search goal remains unachieved. A pass only permits separately
registering a controlled conditional endpoint screen at the unchanged1M horizon.
Fresh development, independent confirmation, MM2 evaluation and untouched
validation remain separate gates. msr1 is not accessed.
