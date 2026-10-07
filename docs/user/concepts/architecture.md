# How Harmony fits together

Harmony separates the decision about what to try from the machinery that
executes it.

| Component | Responsibility |
| --- | --- |
| CLI | Prepare recipes, select execution, manage saved searches and branches, expose evidence. |
| Workload package | Define inputs, available actions, observations, and what counts as progress or failure. |
| Runner | Execute a workload and identify the runtime artifacts it requires. |
| Consonance | Provide controlled execution and state that can be saved and restored. |
| Dissonance | Select states and alternative continuations to explore. |

For application testing, the faults workload interprets services, process
faults, and assertions. Consonance executes the application inside a controlled
Linux environment. Its backend can use KVM, Hypervisor.framework, or UML,
according to the host and supported configuration.

The NES package uses game-specific actions and observations. It can use native
QuickNES or a supported Consonance adapter. Those capabilities belong to the
package and runner combination; not every command is meaningful for every
workload.

## A search, one continuation at a time

The application reaches a state. Harmony can retain it, restore it, and try a
different continuation. The workload evaluates the resulting observations;
Dissonance uses that feedback to decide what to explore next. Findings retain
the evidence needed to return to interesting executions.

This is why branching is useful: a long setup need not be repeated as a new
uncontrolled experiment every time you want to ask what happens next.

For implementation boundaries, see the repository
[architecture](https://github.com/pH14/harmony/blob/main/docs/ARCHITECTURE.md).
