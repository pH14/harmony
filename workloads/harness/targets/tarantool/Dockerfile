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
COPY workloads/harness/targets/tarantool/go.mod workloads/harness/targets/tarantool/go.sum targets/tarantool/
RUN (cd kit && go mod download) && (cd targets/tarantool && go mod download)
COPY workloads/harness/kit kit
COPY workloads/harness/targets/tarantool/main.go targets/tarantool/
RUN mkdir -p /out \
    && (cd kit && go test -mod=readonly ./...) \
    && (cd targets/tarantool && go vet -mod=readonly ./... \
        && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/tarantool-harness .) \
    && (cd kit && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/harness-net ./cmd/harness-net) \
    && test -x /out/tarantool-harness

FROM ${DEBIAN_IMAGE} AS instrumented-build

ARG TARANTOOL_VERSION=3.8.1
ARG TARANTOOL_COMMIT=e643f88a5e0c6d9ef2789b2033088fd04e7d39ae
ARG TARANTOOL_REPO=https://github.com/tarantool/tarantool.git

SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

# Tarantool needs clang 19, which Bookworm ships in backports.
RUN echo "deb http://deb.debian.org/debian bookworm-backports main" > /etc/apt/sources.list.d/backports.list \
    && apt-get update \
    && apt-get install --yes --no-install-recommends \
        autoconf automake binutils ca-certificates cmake git libc6-dev libicu-dev \
        libncurses-dev libreadline-dev libssl-dev libtool make pkg-config python3 zlib1g-dev \
    && apt-get install --yes --no-install-recommends -t bookworm-backports clang-19 libclang-rt-19-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
# The tag's tree records the commit of every submodule.
RUN git clone -q --depth 1 --branch "${TARANTOOL_VERSION}" --recurse-submodules --shallow-submodules \
        "${TARANTOOL_REPO}" tarantool \
    && test "$(git -C tarantool rev-parse HEAD)" = "${TARANTOOL_COMMIT}" \
    && git -C tarantool/third_party/luajit fetch -q --unshallow --tags \
    && (echo "tarantool $(git -C tarantool rev-parse HEAD)"; git -C tarantool submodule status --recursive) > /build/sources

COPY workloads/languages/c/shim.c /build/languages/c/shim.c
COPY workloads/languages/vendor /build/languages/vendor

ENV CC=clang-19 CXX=clang++-19
RUN clang-19 -O1 -g -fPIC -c /build/languages/c/shim.c -o /build/shim.o \
    && cmake -S tarantool -B tarantool/build \
        -DCMAKE_BUILD_TYPE=RelWithDebInfo \
        -DCMAKE_C_FLAGS="-fsanitize-coverage=trace-pc-guard" \
        -DCMAKE_CXX_FLAGS="-fsanitize-coverage=trace-pc-guard" \
        -DCMAKE_EXE_LINKER_FLAGS="-fno-sanitize-link-runtime /build/shim.o -ldl -Wl,--build-id" \
        -DCMAKE_SHARED_LINKER_FLAGS="-fno-sanitize-link-runtime" \
        -DCMAKE_MODULE_LINKER_FLAGS="-fno-sanitize-link-runtime" \
        -DTARANTOOL_VERSION="${TARANTOOL_VERSION}-0-g${TARANTOOL_COMMIT}" \
        -DENABLE_DIST=OFF \
        -DENABLE_FEEDBACK_DAEMON=OFF \
    && cmake --build tarantool/build --target tarantool -j"$(nproc)" \
    && nm tarantool/build/src/tarantool | grep -c antithesis_load_libvoidstar \
    && nm tarantool/build/src/tarantool | grep -c ' T __sanitizer_cov_trace_pc_guard$'

RUN mkdir -p /out/tarantool /out/symbols \
    && cp tarantool/build/src/tarantool /out/tarantool/tarantool \
    && cp /out/tarantool/tarantool /out/symbols/tarantool \
    && nm --defined-only /out/tarantool/tarantool | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > /out/symbols/tarantool.sym.tsv \
    && test -s /out/symbols/tarantool.sym.tsv \
    && sha256sum /out/tarantool/tarantool | awk '{print $1 "  /opt/tarantool/tarantool"}' > /out/symbols/harmony-instrumented-events \
    && cp /build/sources /out/symbols/sources \
    && cp tarantool/LICENSE /out/TARANTOOL-LICENSE

FROM ${HARMONY_RUNTIME_IMAGE} AS voidstar

FROM ${DEBIAN_IMAGE}
ARG TARANTOOL_VERSION=3.8.1
SHELL ["/bin/sh", "-euxc"]
# Image paths must stay distinct on case-insensitive hosts. The harness uses
# none of the iptables extensions whose names differ only in case. Tarantool
# links OpenSSL, so libcrypto's entropy sites carry reviews in
# tarantool-x86_64.txt. Tarantool never loads the legacy provider, which has
# its own unreviewed copies.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends iproute2 iptables libicu72 libncurses6 libreadline8 libssl3 zlib1g \
    && rm -rf /var/lib/apt/lists/* \
    && rm /usr/lib/*/ossl-modules/legacy.so \
    && ip -V && tc -V && iptables -V \
    && for dir in /usr/lib/*/xtables; do \
        ls "$dir" | tr A-Z a-z | sort | uniq -d | while read -r name; do \
            find "$dir" -maxdepth 1 -iname "$name" -delete; \
        done; \
    done
COPY workloads/languages/reviewed/bookworm-x86_64.txt workloads/harness/targets/tarantool/tarantool-x86_64.txt /tmp/
RUN if [ "$(uname -m)" = x86_64 ]; then \
        mkdir -p /etc/harmony && cat /tmp/bookworm-x86_64.txt /tmp/tarantool-x86_64.txt > /etc/harmony/instruction-allowlist; \
    fi && rm /tmp/bookworm-x86_64.txt /tmp/tarantool-x86_64.txt

COPY --from=instrumented-build /out/tarantool /opt/tarantool
COPY --from=harness-build /out/tarantool-harness /out/harness-net /opt/harmony/
COPY --from=voidstar /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=instrumented-build /out/symbols /symbols
COPY --from=instrumented-build /out/TARANTOOL-LICENSE /licenses/tarantool.txt
COPY workloads/languages/vendor/ANTITHESIS-SDK-CPP-LICENSE /licenses/antithesis-sdk-cpp.txt

RUN chmod 0755 /opt/tarantool/tarantool /opt/harmony/tarantool-harness /opt/harmony/harness-net /usr/lib/libvoidstar.so \
    && ! ldd /opt/tarantool/tarantool | grep -q "not found" \
    && /opt/tarantool/tarantool --version \
    && test -s /symbols/tarantool.sym.tsv \
    && test "$(wc -l < /symbols/harmony-instrumented-events)" -eq 1

LABEL org.opencontainers.image.title="harmony Tarantool harness workload"
LABEL org.opencontainers.image.version="${TARANTOOL_VERSION}"
LABEL org.opencontainers.image.description="upstream Tarantool built with clang trace-pc-guard coverage, driven by the Harmony harness kit"

COPY workloads/harness/targets/tarantool/bundle /etc/harmony/bundle
COPY workloads/harness/targets/tarantool/setup.sh workloads/harness/targets/tarantool/node.sh /opt/harmony/
COPY workloads/harness/targets/tarantool/config.yaml workloads/harness/targets/tarantool/harness.lua /opt/tarantool/

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh

CMD ["/opt/harmony/node.sh", "1"]
