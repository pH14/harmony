# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
ARG GO_IMAGE=golang:1.26-bookworm@sha256:d9c68c2c51161e12fd77e4c6320687c9cd86e1af1e3ad6e6cd63ff970641453c

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
COPY workloads/harness/targets/nats-kv/go.mod workloads/harness/targets/nats-kv/go.sum targets/nats-kv/
RUN (cd kit && go mod download) && (cd targets/nats-kv && go mod download)
COPY workloads/harness/kit kit
COPY workloads/harness/targets/nats-kv/main.go targets/nats-kv/
RUN mkdir -p /out \
    && (cd kit && go test -mod=readonly ./...) \
    && (cd targets/nats-kv && go vet -mod=readonly ./... \
        && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/nats-kv-harness .) \
    && (cd kit && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           go build -mod=readonly -buildvcs=false -o /out/harness-net ./cmd/harness-net) \
    && ! grep -a -q antithesishq/antithesis-sdk-go/instrumentation /out/nats-kv-harness \
    && test -x /out/nats-kv-harness

FROM ${GO_IMAGE} AS instrumented-build

ARG TARGETARCH
ARG NATS_VERSION=v2.10.20
ARG NATS_SOURCE_SHA256=
ARG ANTITHESIS_SDK_VERSION=0.8.0
ARG NATS_SOURCE_BASE=https://github.com/nats-io/nats-server/archive/refs/tags
ENV CGO_ENABLED=1 GOFLAGS=-trimpath GOTOOLCHAIN=local

SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl gcc libc6-dev make patch \
    && rm -rf /var/lib/apt/lists/*

RUN case "${TARGETARCH}" in \
        amd64|arm64) ;; \
        *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac

WORKDIR /build
RUN sha256="${NATS_SOURCE_SHA256}" \
    && if [ -z "${sha256}" ]; then \
        case "${NATS_VERSION}" in \
            v2.10.20) sha256=598704e96ea92fd74283526c9b6044df8b54e4cdccf9ddc5e25a52249c44424a ;; \
            v2.10.23) sha256=60b9bbdb84661e9fe1c3834ab1fb421ed74453dd35dc206c04d3272f85b93f28 ;; \
            v2.15.1) sha256=41b25fcb78a174f3afce4ec1eca0c1035439abf580f0a0569dbb03bd0d955e08 ;; \
            *) echo "pass NATS_SOURCE_SHA256 for ${NATS_VERSION}" >&2; exit 1 ;; \
        esac; \
    fi \
    && curl --fail --silent --show-error --location --retry 3 \
       --output nats-source.tar.gz \
       "${NATS_SOURCE_BASE}/${NATS_VERSION}.tar.gz" \
    && echo "${sha256}  nats-source.tar.gz" | sha256sum --check --strict - \
    && mkdir nats \
    && tar -xf nats-source.tar.gz -C nats --strip-components=1 \
    && rm nats-source.tar.gz

# NATS 2.11 and later require the SDK's no-op module. The toolexec emits calls
# for its own SDK version, so the module must be that version too.
RUN cd nats \
    && if grep -q 'github.com/antithesishq/antithesis-sdk-go ' go.mod; then \
        go get "github.com/antithesishq/antithesis-sdk-go@v${ANTITHESIS_SDK_VERSION}-default-no-op"; \
    fi

RUN go install "github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-toolexec@v${ANTITHESIS_SDK_VERSION}"

COPY workloads/bugs/historical/etcd-3.5-inconsistency/image/patches/antithesis-sdk-go-v0.8.0-linux-arm64.patch \
     /build/antithesis-sdk-go-v0.8.0-linux-arm64.patch
COPY workloads/languages/go/antithesis-go-toolexec-v0.8.0-stdlib.patch /build/stdlib.patch
COPY workloads/languages/go/configure-stdlib.sh /build/configure-stdlib.sh

RUN sdk_cache_dir="$(go env GOMODCACHE)/github.com/antithesishq/antithesis-sdk-go@v${ANTITHESIS_SDK_VERSION}" \
    && test -d "${sdk_cache_dir}" \
    && mkdir -p /build/antithesis-sdk-go \
    && cp -a "${sdk_cache_dir}/." /build/antithesis-sdk-go/ \
    && chmod -R u+w /build/antithesis-sdk-go \
    && patch --batch --forward --fuzz=0 -d /build/antithesis-sdk-go -p1 \
       < /build/antithesis-sdk-go-v0.8.0-linux-arm64.patch \
    && patch --batch --forward --fuzz=0 -d /build/antithesis-sdk-go -p1 < /build/stdlib.patch \
    && (cd /build/antithesis-sdk-go \
        && gofmt -w tools/antithesis-go-toolexec/config.go tools/antithesis-go-toolexec/policy.go tools/antithesis-go-toolexec/harmony_std_test.go \
        && go test ./tools/antithesis-go-toolexec && go install ./tools/antithesis-go-toolexec) \
    && grep -F -q 'linux && (amd64 || arm64) && cgo' \
       /build/antithesis-sdk-go/internal/voidstar_handler.go

RUN mkdir -p /out/nats /out/symbols /out/stdlib \
    && sdk_dir=/build/antithesis-sdk-go \
    && (cd /build/nats \
        && CGO_ENABLED=1 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           bash /build/configure-stdlib.sh "${sdk_dir}" github.com/nats-io/nats-server/v2 /out/stdlib > /out/stdlib/instrumentation.env \
        && source /out/stdlib/instrumentation.env \
        && CGO_ENABLED=1 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           ANTITHESIS_SDK_MODULE_DIR="${sdk_dir}" \
           ANTITHESIS_SYMBOLS_DIR=/out/symbols \
           ANTITHESIS_SYMBOL_PREFIX=nats-server \
           go build -toolexec=antithesis-go-toolexec -buildvcs=false -o /out/nats/nats-server .) \
    && test -x /out/nats/nats-server \
    && set -- /out/symbols/*.sym.tsv \
    && test -s "$1" \
    && grep -a -q antithesishq/antithesis-sdk-go/instrumentation /out/nats/nats-server \
    && sha256sum /out/nats/nats-server \
       | awk '{print $1 "  /opt/nats/nats-server"}' > /out/harmony-instrumented-events \
    && cp /build/nats/LICENSE /out/NATS-LICENSE \
    && cp "${sdk_dir}/LICENSE" /out/ANTITHESIS-SDK-GO-LICENSE

FROM ${HARMONY_RUNTIME_IMAGE} AS voidstar

FROM ${DEBIAN_IMAGE}
ARG NATS_VERSION=v2.10.20
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

COPY --from=instrumented-build /out/nats/nats-server /opt/nats/nats-server
COPY --from=harness-build /out/nats-kv-harness /out/harness-net /opt/harmony/
COPY --from=voidstar /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=instrumented-build /out/symbols/*.sym.tsv /symbols/
COPY --from=instrumented-build /out/stdlib /symbols/stdlib
COPY --from=instrumented-build /out/harmony-instrumented-events /symbols/harmony-instrumented-events
COPY --from=instrumented-build /out/NATS-LICENSE /licenses/nats-server.txt
COPY --from=instrumented-build /out/ANTITHESIS-SDK-GO-LICENSE /licenses/antithesis-sdk-go.txt

RUN chmod 0755 /opt/nats/nats-server /opt/harmony/nats-kv-harness /opt/harmony/harness-net /usr/lib/libvoidstar.so \
    && test -x /opt/nats/nats-server \
    && test -x /opt/harmony/nats-kv-harness \
    && set -- /symbols/*.sym.tsv \
    && test -s "$1" \
    && test -s /symbols/harmony-instrumented-events \
    && test -s /licenses/nats-server.txt \
    && test -s /licenses/antithesis-sdk-go.txt

LABEL org.opencontainers.image.title="harmony NATS JetStream key-value harness workload"
LABEL org.opencontainers.image.version="${NATS_VERSION}"
LABEL org.opencontainers.image.description="upstream nats-server source built through the Antithesis Go instrumentation pipeline, driven by the Harmony harness kit"

COPY workloads/harness/targets/nats-kv/bundle /etc/harmony/bundle
COPY workloads/harness/targets/nats/setup.sh workloads/harness/targets/nats/node.sh /opt/harmony/

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh

# NATS_SYNC=always (the default) makes JetStream fsync every write
# (`sync_interval: always`). NATS_SYNC=default keeps the server's periodic sync.
ARG NATS_SYNC=always
RUN case "${NATS_SYNC}" in \
        default) ;; \
        always) printf 'jetstream {\n  sync_interval: always\n}\n' > /opt/harmony/nats.conf ;; \
        *) echo "unsupported NATS_SYNC: ${NATS_SYNC}" >&2; exit 1 ;; \
    esac \
    && command -v mkfs.ext4

CMD ["/opt/harmony/node.sh", "n1", "1"]
