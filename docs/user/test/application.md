# Test your application

A useful test needs three parts: services to run, traffic that exercises them,
and properties that identify incorrect behavior. Start with one small workflow
that has a clear success condition.

## Create a recipe

`harmony init` creates a recipe without overwriting an existing one. This
explicit-language example writes a separate starter recipe in the counter
project:

{{ example "init" }}

For an unambiguous project, the CLI can infer C, Rust, Go, Python, or Java from
recognized project files. The built-in recipes cover simple applications;
complex builds need an explicit build command or Dockerfile.

## Describe the services

Here is the actual recipe used by the counter walkthrough:

{{ file "docs/examples/counter.toml" }}

The setup command creates the shared file before the writers start. The ready
command establishes when the workload may be explored. Each node has a name
that appears in actions and evidence. Commands are argument arrays; use an
explicit shell if your command requires pipes, redirects, or shell expansion.

Services share one image filesystem inside one guest. Describe your client or
traffic generator alongside the services it exercises. A setup that starts a
server but never sends it work cannot test much about that server.

## Prepare, then search

{{ example "prepare" }}

{{ example "check" }}

Preparation instruments the supported build, preserves symbols, and checks
executable admission. Search can also prepare configured builds automatically.
Keep an explicit preparation step while bringing up a new application so build
failures are easy to distinguish from execution failures.

Add [assertions](assertions.md) before interpreting a search result. The
[recipe reference](../reference/recipe.md) describes where application and
runner settings belong.
