# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
FROM rust:1.97.0-bookworm@sha256:8fa55b2f3ddf97471ab6a767bfa3f37e6bad0986ba823e75fea57e2a2a5c3073 AS compiled
WORKDIR /build/workloads/languages/rust
COPY workloads/languages/rust /build/workloads/languages/rust
COPY workloads/faults/runtime/tests/language_fixture.rs /build/workloads/faults/runtime/tests/language_fixture.rs
ENV RUSTFLAGS="-Ccodegen-units=1 -Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=3 -Cllvm-args=-sanitizer-coverage-trace-pc-guard -Clink-args=-Wl,--build-id"
RUN target="$(rustc -vV | sed -n 's/^host: //p')" \
    && cargo build --release --locked --target "$target" \
    && mkdir -p /out/symbols \
    && cp "target/$target/release/harmony-rust-language-fixture" /out/fixture \
    && cp /out/fixture /out/symbols/fixture \
    && nm --defined-only /out/fixture | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > /out/symbols/rust.sym.tsv \
    && sha256sum /out/fixture | awk '{print $1 "  /opt/harmony/fixture"}' > /out/symbols/harmony-instrumented-events
FROM ${DEBIAN_IMAGE} AS language-base
COPY --from=compiled /out/fixture /opt/harmony/fixture
COPY --from=compiled /out/symbols /symbols
CMD ["/opt/harmony/fixture"]
