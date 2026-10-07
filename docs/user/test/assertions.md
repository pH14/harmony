# Define what must remain true

Give Harmony properties that capture what your application promises to its users.
An acknowledged record should remain readable, for example, and a completed
transfer should preserve the total balance. These checks let a search recognize
a bug when it encounters one.

The counter tutorial checks **the counter holds every finished increment**.
Each writer tracks its completed increments separately, so the program can
notice when their sum exceeds the shared total.

## Check that the test reaches your assertions

An **Always** assertion describes a property that must hold whenever it is
checked. A **Sometimes** assertion describes a condition you want the search to
reach. Use them together to check both that the interesting operation happened
and that its result was correct. An Always assertion that never runs cannot
tell you whether the application behaved correctly.

Choose the observation point carefully. Reading several values while they’re
changing can produce an inconsistent picture and a false report. Likewise, a
check that runs only at startup won’t catch a failure during recovery.

## Report properties from your application or a checker

You can use a language SDK to report assertions from application code, or a
workload checker to examine application state. Give each property a stable
identity and include enough evidence to understand a failure. The
[SDK and language integrations](https://github.com/pH14/harmony/tree/main/consonance/harmony-linux/sdk)
document the available APIs.

During a fault search, Harmony deliberately pauses, kills, and restarts
processes. Your assertions should check whether the application still meets its
promises under those conditions. A process being killed is expected; losing an
acknowledged record after recovery may be a bug.

Before spending more time on a search, check that it reaches your assertions and
that they cover recovery as well as startup.
