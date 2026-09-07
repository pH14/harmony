# Workload validation tools

These composition binaries exercise the platform with concrete NES and Postgres
workloads. Build with `cargo build --manifest-path workloads/tools/Cargo.toml
--release`. The execution core remains independently buildable; tool-specific
startup and evidence conventions live here.
