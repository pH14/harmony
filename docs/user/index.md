---
hide:
  - toc
---
# Find a bug. Get back to it.

Harmony explores different executions of your application and saves the ones
that break a property you care about. You can return to a finding, inspect what
happened, and change the conditions before exploring again.

For example: two writers update a counter. Both finish their work, but one
silently overwrites the other's increment. Harmony can hold a writer between
its read and write, expose the lost update, and save that execution for investigation.

[Find your first bug](start/first-bug.md){ .md-button .md-button--primary }
[Install Harmony](start/install.md){ .md-button }

## What you bring

An application, a way to exercise it, and assertions that say what must remain
true. Harmony prepares supported language builds with instrumentation, runs
services in a controlled Linux environment, and explores changes in execution
and fault timing. Your assertions distinguish an application bug from an
intentional disturbance such as killing a process.

## Three things to know

| Thing | What it gives you |
| --- | --- |
| **Search** | An exploration that tries many executions and accumulates findings. |
| **Finding** | Evidence of a property violation in a particular execution. |
| **Branch** | A saved point you can inspect, change, and explore further. |

Start with the [counter walkthrough](start/first-bug.md), then
[configure your own application](test/application.md). If you already have a
finding, go to [investigation](investigate/index.md).

Harmony is experimental. Check the [supported environments](reference/environments.md)
before preparing a workload. [NES workloads](nes.md) have a separate setup path.
