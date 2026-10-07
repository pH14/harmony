# Find and investigate your first bug

Two processes increment the same counter. Each reads the old value, adds one,
and writes its result. If one writer waits between reading and writing, it can
overwrite the other writer's completed increment.

The assertion is: **the counter includes every completed increment**. This is a
real property of the program. It passes in an ordinary uninterrupted increment
and fails when the writers interleave badly.

This walkthrough prepares the existing lost-update example, searches for that
failure, and opens a branch for investigation. Use a
[supported host](../reference/environments.md), the built CLI, and Docker or
Podman. Start in the Harmony repository root.

## 1. Copy the example

{{ example "setup" }}

The source and its support header come from the repository's tested lost-update
workload. The supplied recipe names two writers and a setup command that creates
their shared counter. It uses the standard instrumented C build.

## 2. Prepare and check it

{{ example "prepare" }}

Preparation builds the image, retains symbols, and checks admission. The first
build can take several minutes. It does not yet search the application.

{{ example "check" }}

Resolve any reported missing runtime or host requirement before continuing.
A successful check means the execution prerequisites are available, not that the
application is correct.

## 3. Search for a lost update

{{ example "search" }}

The search stops at 1,000 executions or two minutes, whichever comes first.
A search that finds a violation exits with status 1; that is an application
finding, not a failed installation. Infrastructure errors use status 2.

{{ example "findings" }}

Look for a confirmed finding for **the counter holds every finished increment**.
Search results and timing can vary. If this budget finds nothing, inspect the
summary for execution failures and assertion reachability before spending more
time. The [search guide](../search.md) explains the difference between continuing
a search and starting at a saved branch.

The remaining steps require finding 1. CI runs this same bounded example and
fails if it cannot produce the finding; a successful build alone is not accepted
as evidence that the tutorial works.

## 4. Inspect the failing execution

{{ example "inspect" }}

The timeline shows recorded actions and observations. Logs show retained
application output. A quiet application can have little output even when the
assertion evidence identifies a failure. The timeline is the better starting
point for this counter.

## 5. Inspect the counter before the failure

Move back one action from the first observed failure, print the shared counter,
and continue the recorded actions:

{{ example "state" }}

The three numbers are the shared counter and each writer's completed increments.
Compare the first number with the sum of the other two. You are looking at an
earlier point, so it may still satisfy the property. Continuing the suffix lets
you investigate how that state develops. Opening a guest command can itself
advance the application; this branch is a new experiment.

## 6. Save a point for another search

Return to step 0, the prepared initial state, and write a marker in the guest.
Saving without the remaining recorded actions gives us an uncomplicated starting
point for another search:

{{ example "branch" }}

The command prints `investigating`. Its file change belongs to the saved
`debugging` branch. The original search remains available.

{{ example "followup" }}

This starts a new four-execution search from the changed guest state. It does
not need to find another bug to demonstrate that the branch is usable.

Continue with [interactive investigation](../investigate/index.md) to open a
shell, change the state, and search from it. The
[application guide](../test/application.md) explains how to replace this example
with your own services.
