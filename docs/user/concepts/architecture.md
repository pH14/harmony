# How Harmony fits together

Harmony’s components divide the work of choosing executions, running them, and
interpreting the results:

| Component | What it does |
| --- | --- |
| CLI | Prepares recipes and manages saved searches, branches, and evidence. |
| Workload package | Defines the inputs, actions, observations, and conditions for progress or failure. |
| Runner | Executes the workload and identifies its required runtime artifacts. |
| Consonance | Controls execution and provides state that can be saved and restored. |
| Dissonance | Chooses which states and continuations to explore. |

For application testing, the faults workload describes services, process faults,
and assertions. Consonance runs the application inside a controlled Linux guest,
using KVM, Hypervisor.framework, or UML according to the host and configuration.

The NES workload supplies a different set of actions and observations for games.
It can run with native QuickNES or a supported Consonance adapter. The available
commands depend on this combination of workload and runner; a native emulator,
for example, has no guest shell to open.

## How a search uses saved state

Harmony can save a state reached by the application, restore it, and try different
continuations. The workload evaluates what happened, and Dissonance uses that
feedback to choose what to explore next. When an execution produces a finding,
Harmony retains the evidence needed to return to it.

Branches use the same ability to restore state. Once you’ve reached a useful
point, you can try changes from there without starting each investigation with
another uncontrolled run through the application’s setup.

The repository’s [architecture guide](https://github.com/pH14/harmony/blob/main/docs/ARCHITECTURE.md)
describes the implementation and component boundaries in more detail.
