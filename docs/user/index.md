---
hide:
  - toc
---
# Find and investigate bugs

Harmony tests your application by exploring how it behaves under different
execution schedules and failures. When an assertion fails, it saves the
execution so you can return to it, inspect the state, and try changes.

The [first tutorial](start/first-bug.md) uses a shared counter with a race that
loses increments. You’ll find the bug, examine the counter before the failure,
and start another search from a state you’ve changed.

[Find your first bug](start/first-bug.md){ .md-button .md-button--primary }
[Install Harmony](start/install.md){ .md-button }

## What you’ll need

Bring an application, some traffic or other work for it to handle, and assertions
that describe correct behavior. Harmony can prepare supported language builds
and run your services in a controlled Linux environment, where it varies
execution and fault timing. The assertions tell it when those changes have
exposed a bug.

## Searches, findings, and branches

These are the three terms you’ll see throughout the CLI and these docs:

| Term | Meaning |
| --- | --- |
| **Search** | A run that explores many executions and collects findings. |
| **Finding** | Evidence that a property failed in a particular execution. |
| **Branch** | A saved point you can inspect, change, and search from. |

After the tutorial, [configure your own application](test/application.md).
If you already have a finding to examine, go to [Investigate](investigate/index.md).

Harmony is experimental, so check the [supported environments](reference/environments.md)
before setting up an application. [NES workloads](nes.md) have their own setup
instructions.

Harmony is licensed under [AGPL-3.0-or-later](https://github.com/pH14/harmony/blob/main/LICENSE).
