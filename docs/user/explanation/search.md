# Searching for failures

One successful execution shows what happened under one sequence of conditions. A fault campaign tries many alternative sequences and retains useful evidence, including inputs that lead to a property violation.

## The workload defines correctness

Harmony knows how to stop, pause, and restart supervised processes. It does not know whether your database lost an acknowledged write or your service broke a consistency rule. Your checker or SDK assertions define those properties.

A workload bundle brings together services, a traffic driver, optional operations, and a checker. All of them run within one guest. A process being killed is usually an intentional test action; the bug is the application breaking its stated contract as a consequence.

## A campaign explores continuations

The execution environment can save a stopped state and restore it to try a different continuation. This lets the campaign explore alternatives without always repeating all the preceding work. Wait lengths and fault timing are selected by the search, and the actual durations are stored in the resulting input.

You control the campaign's budget through workers, executions, and actions. Fault search can also be bounded by host time. Increasing the budget gives the search more opportunity; it is not a guarantee that a particular bug will be found.

## Discovery and confirmation are separate

When a search encounters bug evidence, it can replay that input in a fresh session. Reports distinguish the original observation from confirmation. An input that looked suspicious but did not reproduce is not counted as a confirmed rediscovery.

You can repeat a recorded bug independently with `--replay` and `--repeat`. This uses the stored actions rather than restarting the search. For a continuous checker, replay may add a settling tail to obtain fresh evidence after the final disturbance.

## Read a result in context

“No bug found” is meaningful only alongside evidence that the workload ran, made progress, and checked the intended properties. A timeout, failed setup, unreachable checker, or exhausted budget can all prevent a useful conclusion. Keep those outcomes separate from passing assertions.

Continue with [your first fault search](../tutorials/first-search.md) or [prepare your own workload](../how-to/fault-workload.md).
