# SPDX-License-Identifier: AGPL-3.0-or-later
# Each region is displayed by MkDocs and executed by docs_examples.py.
# example: setup
mkdir counter-example
cp workloads/bugs/category/lost-update/lost_update.c counter-example/lost_update.c
cp docs/examples/main.c counter-example/main.c
cp workloads/bugs/interleaving.h counter-example/interleaving.h
cp docs/examples/counter.toml counter-example/harmony.toml
export HARMONY_SDK_DIR="$PWD"
cd counter-example
# endexample

# example: prepare
harmony prepare
# endexample

# example: check
harmony check
# endexample

# example: search
harmony search --name baseline --executions 1000 --for 2m
# endexample

# example: findings
harmony show baseline
# endexample

# example: inspect
harmony show baseline --finding 1 --timeline
harmony show baseline --finding 1 --logs
# endexample

# example: state
harmony branch baseline --finding 1 --rewind 1 \
  --exec 'printf "counter, writer-0 completed, writer-1 completed:\n"; od -An -tu8 /tmp/counter' \
  --name before-failure
# endexample

# example: branch
harmony branch baseline --finding 1 --step 0 \
  --exec 'printf "investigating\n" > /tmp/investigation; cat /tmp/investigation' \
  --stop --name debugging
# endexample

# example: followup
harmony search --from debugging --executions 4 --for 30s --name followup
# endexample

# example: resume
harmony search --resume followup --executions 4 --for 30s --name extended
# endexample

# example: compare
harmony diff baseline followup
harmony list
# endexample

# example: shell
harmony branch debugging --step 0 --shell --name interactive
# endexample

# example: shell-input
cat /tmp/investigation
od -An -tu8 /tmp/counter
printf 'shell change\n' > /tmp/shell-change
exit
# endexample

# example: shell-search
harmony search --from interactive --executions 4 --for 30s --name shell-followup
# endexample

# example: script
harmony branch debugging --step 0 --exec-file inspect.sh --stop --name inspected
# endexample

# example: init
harmony init harmony-counter:local --language c --config starter.toml
# endexample
