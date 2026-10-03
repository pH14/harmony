# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
FROM ${DEBIAN_IMAGE} AS compiled
RUN apt-get update && apt-get install --yes --no-install-recommends clang gcc libc6-dev binutils \
    && rm -rf /var/lib/apt/lists/*
COPY workloads/languages/c /build/languages/c
COPY workloads/languages/vendor /build/languages/vendor
COPY workloads/faults/runtime/tests/language_fixture.c /build/fixture.c
RUN mkdir -p /out/symbols \
    && clang -O1 -g -fsanitize-coverage=trace-pc-guard -c /build/fixture.c -o /build/fixture.o \
    && clang -O1 -g -c /build/languages/c/shim.c -o /build/shim.o \
    && clang /build/fixture.o /build/shim.o -pthread -ldl -Wl,--build-id -o /out/fixture \
    && cp /out/fixture /out/symbols/fixture \
    && nm --defined-only /out/fixture | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > /out/symbols/c.sym.tsv \
    && sha256sum /out/fixture | awk '{print $1 "  /opt/harmony/fixture"}' > /out/symbols/harmony-instrumented-events
FROM ${DEBIAN_IMAGE} AS language-base
COPY --from=compiled /out/fixture /opt/harmony/fixture
COPY --from=compiled /out/symbols /symbols
COPY workloads/languages/vendor/ANTITHESIS-SDK-CPP-LICENSE /licenses/antithesis-sdk-cpp.txt
CMD ["/opt/harmony/fixture"]
