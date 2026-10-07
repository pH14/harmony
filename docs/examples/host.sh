# SPDX-License-Identifier: AGPL-3.0-or-later
# example: build-cli
cargo build --locked --release -p harmony-cli
export PATH="$PWD/target/release:$PATH"
harmony --help
# endexample
