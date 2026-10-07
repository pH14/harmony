# Run longer searches

Choose whether to keep exploring the current search history or explore from a
specific changed state. These operations have different starting points.

## Continue the same search

After the counter walkthrough's `followup` search:

{{ example "resume" }}

`--resume` restores the latest retained whole-search checkpoint, including the
search archive and scheduler state. The budget is additional: this command
allows four more executions and a fresh 30-second wall-clock window. It saves
as `extended` so the earlier evidence stays available.

Checkpoints are periodic. An interrupted search can resume from its most recent
retained checkpoint rather than its final attempted execution.

## Search from a branch

{{ example "followup" }}

This example is the walkthrough's original follow-up command; use a fresh name
if you already ran it. `--from` starts fresh search history at the saved branch
state. It does not inherit all the exploration history of the parent search.

## Set a useful budget

`--executions` bounds logical search work. `--for` bounds host elapsed time.
When both are supplied, the first limit reached stops the search. A seed selects
the draw sequence; it is not a guarantee of a particular finding within a
wall-clock budget.

The process's available CPU and memory constrain its workers. Use host resource
limits to bound consumption. More workers and longer time give the search more
opportunities; they do not make an unchecked property meaningful.

Read the summary before interpreting “no finding”: did the application execute,
did the intended assertions run, and were there execution failures? An unmet
reachability assertion is useful evidence that the test never reached what you
wanted to test.
