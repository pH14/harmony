# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
ARG GO_IMAGE=golang:1.27-bookworm@sha256:5cf287a799e6b94384bad13d16b14904c531f51ba65792237e122ce42b392f61

ARG HARMONY_RUNTIME_IMAGE=harmony-language-runtime:local
FROM ${GO_IMAGE} AS harness-build

ARG TARGETARCH
ENV GOTOOLCHAIN=local

SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

RUN case "${TARGETARCH}" in \
        amd64|arm64) ;; \
        *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac

WORKDIR /build
COPY workloads/harness/kit/go.mod workloads/harness/kit/go.sum kit/
COPY workloads/harness/targets/dqlite/go.mod workloads/harness/targets/dqlite/go.sum targets/dqlite/
RUN (cd kit && go mod download) && (cd targets/dqlite && go mod download)
COPY workloads/harness/kit kit
COPY workloads/harness/targets/dqlite/main.go targets/dqlite/
RUN mkdir -p /out \
    && (cd kit && go test -mod=readonly ./...) \
    && (cd targets/dqlite && go vet -mod=readonly ./... \
        && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/dqlite-harness .) \
    && (cd kit && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/harness-net ./cmd/harness-net) \
    && test -x /out/dqlite-harness

FROM ${DEBIAN_IMAGE} AS instrumented-build

ARG DQLITE_COMMIT=1f49444df95301957dc3a396b4557a84e4d0f547
ARG RAFT_COMMIT=63c8aa5b45684f5f3ba23283e924c6c7a7f3e40c
ARG LIBUV_COMMIT=0c1fa696aa502eb749c2c4735005f41ba00a27b8
ARG DQLITE_REPO=https://github.com/canonical/dqlite.git
ARG RAFT_REPO=https://github.com/canonical/raft.git
ARG LIBUV_REPO=https://github.com/libuv/libuv.git

SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        autoconf automake binutils ca-certificates clang git libc6-dev libsqlite3-dev libtool make pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
RUN fetch() { mkdir -p "$1" && git -C "$1" init -q && git -C "$1" fetch -q --depth 1 "$2" "$3" \
        && git -C "$1" checkout -q FETCH_HEAD && test "$(git -C "$1" rev-parse HEAD)" = "$3"; } \
    && fetch dqlite "${DQLITE_REPO}" "${DQLITE_COMMIT}" \
    && fetch raft "${RAFT_REPO}" "${RAFT_COMMIT}" \
    && fetch libuv "${LIBUV_REPO}" "${LIBUV_COMMIT}" \
    && for tree in dqlite raft libuv; do echo "${tree} $(git -C "${tree}" rev-parse HEAD)"; done > /build/sources

COPY workloads/languages/c/shim.c /build/languages/c/shim.c
COPY workloads/languages/vendor /build/languages/vendor
COPY workloads/harness/targets/dqlite/node/dqlite-node.c /build/node.c

# Configure probes link plain test programs, so coverage flags apply only to
# the make step.
ENV CC=clang PKG_CONFIG_PATH=/build/deps/lib/pkgconfig
RUN cover="-O1 -g -fsanitize-coverage=trace-pc-guard" \
    && clang -O1 -g -c /build/languages/c/shim.c -o /build/shim.o \
    && (cd libuv && sh autogen.sh \
        && ./configure --disable-shared --prefix=/build/deps CFLAGS="-O1 -g" \
        && make -j"$(nproc)" CFLAGS="${cover}" && make install) \
    && (cd raft && autoreconf -i \
        && ./configure --disable-shared --disable-lz4 --disable-fixture --prefix=/build/deps CFLAGS="-O1 -g" \
        && make -j"$(nproc)" CFLAGS="${cover}" && make install) \
    && (cd dqlite && autoreconf -i \
        && ./configure --disable-shared --prefix=/build/deps CFLAGS="-O1 -g" \
        && make -j"$(nproc)" CFLAGS="${cover}" && make install) \
    && clang ${cover} -I/build/deps/include -c /build/node.c -o /build/node.o \
    && clang -no-pie -Wl,--build-id -o /build/dqlite-node /build/node.o /build/shim.o \
        /build/deps/lib/libdqlite.a /build/deps/lib/libraft.a /build/deps/lib/libuv.a \
        "$(pkg-config --variable=libdir sqlite3)/libsqlite3.a" -lpthread -ldl -lm \
    && nm /build/dqlite-node | grep -c antithesis_load_libvoidstar \
    && nm /build/dqlite-node | grep -c ' T __sanitizer_cov_trace_pc_guard$' \
    && nm /build/dqlite-node | grep -c ' [tT] raft_apply$'

RUN mkdir -p /out/dqlite /out/symbols \
    && cp /build/dqlite-node /out/dqlite/ \
    && cp /build/dqlite-node /out/symbols/ \
    && nm --defined-only /out/dqlite/dqlite-node | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > /out/symbols/dqlite-node.sym.tsv \
    && test -s /out/symbols/dqlite-node.sym.tsv \
    && sha256sum /out/dqlite/dqlite-node | awk '{print $1 "  /opt/dqlite/dqlite-node"}' > /out/symbols/harmony-instrumented-events \
    && cp /build/sources /out/symbols/sources \
    && cp dqlite/LICENSE /out/DQLITE-LICENSE \
    && cp raft/LICENSE /out/RAFT-LICENSE \
    && cp libuv/LICENSE /out/LIBUV-LICENSE

FROM ${HARMONY_RUNTIME_IMAGE} AS voidstar

FROM ${DEBIAN_IMAGE}
ARG RAFT_COMMIT=63c8aa5b45684f5f3ba23283e924c6c7a7f3e40c
SHELL ["/bin/sh", "-euxc"]
# Image paths must stay distinct on case-insensitive hosts. The harness uses
# none of the iptables extensions whose names differ only in case. OpenSSL
# arrives only through Kerberos, which ip, tc and iptables never load, and its
# random-number instructions are outside the reviewed set.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends iproute2 iptables \
    && rm -rf /var/lib/apt/lists/* \
    && dpkg --purge --force-depends libssl3 \
    && ip -V && tc -V && iptables -V \
    && for dir in /usr/lib/*/xtables; do \
        ls "$dir" | tr A-Z a-z | sort | uniq -d | while read -r name; do \
            find "$dir" -maxdepth 1 -iname "$name" -delete; \
        done; \
    done
COPY workloads/languages/reviewed/bookworm-x86_64.txt /tmp/harmony-base-review
RUN if [ "$(uname -m)" = x86_64 ]; then \
        mkdir -p /etc/harmony && cp /tmp/harmony-base-review /etc/harmony/instruction-allowlist; \
    fi && rm /tmp/harmony-base-review

COPY --from=instrumented-build /out/dqlite /opt/dqlite
COPY --from=harness-build /out/dqlite-harness /out/harness-net /opt/harmony/
COPY --from=voidstar /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=instrumented-build /out/symbols /symbols
COPY --from=instrumented-build /out/DQLITE-LICENSE /licenses/dqlite.txt
COPY --from=instrumented-build /out/RAFT-LICENSE /licenses/raft.txt
COPY --from=instrumented-build /out/LIBUV-LICENSE /licenses/libuv.txt
COPY workloads/languages/vendor/ANTITHESIS-SDK-CPP-LICENSE /licenses/antithesis-sdk-cpp.txt

RUN chmod 0755 /opt/dqlite/dqlite-node /opt/harmony/dqlite-harness /opt/harmony/harness-net /usr/lib/libvoidstar.so \
    && test -s /symbols/dqlite-node.sym.tsv \
    && test "$(wc -l < /symbols/harmony-instrumented-events)" -eq 1

LABEL org.opencontainers.image.title="harmony dqlite harness workload"
LABEL org.opencontainers.image.version="${RAFT_COMMIT}"
LABEL org.opencontainers.image.description="upstream dqlite, canonical/raft and libuv built with clang trace-pc-guard coverage, driven by the Harmony harness kit"

COPY workloads/harness/targets/dqlite/bundle /etc/harmony/bundle
COPY workloads/harness/targets/dqlite/setup.sh workloads/harness/targets/dqlite/node.sh /opt/harmony/

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh

CMD ["/opt/harmony/node.sh", "1"]
