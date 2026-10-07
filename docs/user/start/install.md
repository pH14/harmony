# Install Harmony

Build the CLI from the same checkout as these docs. You also need Docker or
Podman to prepare the tutorial application. The first language build downloads
and compiles dependencies; later builds reuse container layers.

## Build the command-line tool

Clone the [Harmony repository](https://github.com/pH14/harmony), enter its root,
and install Rust through [rustup](https://rustup.rs/). The checkout selects the
required toolchain. Then build and add the executable to this terminal's path:

{{ example "build-cli" }}

Keep that executable and checkout while investigating saved findings. Saved
experiments identify the executable and artifacts that produced them; rebuilding
the program can change that identity.

## Prepare the execution environment

On Linux, Consonance can use KVM when your user can access `/dev/kvm`, or
User-mode Linux (UML) without hardware virtualization. On Apple silicon macOS,
it uses Hypervisor.framework; the executable needs the entitlement in
`cli/harmony.entitlements`. See the source checkout's
[CLI setup](https://github.com/pH14/harmony/blob/main/cli/README.md)
and [guest runtime build instructions](https://github.com/pH14/harmony/blob/main/consonance/harmony-linux/README.md)
for platform setup and development runtime artifacts.

`harmony check` checks the selected workload and runner and obtains missing
versioned runtime assets when a matching release publishes them. A development
checkout may require locally built artifacts. Provisioning support does not mean
every development revision has downloadable assets.

The tutorial explains how to prepare its image before checking it. Continue to
[find your first bug](first-bug.md).
