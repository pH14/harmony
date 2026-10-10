# Install Harmony

You’ll build the CLI from the same checkout as these docs. You’ll also need
Docker or Podman to prepare the tutorial application. Allow extra time for the
first language build to download and compile its dependencies; subsequent
builds can reuse the container layers.

## Build the command-line tool

Clone the [Harmony repository](https://github.com/pH14/harmony) and install Rust
through [rustup](https://rustup.rs/). The checkout selects the required toolchain.
From the repository root, build the CLI and add it to your terminal’s path:

{{ example "build-cli" }}

Keep this executable and checkout while investigating findings. Saved results
record which executable and runtime artifacts produced them, and a rebuild can
change that identity.

## Set up the runtime

On Linux, Harmony can use KVM if your user has access to `/dev/kvm`, or User-mode
Linux (UML) without hardware virtualization. Apple silicon Macs use
Hypervisor.framework and require the entitlement in `cli/harmony.entitlements`.
The [CLI setup instructions](https://github.com/pH14/harmony/blob/main/cli/README.md)
and [guest runtime README](https://github.com/pH14/harmony/blob/main/consonance/harmony-linux/README.md)
cover the platform requirements and runtime builds.

For the KVM or Apple silicon walkthrough, use those instructions to build the
matching kernel and OCI initramfs. Then, from the repository root, install the
executable and guest files together:

{{ example "install-runtime" }}

This layout lets Harmony find the guest files when you change into an application
directory. Keep the terminal open for the walkthrough. On macOS, sign the
installed executable using the entitlement instructions before running a guest.
If you’re using UML, follow the runtime README to set up its separate profile.

`harmony check` checks the requirements for your recipe and can download missing
runtime assets from a matching release. For a development revision without
published assets, you’ll need the local builds described above.

You’re ready to [find your first bug](first-bug.md). The tutorial walks through
preparing the application image and checking it before the first search.
