# Recipe reference

Harmony reads `harmony.toml` by default. Use `--config` to select another file,
or `--config-toml` to supply a recipe inline. Explicit command-line settings
override the recipe, and unknown recipe fields are rejected.

Paths in a recipe file are relative to its directory. For an inline recipe,
paths are relative to your current working directory.

## Configure an application

This is the recipe used by the counter tutorial:

{{ file "docs/examples/counter.toml" }}

Its settings are grouped by what they control:

| Section | Settings |
| --- | --- |
| `workload` | The package and input. An OCI input selects the faults package; a `.nes` input selects NES. |
| `workload.options` | Package-specific build, node, setup, readiness, checker, traffic, and hook settings. |
| `runner` | The execution implementation and backend. |
| `runner.options` | Runtime paths and resource settings. |
| `search` | The seed, execution budget, and time budget. |

For Consonance, put local runtime paths such as `kernel`, `base_initramfs`, and
`uml_profile` under `runner.options`. This keeps the runtime setup separate from
the application’s services and behavior.

The tutorial lets Harmony select a backend automatically. You can request one
explicitly, provided it supports the workload and host. Check the
[supported environments](environments.md) before changing that setting.
