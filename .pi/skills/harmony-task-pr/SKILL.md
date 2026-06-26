---
name: harmony-task-pr
description: Explains how to invoke pi to implement a Hypervizor task, run required gates, commit the work, push a branch, and open a GitHub PR. Use when starting delegated task work from tasks/*.md.
---

# Hypervizor Task PR

Use this skill when the user wants to launch a pi agent to implement one of this repo's `tasks/*.md` files and open a GitHub PR.

## Recommended invocation

Run from the repo root. Replace the task file, branch/session names, model, and effort level as needed:

```sh
cd /Users/phemberger/workspace/harmony

pi --model openai/gpt-5.5:medium --approve --name "task 01 hypercall-proto" \
  @tasks/00-CONVENTIONS.md \
  @tasks/01-hypercall-proto.md \
  @docs/BUILDING.md \
  "Create branch task-01-hypercall-proto, implement Task 01 only, run all required gates, commit the finished changes, push the branch, and open a GitHub PR with gh. If gh is not authenticated or pushing fails, stop after committing and tell me the exact commands to push/open the PR."
```

## Model and effort syntax

Pi accepts model effort either as a separate flag or as shorthand:

```sh
pi --model openai/gpt-5.5 --thinking medium ...
pi --model openai/gpt-5.5:medium ...
```

Common effort levels:

```text
off, minimal, low, medium, high, xhigh
```

List or search models with:

```sh
pi --list-models
pi --list-models gpt
pi --list-models sonnet
```

## Context files to include

For task implementation, include at least:

- `@tasks/00-CONVENTIONS.md`
- the specific task file, e.g. `@tasks/01-hypercall-proto.md`
- `@docs/BUILDING.md`

`docs/BUILDING.md` is not always strictly necessary if the task repeats the gates, but include it by default because it captures standard cargo gates and portability rules.

## GitHub PR prerequisites

Before asking pi to open a PR, check:

```sh
gh auth status
```

If needed:

```sh
gh auth login
```

For Rust guest/no_std tasks, ensure the target exists:

```sh
rustup target add x86_64-unknown-none
```

## Prompting rules

The launch prompt should tell the worker agent to:

1. create a task-specific branch;
2. implement only the requested task;
3. respect the task's touch-scope restrictions;
4. run all required gates;
5. commit the result;
6. push and open a PR with `gh`;
7. if auth/push/PR creation fails, stop after committing and report exact manual commands.

Prefer interactive mode, not `-p`, for substantial implementation work so the worker can ask clarifying questions or report unexpected failures.
