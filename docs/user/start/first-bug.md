# Find and investigate your first bug

We’ll start with a shared counter that loses increments when its two writers
race. The program tracks each writer’s completed increments separately, so it
can detect when the total falls behind.

You’ll use Harmony to find one of these failures, inspect the counter, and save
a branch to test further. Before starting, follow the
[installation instructions](install.md) and have Docker or Podman running.
The commands below start in the Harmony repository root.

## 1. Copy the example

The repository includes the C source and a recipe for running both writers.
Copy them into a working directory:

{{ example "setup" }}

The recipe initializes the shared counter before starting the writers and uses
Harmony’s standard C build to instrument the program.

## 2. Prepare and check it

Build the application image:

{{ example "prepare" }}

This also saves the debugging symbols and checks that Harmony can use the image.
Allow several minutes for the first build.

Next, check that the host and runtime are ready:

{{ example "check" }}

If this reports a missing dependency or a host configuration problem, resolve it
before continuing.

## 3. Search for a lost update

Give Harmony up to 1,000 executions or two minutes to find the race:

{{ example "search" }}

Finding a bug causes this command to exit with status 1. That’s the expected
outcome here; status 2 indicates an error running the search.

Open the results:

{{ example "findings" }}

You should see a confirmed failure of **the counter holds every finished
increment**. If no finding appears, check the summary for execution errors and
whether the assertion was reached. See [Run longer searches](../search.md) for
how to continue the search.

The following commands investigate finding 1 in `baseline`. If you saved a
longer search under another name, substitute that name below.

## 4. Inspect the failing execution

Open the timeline and application logs:

{{ example "inspect" }}

Start with the timeline, which includes the actions leading up to the failed
assertion. This example writes little to its application logs, though those
logs can be useful when investigating your own program.

## 5. Inspect the counter before the failure

Go back one action before the failure and read the counter:

{{ example "state" }}

The output contains three numbers: the shared total, followed by each writer’s
count of completed increments. If the first is smaller than the other two added
together, an increment has been lost.

Here you’re reading an earlier state, and the writers can run while the command
executes. The numbers may therefore differ from those at the original failure;
use the recorded assertion as evidence of the bug. After the command finishes,
Harmony continues the recorded actions and saves the result as `before-failure`.

## 6. Save a point for another search

You can also change the guest and search from that changed state. To try this,
return to the prepared initial state at step 0 and create a file:

{{ example "branch" }}

The command prints `investigating` and saves a branch called `debugging` with the
new file in it. `--stop` saves that state without running the remaining recorded
actions.

Run four executions from this branch:

{{ example "followup" }}

Each starts with `/tmp/investigation` present. You now have a separate search
called `followup`; `baseline` is still available for comparison.

Continue to [Investigate](../investigate/index.md) to try an interactive shell,
or [Configure an application](../test/application.md) to test your own program.
