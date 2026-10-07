# Recipe reference

Commands read `harmony.toml` by default. `--config` selects another file;
`--config-toml` supplies the same format inline. Explicit command-line settings
take precedence. File paths are relative to the recipe directory; inline paths
are relative to the current working directory. Unknown fields are rejected.

## A complete application recipe

This is the recipe built and executed by the documentation's counter walkthrough:

{{ file "docs/examples/counter.toml" }}

| Section | Owns |
| --- | --- |
| `workload` | Package and input identity. An OCI input selects the faults package; a `.nes` input selects NES. |
| `workload.options` | Package-specific build, nodes, setup, readiness, checker, traffic, and hooks. |
| `runner` | Execution implementation and backend choice. |
| `runner.options` | Runtime-specific paths and resource settings. |
| `search` | Seed, execution budget, and wall-clock budget. |

For Consonance, runtime overrides belong under `runner.options`: `kernel`,
`base_initramfs`, and `uml_profile`. These are environment setup choices, not
application behavior. An explicit backend request is validated against the
workload and host.

The example uses automatic backend selection. For a different environment,
check [support and requirements](environments.md) before changing it.
