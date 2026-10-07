# Test your application

Start with a small workflow whose result you can check. Along with the services
that run it, you’ll need traffic to exercise them and assertions that can detect
a wrong result.

## Create a recipe

`harmony init` creates a recipe and refuses to overwrite an existing one. In the
counter project, this command writes a separate starter recipe with C selected
as the language:

{{ example "init" }}

For projects with recognizable files, Harmony can infer C, Rust, Go, Python, or
Java. The starter recipes handle simple builds. If yours needs more setup, you
can supply a build command or Dockerfile.

## Describe the services

The counter tutorial uses this recipe:

{{ file "docs/examples/counter.toml" }}

The setup command creates the shared counter before either writer starts. The
ready command tells Harmony when it can begin exploring, and each writer’s node
name identifies it in the recorded actions and results.

Commands are arrays of arguments. If a command needs a pipe, redirect, or shell
expansion, invoke a shell explicitly. All services share the image’s filesystem
inside one guest, including any client or traffic generator you add. Make sure
something actually sends work to the services you want to test.

## Prepare the application

Build the image, then check that it can run on this host:

{{ example "prepare" }}

{{ example "check" }}

Preparation instruments supported builds, saves their symbols, and checks the
executables for admission. A search can prepare a configured build automatically,
but running preparation separately makes build problems easier to diagnose while
you’re setting up a new application.

Before searching, add [assertions](assertions.md) to tell Harmony which outcomes
are wrong. The [recipe reference](../reference/recipe.md) explains the remaining
application and runtime settings.
