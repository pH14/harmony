# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=docker.io/library/debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
ARG TARGETARCH

FROM ${DEBIAN_IMAGE} AS harness-amd64
RUN mkdir -p /out/harness/musl /out/harness/root

FROM ${DEBIAN_IMAGE} AS harness-arm64
ARG MUSL_URL=https://musl.libc.org/releases/musl-1.2.6.tar.gz
ARG MUSL_SHA256=d585fd3b613c66151fc3249e8ed44f77020cb5e6c1e635a616d3f9f82460512a
ARG BUSYBOX_URL=https://busybox.net/downloads/busybox-1.38.0.tar.bz2
ARG BUSYBOX_SHA256=34f9ea6ff8636f2c9241153b9114eefa9e65674a45318ae1ef95bb5f31c53bb2
ARG IPROUTE2_URL=https://cdn.kernel.org/pub/linux/utils/net/iproute2/iproute2-6.18.0.tar.xz
ARG IPROUTE2_SHA256=6ba520e1975e4c50dc931eeae91ea37c198b8a173744885f8895b84325f9d456
ARG IPTABLES_URL=https://www.netfilter.org/projects/iptables/files/iptables-1.8.11.tar.xz
ARG IPTABLES_SHA256=d87303d55ef8c92bcad4dd3f978b26d272013642b029425775f5bad1009fe7b2
RUN apt-get update \
    && apt-get install --yes --no-install-recommends bison bzip2 ca-certificates curl flex gcc \
        libc6-dev linux-libc-dev make binutils patch pkg-config xz-utils \
    && rm -rf /var/lib/apt/lists/*
COPY consonance/harmony-linux/libvoidstar /build/libvoidstar
COPY workloads/faults/runtime /build/fault-runtime
COPY consonance/harmony-linux/linux/patches/musl /build/harness-image/musl
COPY workloads/harness/image/build-arm64-root.sh /build/harness-image/build-arm64-root.sh
RUN MUSL_URL="${MUSL_URL}" MUSL_SHA256="${MUSL_SHA256}" \
    BUSYBOX_URL="${BUSYBOX_URL}" BUSYBOX_SHA256="${BUSYBOX_SHA256}" \
    IPROUTE2_URL="${IPROUTE2_URL}" IPROUTE2_SHA256="${IPROUTE2_SHA256}" \
    IPTABLES_URL="${IPTABLES_URL}" IPTABLES_SHA256="${IPTABLES_SHA256}" \
    sh /build/harness-image/build-arm64-root.sh \
    && cp -a /opt/harmony-musl /out/harness/musl

FROM harness-${TARGETARCH} AS harness

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
COPY --from=harness /out/harness /out/harness
