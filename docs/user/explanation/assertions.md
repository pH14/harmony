# Writing useful assertions

A useful assertion expresses an application promise in terms of an observation your test can actually make. For example: “Every acknowledged record remains readable after the service recovers.” That is more informative than “the server process is still running,” especially when the test intentionally kills it.

## Safety and progress answer different questions

An **always** property describes something that must not go wrong when it is observed: balances must not become negative, committed records must not disappear, and duplicate requests must not apply a payment twice.

A **sometimes** or **reachable** point records that an interesting event happened: the service accepted traffic, an operation completed, or a recovery check reached a conclusive result. These points help distinguish an uneventful test from one that exercised the intended behavior.

Failure to reach a point in a finite run is not automatically a correctness violation. The campaign may have run out of budget, or its workload may never have created the required conditions.

## Avoid declaring intentional faults to be bugs

During a fault campaign, a service may be stopped or temporarily unavailable. A checker should distinguish:

- A conclusive incorrect result, which should emit a violated property.
- A conclusive correct result, which can emit a reached/sometimes point.
- An inconclusive observation, which should wait for another opportunity instead of claiming success.

Decide what availability your system actually promises under the injected failure. An application that promises tolerance of one failed replica can assert that promise; an application with no such promise should not report every killed process as a correctness bug.

## Check recovery using fresh observations

A check that passed before a crash cannot show that data survived the crash. Harmony records disturbance generations and completed-check evidence so a replay can distinguish earlier hits from a check completed after the final disturbance.

Emit a supported reached point only after the checker has made its conclusive observation. For continuous checks, IDs `0`–`47` can be represented in that completed-check evidence. A true `@always` line alone does not mark a completed observation.

## Validate the checker itself

Begin with a deliberately false property and verify that it appears as a violation in the report. Then exercise a case you know should satisfy the property. This catches common integration mistakes: output sent to the wrong place, an incorrect ID, a malformed directive, or a check that never runs.

Keep property IDs stable and describe them beside your workload. A report containing violation `7` is useful only if you know which application promise `7` represents.
