# Run longer searches

You can give an existing search more time, or start a new one from a branch you’ve
saved. Choose based on whether you want to keep the accumulated search history
or explore from a particular guest state.

## Continue a search

After running `followup` in the counter tutorial, give it four more executions:

{{ example "resume" }}

`--resume` restores the latest retained checkpoint of the whole search, including
its archive and scheduler state. The new budget adds four executions and a fresh
30-second window. Saving as `extended` keeps the earlier result available too.

Harmony writes checkpoints periodically. If a search is interrupted, resuming
picks up from its latest retained checkpoint, which may precede the last
execution it attempted.

## Start from a branch

The counter tutorial starts its follow-up search this way:

{{ example "followup" }}

Use a fresh name if you’ve already run this command. `--from` restores the saved
branch state and begins a new search history there, without carrying over the
parent search’s accumulated exploration history.

## Choose a budget

`--executions` limits the number of executions, while `--for` limits elapsed host
time. If you supply both, the search stops when it reaches either limit. The
seed selects the sequence of random draws, but a time limit can stop the search
before it reaches the execution you’re interested in.

Available CPU and memory constrain the search workers. Set host resource limits
if you need to bound consumption. Before increasing the budget, check the summary
to confirm that the application ran, the assertions were reached, and no
execution failures prevented useful work. An unmet reachability assertion may
mean the test never reached the behavior you intended to exercise.
