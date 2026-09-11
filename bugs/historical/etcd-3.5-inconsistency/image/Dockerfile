# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
ARG GO_IMAGE=golang:1.24-bookworm@sha256:1a6d4452c65dea36aac2e2d606b01b4a029ec90cc1ae53890540ce6173ea77ac

FROM ${GO_IMAGE} AS workload-writer-build

ARG TARGETARCH

SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

RUN case "${TARGETARCH}" in \
        amd64|arm64) ;; \
        *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac

WORKDIR /build/writer
COPY bugs/historical/etcd-3.5-inconsistency/image/writer/go.mod \
     bugs/historical/etcd-3.5-inconsistency/image/writer/go.sum ./
RUN go mod download
COPY bugs/historical/etcd-3.5-inconsistency/image/writer/main.go ./
RUN mkdir -p /out \
    && CGO_ENABLED=0 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
    go build -mod=readonly -buildvcs=false -o /out/etcd-writer . \
    && test -x /out/etcd-writer

FROM ${GO_IMAGE} AS instrumented-build

ARG TARGETARCH
ARG ETCD_VERSION=3.5.2
ARG ETCD_SOURCE_SHA256=ecb4d2bc76e48ae504d9a00b50bcc906503c4d0324acf9dee79d97d16f7282ad
ARG ANTITHESIS_SDK_VERSION=0.8.0
ARG ETCD_SOURCE_BASE=https://github.com/etcd-io/etcd/archive/refs/tags

SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl gcc libc6-dev make patch \
    && rm -rf /var/lib/apt/lists/*

RUN case "${TARGETARCH}" in \
        amd64|arm64) ;; \
        *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac

WORKDIR /build
RUN curl --fail --silent --show-error --location --retry 3 \
       --output etcd-source.tar.gz \
       "${ETCD_SOURCE_BASE}/v${ETCD_VERSION}.tar.gz" \
    && echo "${ETCD_SOURCE_SHA256}  etcd-source.tar.gz" | sha256sum --check --strict - \
    && mkdir etcd \
    && tar -xf etcd-source.tar.gz -C etcd --strip-components=1 \
    && rm etcd-source.tar.gz

# This is Antithesis's current documented Go instrumentation pass. The wrapper
# rewrites compiler inputs at build time and emits symbol tables automatically.
# ANTITHESIS_SDK_MODULE_DIR supplies its runtime without modifying upstream
# etcd's modules. Keep object-compatibility flags at their Go defaults: the
# wrapper resolves that SDK in a nested Go build, so object-compatibility flags
# such as -trimpath belong in inherited GOFLAGS rather than only on the outer
# go command.
RUN go install "github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-toolexec@v${ANTITHESIS_SDK_VERSION}"

COPY bugs/historical/etcd-3.5-inconsistency/image/patches/antithesis-sdk-go-v0.8.0-linux-arm64.patch \
     /build/antithesis-sdk-go-v0.8.0-linux-arm64.patch

RUN sdk_cache_dir="$(go env GOMODCACHE)/github.com/antithesishq/antithesis-sdk-go@v${ANTITHESIS_SDK_VERSION}" \
    && test -d "${sdk_cache_dir}" \
    && mkdir -p /build/antithesis-sdk-go \
    && cp -a "${sdk_cache_dir}/." /build/antithesis-sdk-go/ \
    && chmod -R u+w /build/antithesis-sdk-go \
    && patch --batch --forward --fuzz=0 -d /build/antithesis-sdk-go -p1 \
       < /build/antithesis-sdk-go-v0.8.0-linux-arm64.patch \
    && grep -F -q 'linux && (amd64 || arm64) && cgo' \
       /build/antithesis-sdk-go/internal/voidstar_handler.go

RUN mkdir -p /out/etcd /out/symbols \
    && sdk_dir=/build/antithesis-sdk-go \
    && test -d "${sdk_dir}" \
    && (cd /build/etcd/server \
        && CGO_ENABLED=1 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           ANTITHESIS_SDK_MODULE_DIR="${sdk_dir}" \
           ANTITHESIS_SYMBOLS_DIR=/out/symbols \
           ANTITHESIS_SYMBOL_PREFIX=etcd-server \
           go build -toolexec=antithesis-go-toolexec -buildvcs=false -o /out/etcd/etcd .) \
    && (cd /build/etcd/etcdctl \
        && CGO_ENABLED=1 GOOS=linux GOARCH="${TARGETARCH}" GOFLAGS=-trimpath \
           ANTITHESIS_SDK_MODULE_DIR="${sdk_dir}" \
           ANTITHESIS_SYMBOLS_DIR=/out/symbols \
           ANTITHESIS_SYMBOL_PREFIX=etcdctl \
           go build -toolexec=antithesis-go-toolexec -buildvcs=false -o /out/etcd/etcdctl .) \
    && test -x /out/etcd/etcd \
    && test -x /out/etcd/etcdctl \
    && set -- /out/symbols/*.sym.tsv \
    && test -s "$1" \
    && grep -a -q antithesishq/antithesis-sdk-go/instrumentation /out/etcd/etcd \
    && sha256sum /out/etcd/etcd \
       | awk '{print $1 "  /opt/etcd/etcd"}' > /out/harmony-instrumented-events \
    && cp /build/etcd/LICENSE /out/ETCD-LICENSE \
    && cp "${sdk_dir}/LICENSE" /out/ANTITHESIS-SDK-GO-LICENSE

COPY consonance/harmony-linux/libvoidstar /build/libvoidstar
RUN make -C /build/libvoidstar BUILD_DIR=/build/libvoidstar-build all \
    && cp /build/libvoidstar-build/libvoidstar.so /out/

FROM ${DEBIAN_IMAGE}

ARG ETCD_VERSION=3.5.2

SHELL ["/bin/sh", "-euxc"]

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=instrumented-build /out/etcd/etcd /opt/etcd/etcd
COPY --from=instrumented-build /out/etcd/etcdctl /opt/etcd/etcdctl
COPY --from=workload-writer-build /out/etcd-writer /opt/harmony/etcd-writer
COPY --from=instrumented-build /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=instrumented-build /out/symbols/*.sym.tsv /symbols/
COPY --from=instrumented-build /out/harmony-instrumented-events /symbols/harmony-instrumented-events
COPY --from=instrumented-build /out/ETCD-LICENSE /licenses/etcd.txt
COPY --from=instrumented-build /out/ANTITHESIS-SDK-GO-LICENSE /licenses/antithesis-sdk-go.txt

RUN chmod 0755 /opt/etcd/etcd /opt/etcd/etcdctl /opt/harmony/etcd-writer \
        /usr/lib/libvoidstar.so \
    && test -x /opt/etcd/etcd \
    && test -x /opt/etcd/etcdctl \
    && test -x /opt/harmony/etcd-writer \
    && set -- /symbols/*.sym.tsv \
    && test -s "$1" \
    && test -s /symbols/harmony-instrumented-events \
    && test -s /licenses/etcd.txt \
    && test -s /licenses/antithesis-sdk-go.txt

LABEL org.opencontainers.image.title="harmony etcd consistent-index inconsistency workload"
LABEL org.opencontainers.image.version="${ETCD_VERSION}"
LABEL org.opencontainers.image.description="upstream etcd source built through the Antithesis Go instrumentation pipeline"

COPY bugs/historical/etcd-3.5-inconsistency/image/bundle /etc/harmony/bundle
COPY bugs/historical/etcd-3.5-inconsistency/image/setup.sh \
     bugs/historical/etcd-3.5-inconsistency/image/node.sh \
     bugs/historical/etcd-3.5-inconsistency/image/ready.sh \
     bugs/historical/etcd-3.5-inconsistency/image/hooks.sh /opt/harmony/

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh \
        /opt/harmony/ready.sh /opt/harmony/hooks.sh

CMD ["/opt/harmony/node.sh"]
