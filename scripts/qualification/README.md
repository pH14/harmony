# Host SHA qualification

`python3 scripts/qualify-host-sha256.py --check` first builds `oci-support`, `unison`, and `searcher` independently, with no VMM feature
unification. It reads compiler-artifact features from each actual target build
and requires ARM acceleration on native ARM64 Linux/macOS, existing defaults on
x86, and no forced software backend. Removing a component's ARM declaration must
fail this check even when another component enables it.

It then builds the production Unison and searcher beside same-source copies
using a renamed software-only SHA crate, all in one executable. The software
source archive is verified against the lockfile checksum and compared with the
production SHA source. Unison machine state hashes, postcard result digests, and derived worker seeds must agree. These checks preserve the existing digest
algorithms, serialized input bytes, and seed derivation.

Omit `--check` for nine alternating native/software timing pairs per operation.
The Unison hash includes its complete 64 KiB machine state; postcard digests
include serialization and hex formatting. Inputs range from empty and SHA padding boundaries to
32 MiB. Every result is checked; there are no timing thresholds in CI. The output
records the executable digest and compiled SHA features. These operation costs
are not a measurement of whole-campaign throughput.

The compiler checks cover all independent dependency owners. Native NES and
other compositions inherit acceleration through these libraries, while VMM
compositions retain their existing explicit selection. The client's image hashes
are gated by its in-process feature, which already brings in the VMM, so the
client needs no additional acceleration declaration. Root and standalone
workspace lockfiles keep the same SHA version. No CPU policy exposed to a guest
changes.

Run `cargo fetch --locked` and
`cargo fetch --locked --manifest-path dissonance/Cargo.toml` before using the
offline qualifier. Both ARM host compatibility jobs and the x86 Snapshot and
Restore job run the bounded check. The VMM's separate SHA qualifier retains its
streaming, alignment, and full-state checks.
