# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=docker.io/library/debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
FROM ${DEBIAN_IMAGE}
RUN apt-get update \
    && apt-get install --yes --no-install-recommends gcc libc6-dev make binutils \
    && rm -rf /var/lib/apt/lists/*
COPY consonance/harmony-linux/libvoidstar /build/libvoidstar
COPY workloads/faults/runtime /build/fault-runtime
RUN make -C /build/fault-runtime CORE_DIR=/build/libvoidstar BUILD_DIR=/out all \
    && /build/libvoidstar/check-symbols.sh /out/libvoidstar.so \
    && gcc -O2 -Wall -Wextra -Werror -Wconversion -Wpedantic \
        /build/fault-runtime/tests/language_park_launcher.c -o /out/park-launcher \
    && gcc -O1 -g -fsanitize-coverage=trace-pc \
        /build/fault-runtime/tests/language_fixture.c -L/out -lvoidstar -pthread \
        -Wl,-rpath,/usr/lib -Wl,--build-id -o /out/gcc-fixture \
    && nm --defined-only /out/gcc-fixture | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > /out/gcc.sym.tsv \
    && sha256sum /out/gcc-fixture | awk '{print $1 "  /opt/harmony/gcc-fixture"}' > /out/gcc-attestation
